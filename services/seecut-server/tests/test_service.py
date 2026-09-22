from __future__ import annotations

import os
import base64
import json
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from seecut_server.config import Config
from seecut_server.db import Database
from seecut_server.emailer import EmailSender
from seecut_server.service import ApiError, SeeCutService


class FakeEmailSender(EmailSender):
    def __init__(self, config: Config):
        super().__init__(config)
        self.sent: list[tuple[str, str, str]] = []

    @property
    def configured(self) -> bool:
        return True

    def send_token(self, email: str, purpose: str, token: str) -> None:
        self.sent.append((email, purpose, token))


class FakeXiangxin:
    configured = True

    def create_image(self, body):
        return {"created": 1, "data": [{"url": "https://example.test/result.png"}]}

    def create_video(self, body):
        return {"id": "upstream-video-1", "status": "queued"}

    def get_video(self, task_id):
        return {"id": task_id, "status": "completed", "video_url": "https://example.test/result.mp4"}

    def models(self):
        return {"data": [{"id": "image-model"}, {"id": "video-model"}]}

    def register_asset(self, source_url, asset_type):
        return {"assetId": "asset-1", "source_url_seen": source_url.startswith("http")}


class FakeImage2:
    configured = True

    def create(self, body):
        return [
            base64.b64decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
            )
        ]


class ServiceTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        env = {
            "SECUT_ENV": "test",
            "SECUT_DATABASE_PATH": str(root / "test.sqlite3"),
            "SECUT_STORAGE_PATH": str(root / "storage"),
            "SECUT_SIGNING_SECRET": "test-signing-secret-that-is-long-enough",
            "SECUT_EXPOSE_TEST_TOKENS": "true",
            "SECUT_MODEL_PRICES_JSON": '{"image:gpt-image-2.5-flare":7,"video:sd_2.0_mini_special":12}',
        }
        with patch.dict(os.environ, env, clear=True):
            config = Config.from_env()
        schema = Path(__file__).resolve().parents[1] / "schema.sql"
        database = Database(config.database_path, schema)
        database.initialize()
        self.emailer = FakeEmailSender(config)
        self.service = SeeCutService(
            config, database, emailer=self.emailer, xiangxin=FakeXiangxin(), image2=FakeImage2(), clock=lambda: 1_700_000_000
        )

    def tearDown(self):
        self.temp.cleanup()

    def create_user(self, email: str) -> tuple[str, str]:
        registration = self.service.register(email, "correct-horse-battery")
        self.service.verify_email(email, registration["verification_token"])
        login = self.service.login(email, "correct-horse-battery")
        return login["user"]["id"], login["access_token"]

    def test_registration_verification_login_and_logout(self):
        registration = self.service.register("User@Example.com", "correct-horse-battery")
        self.assertEqual(registration["email"], "user@example.com")
        with self.assertRaises(ApiError) as context:
            self.service.login("user@example.com", "correct-horse-battery")
        self.assertEqual(context.exception.code, "EMAIL_NOT_VERIFIED")
        self.service.verify_email("user@example.com", registration["verification_token"])
        login = self.service.login("user@example.com", "correct-horse-battery")
        self.assertEqual(self.service.authenticate(login["access_token"])["email"], "user@example.com")
        self.service.logout(login["access_token"])
        with self.assertRaises(ApiError):
            self.service.authenticate(login["access_token"])

    def test_invite_is_single_use(self):
        owner_id, _ = self.create_user("owner@example.com")
        member_id, _ = self.create_user("member@example.com")
        other_id, _ = self.create_user("other@example.com")
        team = self.service.create_team(owner_id, "Film Team")
        invite = self.service.create_invite(owner_id, team["id"])
        joined = self.service.accept_invite(member_id, invite["token"])
        self.assertEqual(joined["role"], "member")
        with self.assertRaises(ApiError) as context:
            self.service.accept_invite(other_id, invite["token"])
        self.assertEqual(context.exception.code, "INVITE_UNAVAILABLE")

    def test_team_asset_requires_explicit_team_upload(self):
        owner_id, _ = self.create_user("owner@example.com")
        team = self.service.create_team(owner_id, "Assets")
        body = base64.b64decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
        )
        upload = self.service.prepare_upload(
            owner_id, "team_asset", "frame.png", "image/png", len(body), team_id=team["id"]
        )
        self.service.save_upload(upload["upload_id"], body, "image/png")
        asset = self.service.complete_team_asset(owner_id, team["id"], upload["upload_id"])
        self.assertEqual(asset["filename"], "frame.png")
        self.assertEqual(len(self.service.list_assets(owner_id, team["id"])), 1)

        staging = self.service.prepare_upload(
            owner_id, "generation_input", "local.png", "image/png", len(body)
        )
        self.service.save_upload(staging["upload_id"], body, "image/png")
        with self.assertRaises(ApiError):
            self.service.complete_team_asset(owner_id, team["id"], staging["upload_id"])

    def test_generation_holds_and_captures_personal_credits(self):
        user_id, _ = self.create_user("creator@example.com")
        with self.service.db.transaction() as connection:
            connection.execute(
                "UPDATE wallets SET available_credits=20 WHERE user_id=?", (user_id,)
            )
        request = {"model": "gpt-image-2.5-flare", "prompt": "a clean studio image"}
        quote = self.service.quote_generation(user_id, "image", request)
        request["quote_id"] = quote["quote_id"]
        task = self.service.create_generation_task(
            user_id,
            "image",
            request,
            "request-1",
        )
        self.assertEqual(task["status"], "queued")
        self.assertEqual(task["request"]["model"], "gpt-image-2.5-flare")
        self.assertEqual(task["request"]["prompt"], "a clean studio image")
        self.assertNotIn("quote_id", task["request"])
        self.assertNotIn("provider", task["request"])
        for claimed in self.service.claim_generation_tasks():
            self.service.process_generation_task(claimed)
        task = self.service.get_generation_task(user_id, task["id"])
        self.assertEqual(task["status"], "succeeded")
        wallet = self.service.wallet(user_id)
        self.assertEqual(wallet["available_credits"], 13)
        self.assertEqual(wallet["held_credits"], 0)
        duplicate = self.service.create_generation_task(
            user_id,
            "image",
            request,
            "request-1",
        )
        self.assertEqual(duplicate["id"], task["id"])
        self.assertEqual(self.service.wallet(user_id)["available_credits"], 13)

    def test_generation_task_request_exposes_only_public_generation_fields(self):
        user_id, _ = self.create_user("request-snapshot@example.com")
        with self.service.db.transaction() as connection:
            connection.execute(
                "UPDATE wallets SET available_credits=20 WHERE user_id=?", (user_id,)
            )
        request = {"model": "gpt-image-2.5-flare", "prompt": "public prompt"}
        quote = self.service.quote_generation(user_id, "image", request)
        task = self.service.create_generation_task(
            user_id,
            "image",
            request | {"quote_id": quote["quote_id"]},
            "request-public-fields",
        )
        with self.service.db.transaction() as connection:
            stored = connection.execute(
                "SELECT request_json FROM generation_tasks WHERE id=?", (task["id"],)
            ).fetchone()
            injected = json.loads(stored["request_json"])
            injected.update(
                {
                    "provider": "internal-provider",
                    "api_key": "secret-never-return",
                    "worker_context": {"attempt": 3},
                }
            )
            connection.execute(
                "UPDATE generation_tasks SET request_json=? WHERE id=?",
                (json.dumps(injected), task["id"]),
            )

        public = self.service.get_generation_task(user_id, task["id"])["request"]
        self.assertEqual(public["model"], "gpt-image-2.5-flare")
        self.assertEqual(public["prompt"], "public prompt")
        self.assertEqual(public["size"], "auto")
        self.assertEqual(public["quality"], "high")
        self.assertNotIn("provider", public)
        self.assertNotIn("api_key", public)
        self.assertNotIn("worker_context", public)
        self.assertNotIn("n", public)
        self.assertNotIn("output_format", public)

    def test_alipay_configuration_failure_is_explicit(self):
        user_id, _ = self.create_user("payer@example.com")
        with self.assertRaises(ApiError) as context:
            self.service.create_order(user_id, "missing")
        self.assertEqual(context.exception.code, "ALIPAY_NOT_CONFIGURED")

    def test_verified_alipay_notification_credits_once(self):
        user_id, _ = self.create_user("payer@example.com")
        root = Path(self.temp.name)
        public_key = root / "alipay-public.pem"
        private_key = root / "merchant-private.pem"
        public_key.write_text("test", encoding="utf-8")
        private_key.write_text("test", encoding="utf-8")
        config = replace(
            self.service.config,
            alipay_app_id="app-1",
            alipay_public_key_path=str(public_key),
            alipay_merchant_private_key_path=str(private_key),
            alipay_notify_url="https://api.example.test/api/payments/alipay/notify",
            alipay_seller_id="seller-1",
        )
        service = SeeCutService(
            config,
            self.service.db,
            emailer=self.emailer,
            xiangxin=FakeXiangxin(),
            clock=lambda: 1_700_000_000,
        )
        with service.db.transaction() as connection:
            connection.execute(
                "INSERT INTO plans(id,name,price_fen,credits,active,created_at) VALUES('plan-1','100 积分',100,100,1,?)",
                (service.now(),),
            )
        with patch.object(service, "_alipay_page_url", return_value="https://pay.example.test/order"):
            order = service.create_order(user_id, "plan-1")
        fields = {
            "app_id": "app-1",
            "seller_id": "seller-1",
            "out_trade_no": order["id"],
            "trade_no": "trade-1",
            "trade_status": "TRADE_SUCCESS",
            "total_amount": "1.00",
            "sign_type": "RSA2",
            "sign": "test-signature",
        }
        with patch.object(service, "_verify_alipay_signature", return_value=True):
            self.assertEqual(service.process_alipay_notification(fields), "success")
            self.assertEqual(service.process_alipay_notification(fields), "success")
        self.assertEqual(service.wallet(user_id)["available_credits"], 100)


if __name__ == "__main__":
    unittest.main()
