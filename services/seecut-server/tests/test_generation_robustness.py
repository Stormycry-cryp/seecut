from __future__ import annotations

import base64
import io
import json
import os
import tempfile
import threading
import unittest
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from seecut_server.config import Config
from seecut_server.db import Database
from seecut_server.errors import ApiError
from seecut_server.image2 import Image2Error
from seecut_server.service import SeeCutService
from seecut_server.xiangxin import XiangxinError


PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
)


class Clock:
    def __init__(self, value: int = 1_700_000_000):
        self.value = value

    def __call__(self):
        return self.value


class FakeImage2:
    configured = True

    def __init__(self, content: bytes = PNG):
        self.content = content
        self.calls = 0
        self.edit_calls = []
        self.error: Image2Error | None = None

    def create(self, body):
        self.calls += 1
        if self.error:
            raise self.error
        return [self.content]

    def edit(self, body, images):
        self.edit_calls.append((body, images))
        return [self.content]


class FakeXiangxin:
    configured = True

    def __init__(self):
        self.submit_calls = 0
        self.asset_calls = []
        self.poll_error: XiangxinError | None = None
        self.poll_response: dict | None = None
        self.nested_response = False
        self.nested_asset_response = False

    def create_video(self, body):
        self.submit_calls += 1
        if self.nested_response:
            return {"data": {"task_id": "video-upstream", "status": "queued"}}
        return {"id": "video-upstream", "status": "queued"}

    def get_video(self, task_id):
        if self.poll_error:
            raise self.poll_error
        if self.poll_response is not None:
            return self.poll_response
        if self.nested_response:
            return {"data": {"id": task_id, "status": "processing"}}
        return {"id": task_id, "status": "processing"}

    def register_asset(self, source_url, asset_type):
        self.asset_calls.append((source_url, asset_type))
        if self.nested_asset_response:
            return {"data": {"assetId": "provider-asset"}}
        return {"assetId": "provider-asset"}


class FakeEmailSender:
    configured = True

    def send_token(self, email, purpose, token):
        pass


class GenerationRobustnessTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        prices = {
            "image:gpt-image-2.5-flare": 7,
            "video:sd_2.0_mini_special": 12,
        }
        with patch.dict(
            os.environ,
            {
                "SECUT_ENV": "test",
                "SECUT_DATABASE_PATH": str(root / "database.sqlite3"),
                "SECUT_STORAGE_PATH": str(root / "storage"),
                "SECUT_SIGNING_SECRET": "test-signing-secret-that-is-long-enough",
                "SECUT_EXPOSE_TEST_TOKENS": "true",
                "SECUT_UPLOAD_TTL_SECONDS": "30",
                "SECUT_GENERATION_OUTPUT_TTL_SECONDS": "60",
                "SECUT_PROVIDER_TIMEOUT_SECONDS": "10",
                "SECUT_MODEL_PRICES_JSON": __import__("json").dumps(prices),
            },
            clear=True,
        ):
            config = Config.from_env()
        database = Database(config.database_path, root.parent / "missing")
        database.schema_path = Path(__file__).resolve().parents[1] / "schema.sql"
        database.initialize()
        self.clock = Clock()
        self.image2 = FakeImage2()
        self.xiangxin = FakeXiangxin()
        self.service = SeeCutService(
            config,
            database,
            emailer=FakeEmailSender(),
            image2=self.image2,
            xiangxin=self.xiangxin,
            clock=self.clock,
        )
        registration = self.service.register("worker@example.test", "password-long-enough")
        self.service.verify_email("worker@example.test", registration["verification_token"])
        self.user_id = self.service.login("worker@example.test", "password-long-enough")["user"]["id"]
        with database.transaction() as connection:
            connection.execute(
                "UPDATE wallets SET available_credits=100 WHERE user_id=?", (self.user_id,)
            )

    def tearDown(self):
        self.directory.cleanup()

    def image_task(self, key="image-key"):
        request = {"model": "gpt-image-2.5-flare", "prompt": "clean product"}
        quote = self.service.quote_generation(self.user_id, "image", request)
        return self.service.create_generation_task(
            self.user_id, "image", request | {"quote_id": quote["quote_id"]}, key
        )

    def video_task(self, key="video-key", reference_asset_ids=None):
        request = {
            "model": "sd_2.0_mini_special",
            "prompt": "slow camera move",
            "resolution": "720p",
            "duration": 5,
            "aspect_ratio": "16:9",
        }
        if reference_asset_ids is not None:
            request["reference_asset_ids"] = reference_asset_ids
        quote = self.service.quote_generation(self.user_id, "video", request)
        return self.service.create_generation_task(
            self.user_id, "video", request | {"quote_id": quote["quote_id"]}, key
        )

    def generation_asset(self, filename: str, content: bytes = PNG):
        upload = self.service.prepare_upload(
            self.user_id, "generation_input", filename, "image/png", len(content)
        )
        self.service.save_upload_stream(
            upload["upload_id"], io.BytesIO(content), len(content), "image/png"
        )
        return self.service.register_generation_asset(self.user_id, upload["upload_id"])

    def synthetic_generation_asset(
        self, filename: str, content_type: str, media_kind: str, duration_ms: int
    ):
        content = f"synthetic-{filename}".encode()
        upload = self.service.prepare_upload(
            self.user_id, "generation_input", filename, content_type, len(content)
        )
        with patch.object(
            self.service,
            "_probe_generation_input_file",
            return_value=(media_kind, duration_ms),
        ):
            self.service.save_upload_stream(
                upload["upload_id"], io.BytesIO(content), len(content), content_type
            )
        return self.service.register_generation_asset(self.user_id, upload["upload_id"])

    def test_expired_submitting_becomes_reconcile_without_second_post(self):
        task = self.video_task()
        claimed = self.service.claim_generation_tasks()[0]
        self.service._set_task_status(task["id"], "submitting", keep_lease=True)
        self.clock.value += 181
        self.assertEqual(self.service.claim_generation_tasks(), [])
        recovered = self.service.get_generation_task(self.user_id, task["id"])
        self.assertEqual(recovered["status"], "pending_reconcile")
        self.assertEqual(self.xiangxin.submit_calls, 0)
        wallet = self.service.wallet(self.user_id)
        self.assertEqual(wallet["held_credits"], 12)

    def test_nested_xiangxin_task_response_is_normalized(self):
        self.xiangxin.nested_response = True
        task = self.video_task()
        claimed = self.service.claim_generation_tasks()[0]
        self.service.process_generation_task(claimed)
        current = self.service.get_generation_task(self.user_id, task["id"])
        self.assertEqual(current["status"], "provider_accepted")
        self.assertEqual(current["upstream_task_id"], "video-upstream")
        self.clock.value += 11
        claimed = self.service.claim_generation_tasks()[0]
        self.service.process_generation_task(claimed)
        self.assertEqual(self.service.get_generation_task(self.user_id, task["id"])["status"], "processing")

    def test_nested_xiangxin_asset_response_is_normalized(self):
        upload = self.service.prepare_upload(
            self.user_id, "generation_input", "reference.png", "image/png", len(PNG)
        )
        self.service.save_upload_stream(upload["upload_id"], io.BytesIO(PNG), len(PNG), "image/png")
        asset = self.service.register_generation_asset(self.user_id, upload["upload_id"])
        self.xiangxin.nested_asset_response = True
        task = self.video_task(reference_asset_ids=[asset["id"]])
        with self.service.db.connect() as connection:
            task_row = connection.execute("SELECT * FROM generation_tasks WHERE id=?", (task["id"],)).fetchone()
        payload = self.service._task_payload(task_row)
        self.assertEqual(payload["reference_images"], ["assetId://provider-asset"])
        self.assertNotIn("first_image", payload)
        self.assertNotIn("last_image", payload)
        self.assertNotIn("reference_videos", payload)
        self.assertNotIn("reference_audios", payload)

    def test_multimodal_video_payload_groups_reference_media(self):
        image = self.generation_asset("reference.png")
        video = self.synthetic_generation_asset(
            "reference.mov", "video/quicktime", "video", 4_000
        )
        audio = self.synthetic_generation_asset(
            "reference.wav", "audio/wav", "audio", 3_000
        )
        request = {
            "model": "sd_2.0_mini_special",
            "prompt": "use every reference",
            "resolution": "720p",
            "duration": 4,
            "aspect_ratio": "adaptive",
            "generate_audio": False,
            "reference_asset_ids": [image["id"], video["id"], audio["id"]],
        }
        quote = self.service.quote_generation(self.user_id, "video", request)
        self.assertTrue(quote["billing_key"].endswith("reference_count=1"))
        task = self.service.create_generation_task(
            self.user_id,
            "video",
            request | {"quote_id": quote["quote_id"]},
            "mixed-reference-video",
        )
        with self.service.db.connect() as connection:
            task_row = connection.execute(
                "SELECT * FROM generation_tasks WHERE id=?", (task["id"],)
            ).fetchone()

        payload = self.service._task_payload(task_row)

        self.assertEqual(payload["reference_images"], ["assetId://provider-asset"])
        self.assertEqual(payload["reference_videos"], ["assetId://provider-asset"])
        self.assertEqual(payload["reference_audios"], ["assetId://provider-asset"])
        self.assertIs(payload["generate_audio"], False)
        self.assertNotIn("first_image", payload)
        self.assertNotIn("last_image", payload)
        self.assertEqual(
            [asset_type for _, asset_type in self.xiangxin.asset_calls],
            ["Image", "Video", "Audio"],
        )

    def test_video_reference_semantic_limits_are_enforced(self):
        audio = self.synthetic_generation_asset(
            "audio-only.mp3", "audio/mpeg", "audio", 3_000
        )
        request = {
            "model": "sd_2.0_mini_special",
            "prompt": "audio only",
            "resolution": "720p",
            "duration": 5,
            "aspect_ratio": "16:9",
            "reference_asset_ids": [audio["id"]],
        }
        with self.assertRaises(ApiError) as context:
            self.service.quote_generation(self.user_id, "video", request)
        self.assertEqual(context.exception.code, "AUDIO_REFERENCE_REQUIRES_VISUAL")

        videos = [
            self.synthetic_generation_asset(
                f"video-{index}.mp4", "video/mp4", "video", 3_000
            )
            for index in range(4)
        ]
        request["reference_asset_ids"] = [asset["id"] for asset in videos]
        with self.assertRaises(ApiError) as context:
            self.service.quote_generation(self.user_id, "video", request)
        self.assertEqual(context.exception.code, "REFERENCE_KIND_LIMIT_EXCEEDED")

        long_videos = [
            self.synthetic_generation_asset(
                f"long-video-{index}.mp4", "video/mp4", "video", 6_000
            )
            for index in range(3)
        ]
        request["reference_asset_ids"] = [asset["id"] for asset in long_videos]
        with self.assertRaises(ApiError) as context:
            self.service.quote_generation(self.user_id, "video", request)
        self.assertEqual(context.exception.code, "REFERENCE_TOTAL_DURATION_EXCEEDED")

    def test_generation_upload_rejects_spoofed_media_content(self):
        upload = self.service.prepare_upload(
            self.user_id, "generation_input", "fake.mp3", "audio/mpeg", len(PNG)
        )
        with self.assertRaises(ApiError) as context:
            self.service.save_upload_stream(
                upload["upload_id"], io.BytesIO(PNG), len(PNG), "audio/mpeg"
            )
        self.assertEqual(context.exception.code, "INVALID_REFERENCE_MEDIA")

    def test_generation_media_probe_rejects_non_finite_duration_and_limits_protocols(self):
        with tempfile.NamedTemporaryFile(suffix=".mp4") as media:
            media.write(b"synthetic-video")
            media.flush()
            probe = {
                "format": {"format_name": "mov,mp4", "duration": "nan"},
                "streams": [
                    {
                        "codec_type": "video",
                        "codec_name": "h264",
                        "width": 1280,
                        "height": 720,
                        "duration": "inf",
                    }
                ],
            }
            with patch(
                "seecut_server.service.subprocess.run",
                return_value=SimpleNamespace(stdout=json.dumps(probe).encode()),
            ) as run:
                with self.assertRaises(ApiError) as context:
                    self.service._probe_generation_input_file(
                        Path(media.name), "video/mp4"
                    )
        self.assertEqual(context.exception.code, "INVALID_REFERENCE_DURATION")
        command = run.call_args.args[0]
        self.assertEqual(
            command[command.index("-protocol_whitelist") + 1], "file,pipe"
        )

    def test_image_edit_preserves_reference_asset_order(self):
        first = self.generation_asset("first.png")
        second = self.generation_asset("second.png")
        reference_ids = [second["id"], first["id"]]
        request = {
            "model": "gpt-image-2.5-flare",
            "operation": "edit",
            "prompt": "use the references in order",
            "reference_asset_ids": reference_ids,
        }
        quote = self.service.quote_generation(self.user_id, "image", request)
        self.service.create_generation_task(
            self.user_id,
            "image",
            request | {"quote_id": quote["quote_id"]},
            "ordered-image-edit",
        )

        self.service.process_generation_task(self.service.claim_generation_tasks()[0])

        _, images = self.image2.edit_calls[0]
        self.assertEqual([path.name for path, _ in images], ["second.png", "first.png"])

    def test_video_payload_preserves_reference_asset_order(self):
        first = self.generation_asset("first-video-reference.png")
        second = self.generation_asset("second-video-reference.png")
        with self.service.db.transaction() as connection:
            connection.execute(
                "UPDATE generation_assets SET provider_asset_id=? WHERE id=?",
                ("provider-first", first["id"]),
            )
            connection.execute(
                "UPDATE generation_assets SET provider_asset_id=? WHERE id=?",
                ("provider-second", second["id"]),
            )
        task = {
            "user_id": self.user_id,
            "request_json": json.dumps(
                {
                    "model": "sd_2.0_mini_special",
                    "operation": "generate",
                    "prompt": "use the references in order",
                    "reference_asset_ids": [second["id"], first["id"]],
                }
            ),
        }

        payload = self.service._task_payload(task)

        self.assertEqual(
            payload["reference_images"],
            ["assetId://provider-second", "assetId://provider-first"],
        )

    def test_polling_error_keeps_hold_for_reconcile(self):
        task = self.video_task()
        claimed = self.service.claim_generation_tasks()[0]
        self.service.process_generation_task(claimed)
        self.clock.value += 11
        self.xiangxin.poll_error = XiangxinError("BAD_POLL", "lookup rejected", False)
        claimed = self.service.claim_generation_tasks()[0]
        self.service.process_generation_task(claimed)
        current = self.service.get_generation_task(self.user_id, task["id"])
        self.assertEqual(current["status"], "pending_reconcile")
        self.assertEqual(self.service.wallet(self.user_id)["held_credits"], 12)

    def test_async_image_pending_is_not_claimed_or_resubmitted(self):
        self.image2.error = Image2Error(
            "IMAGE2_ASYNC_PENDING",
            "still processing",
            True,
            "image-upstream",
        )
        task = self.image_task()
        self.service.process_generation_task(self.service.claim_generation_tasks()[0])

        current = self.service.get_generation_task(self.user_id, task["id"])
        self.assertEqual(current["status"], "pending_reconcile")
        self.assertEqual(current["upstream_task_id"], "image-upstream")
        self.assertEqual(self.image2.calls, 1)
        self.clock.value += 301
        self.assertEqual(self.service.claim_generation_tasks(), [])
        self.assertEqual(self.image2.calls, 1)
        wallet = self.service.wallet(self.user_id)
        self.assertEqual(wallet["available_credits"], 93)
        self.assertEqual(wallet["held_credits"], 7)

    def test_success_clears_stale_reconcile_error(self):
        task = self.image_task()
        with self.service.db.transaction() as connection:
            connection.execute(
                "UPDATE generation_tasks SET error_code=?,error_message=? WHERE id=?",
                ("PROVIDER_RESULT_UNAVAILABLE", "temporary failure", task["id"]),
            )

        self.service.process_generation_task(self.service.claim_generation_tasks()[0])

        current = self.service.get_generation_task(self.user_id, task["id"])
        self.assertEqual(current["status"], "succeeded")
        self.assertIsNone(current["error"])

    def test_idempotency_conflict_rejects_different_payload(self):
        task = self.image_task("same-key")
        other = {"model": "gpt-image-2.5-flare", "prompt": "different product"}
        quote = self.service.quote_generation(self.user_id, "image", other)
        with self.assertRaises(ApiError) as context:
            self.service.create_generation_task(
                self.user_id, "image", other | {"quote_id": quote["quote_id"]}, "same-key"
            )
        self.assertEqual(context.exception.code, "IDEMPOTENCY_CONFLICT")
        self.assertEqual(self.service.get_generation_task(self.user_id, task["id"])["prompt"], "clean product")

    def test_existing_task_accepts_new_quote_with_same_idempotency_key(self):
        request = {"model": "gpt-image-2.5-flare", "prompt": "stable retry"}
        first_quote = self.service.quote_generation(self.user_id, "image", request)
        original = self.service.create_generation_task(
            self.user_id,
            "image",
            request | {"quote_id": first_quote["quote_id"]},
            "stable-key",
        )

        self.clock.value += 601
        replacement_quote = self.service.quote_generation(self.user_id, "image", request)
        recovered = self.service.create_generation_task(
            self.user_id,
            "image",
            request | {"quote_id": replacement_quote["quote_id"]},
            "stable-key",
        )

        self.assertEqual(recovered["id"], original["id"])
        with self.service.db.connect() as connection:
            task_count = connection.execute(
                "SELECT COUNT(*) FROM generation_tasks WHERE user_id=?",
                (self.user_id,),
            ).fetchone()[0]
            hold_count = connection.execute(
                "SELECT COUNT(*) FROM wallet_holds WHERE user_id=?",
                (self.user_id,),
            ).fetchone()[0]
        self.assertEqual(task_count, 1)
        self.assertEqual(hold_count, 1)
        wallet = self.service.wallet(self.user_id)
        self.assertEqual(wallet["available_credits"], 93)
        self.assertEqual(wallet["held_credits"], 7)

    def test_quote_is_consumed_once_under_concurrent_submissions(self):
        request = {"model": "gpt-image-2.5-flare", "prompt": "one quoted request"}
        quote = self.service.quote_generation(self.user_id, "image", request)
        barrier = threading.Barrier(2)
        outcomes: list[dict | ApiError] = []
        lock = threading.Lock()

        def submit(key: str) -> None:
            barrier.wait()
            try:
                outcome = self.service.create_generation_task(
                    self.user_id,
                    "image",
                    request | {"quote_id": quote["quote_id"]},
                    key,
                )
            except ApiError as exc:
                outcome = exc
            with lock:
                outcomes.append(outcome)

        threads = [threading.Thread(target=submit, args=(f"concurrent-{index}",)) for index in range(2)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

        successes = [outcome for outcome in outcomes if isinstance(outcome, dict)]
        failures = [outcome for outcome in outcomes if isinstance(outcome, ApiError)]
        self.assertEqual(len(successes), 1)
        self.assertEqual([failure.code for failure in failures], ["QUOTE_MISMATCH"])
        wallet = self.service.wallet(self.user_id)
        self.assertEqual(wallet["available_credits"], 93)
        self.assertEqual(wallet["held_credits"], 7)
        with self.service.db.connect() as connection:
            task_count = connection.execute(
                "SELECT COUNT(*) FROM generation_tasks WHERE user_id=?",
                (self.user_id,),
            ).fetchone()[0]
        self.assertEqual(task_count, 1)

    def test_invalid_image_does_not_capture_credits(self):
        self.image2.content = b"not-an-image"
        task = self.image_task()
        self.service.process_generation_task(self.service.claim_generation_tasks()[0])
        current = self.service.get_generation_task(self.user_id, task["id"])
        self.assertEqual(current["status"], "failed")
        wallet = self.service.wallet(self.user_id)
        self.assertEqual(wallet["available_credits"], 100)
        self.assertEqual(wallet["held_credits"], 0)

    def test_cleanup_expires_outputs_but_keeps_inflight_input(self):
        image_task = self.image_task()
        self.service.process_generation_task(self.service.claim_generation_tasks()[0])
        completed = self.service.get_generation_task(self.user_id, image_task["id"])
        output_path = self.service.config.storage_path / "outputs" / image_task["id"] / completed["outputs"][0]["id"]
        self.assertTrue(output_path.is_file())

        upload = self.service.prepare_upload(
            self.user_id, "generation_input", "reference.png", "image/png", len(PNG)
        )
        self.service.save_upload_stream(upload["upload_id"], io.BytesIO(PNG), len(PNG), "image/png")
        generation_asset = self.service.register_generation_asset(self.user_id, upload["upload_id"])
        video_task = self.video_task(reference_asset_ids=[generation_asset["id"]])

        self.clock.value += 61
        cleaned = self.service.cleanup_expired()
        self.assertEqual(cleaned["outputs"], 1)
        self.assertFalse(output_path.exists())
        self.assertEqual(self.service.get_generation_task(self.user_id, image_task["id"])["status"], "expired")
        with self.service.db.connect() as connection:
            staging = connection.execute(
                "SELECT object_key FROM uploads WHERE id=?", (upload["upload_id"],)
            ).fetchone()
        self.assertIsNotNone(staging)
        self.assertTrue((self.service.config.storage_path / staging["object_key"]).is_file())
        self.assertEqual(self.service.get_generation_task(self.user_id, video_task["id"])["status"], "queued")

    def test_production_requires_dimension_specific_price(self):
        production = SeeCutService(
            replace(self.service.config, env="production"),
            self.service.db,
            image2=self.image2,
            xiangxin=self.xiangxin,
            clock=self.clock,
        )
        with self.assertRaises(ApiError) as context:
            production.quote_generation(
                self.user_id,
                "image",
                {"model": "gpt-image-2.5-flare", "prompt": "priced request"},
            )
        self.assertEqual(context.exception.code, "MODEL_PRICE_NOT_CONFIGURED")

    def test_zero_price_image_succeeds_with_zero_balance_without_credit_movement(self):
        image_model = "gpt-image-2"
        billing_key = f"image2:{image_model}:operation=generate:size=auto:quality=high"
        service = SeeCutService(
            replace(
                self.service.config,
                env="production",
                image2_model=image_model,
                model_prices={billing_key: 0},
            ),
            self.service.db,
            image2=self.image2,
            xiangxin=self.xiangxin,
            clock=self.clock,
        )
        with service.db.transaction() as connection:
            connection.execute(
                "UPDATE wallets SET available_credits=0, held_credits=0 WHERE user_id=?",
                (self.user_id,),
            )

        request = {"model": image_model, "prompt": "zero-price image"}
        quote = service.quote_generation(self.user_id, "image", request)
        task = service.create_generation_task(
            self.user_id,
            "image",
            request | {"quote_id": quote["quote_id"]},
            "zero-price-image",
        )
        service.process_generation_task(service.claim_generation_tasks()[0])

        self.assertEqual(quote["credits"], 0)
        self.assertEqual(service.get_generation_task(self.user_id, task["id"])["status"], "succeeded")
        self.assertEqual(service.wallet(self.user_id)["available_credits"], 0)
        self.assertEqual(service.wallet(self.user_id)["held_credits"], 0)
        with service.db.connect() as connection:
            entries = connection.execute(
                """SELECT kind,delta_available,delta_held FROM ledger_entries
                   WHERE reference_id=? ORDER BY rowid""",
                (task["id"],),
            ).fetchall()
        self.assertEqual(
            [(row["kind"], row["delta_available"], row["delta_held"]) for row in entries],
            [("hold", 0, 0), ("capture", 0, 0)],
        )

    def test_zero_price_video_failure_releases_without_credit_movement(self):
        billing_key = (
            "xiangxin:sd_2.0_mini_special:resolution=720p:duration=5:"
            "aspect_ratio=16:9:reference_count=0"
        )
        service = SeeCutService(
            replace(self.service.config, env="production", model_prices={billing_key: 0}),
            self.service.db,
            image2=self.image2,
            xiangxin=self.xiangxin,
            clock=self.clock,
        )
        with service.db.transaction() as connection:
            connection.execute(
                "UPDATE wallets SET available_credits=0, held_credits=0 WHERE user_id=?",
                (self.user_id,),
            )

        request = {
            "model": "sd_2.0_mini_special",
            "prompt": "zero-price video",
            "resolution": "720p",
            "duration": 5,
            "aspect_ratio": "16:9",
        }
        quote = service.quote_generation(self.user_id, "video", request)
        task = service.create_generation_task(
            self.user_id,
            "video",
            request | {"quote_id": quote["quote_id"]},
            "zero-price-video",
        )
        service.process_generation_task(service.claim_generation_tasks()[0])
        self.clock.value += 11
        self.xiangxin.poll_response = {"id": "video-upstream", "status": "failed", "error": "rejected"}
        service.process_generation_task(service.claim_generation_tasks()[0])

        self.assertEqual(quote["credits"], 0)
        self.assertEqual(service.get_generation_task(self.user_id, task["id"])["status"], "failed")
        self.assertEqual(service.wallet(self.user_id)["available_credits"], 0)
        self.assertEqual(service.wallet(self.user_id)["held_credits"], 0)
        with service.db.connect() as connection:
            entries = connection.execute(
                """SELECT kind,delta_available,delta_held FROM ledger_entries
                   WHERE reference_id=? ORDER BY rowid""",
                (task["id"],),
            ).fetchall()
        self.assertEqual(
            [(row["kind"], row["delta_available"], row["delta_held"]) for row in entries],
            [("hold", 0, 0), ("release", 0, 0)],
        )


if __name__ == "__main__":
    unittest.main()
