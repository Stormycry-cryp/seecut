from __future__ import annotations

import json
import os
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

from seecut_server.config import Config
from seecut_server.catalog import public_catalog, validate_request
from seecut_server.db import Database
from seecut_server.errors import ApiError
from seecut_server.image2 import Image2Client, Image2Error
from seecut_server.network import NoRedirectHandler, read_limited
from seecut_server.ratelimit import InMemoryRateLimiter
from seecut_server.service import SeeCutService
from seecut_server.xiangxin import XiangxinClient, XiangxinError


class FakeResponse:
    def __init__(self, body: bytes):
        self.body = body

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def read(self, size=-1):
        if size < 0:
            size = len(self.body)
        data, self.body = self.body[:size], self.body[size:]
        return data


class SecurityBoundaryTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        with patch.dict(
            os.environ,
            {
                "SECUT_ENV": "test",
                "SECUT_DATABASE_PATH": str(root / "db.sqlite3"),
                "SECUT_STORAGE_PATH": str(root / "storage"),
                "SECUT_SIGNING_SECRET": "test-signing-secret-that-is-long-enough",
                "SECUT_IMAGE2_API_KEY": "image-key",
                "SECUT_XIANGXIN_API_KEY": "video-key",
                "SECUT_MAX_PROVIDER_RESPONSE_BYTES": "1024",
                "SECUT_MAX_REFERENCE_IMAGE_BYTES": "20",
            },
            clear=True,
        ):
            self.config = Config.from_env()

    def tearDown(self):
        self.temp.cleanup()

    def test_redirect_handler_never_reissues_request(self):
        self.assertIsNone(NoRedirectHandler().redirect_request(None, None, 302, "Found", {}, "https://evil.test"))

    def test_provider_response_read_is_bounded(self):
        with self.assertRaises(ValueError):
            read_limited(FakeResponse(b"12345"), 4)

    def test_image2_async_result_keeps_provider_task_id(self):
        client = Image2Client(self.config)
        response = FakeResponse(json.dumps({"id": "img-task-1", "status": "queued"}).encode())
        with patch("seecut_server.image2.open_no_redirect", return_value=response):
            with self.assertRaises(Image2Error) as context:
                client.create({"model": self.config.image2_model, "prompt": "test"})
        self.assertEqual(context.exception.code, "IMAGE2_ASYNC_PENDING")
        self.assertEqual(context.exception.upstream_task_id, "img-task-1")

    def test_xiangxin_asset_request_declares_image_type(self):
        client = XiangxinClient(self.config)
        with patch.object(client, "_request", return_value={}) as request:
            client.register_asset("https://example.test/reference.png")
        request.assert_called_once_with(
            "POST",
            "/v1/videos/assets",
            {"assetType": "Image", "url": "https://example.test/reference.png"},
        )

    def test_image_model_is_configurable_across_catalog_and_client(self):
        with patch.dict(os.environ, {"SECUT_IMAGE2_MODEL": "gpt-image-2"}, clear=True):
            config = Config.from_env()
        self.assertEqual(public_catalog(config.image2_model)["models"][0]["id"], "gpt-image-2")
        model, normalized = validate_request(
            "image",
            {"model": "gpt-image-2", "prompt": "configured model"},
            config.image2_model,
        )
        self.assertEqual(model["id"], "gpt-image-2")
        self.assertEqual(normalized["model"], "gpt-image-2")
        self.assertEqual(Image2Client(config).model, "gpt-image-2")

    def test_rate_limiter_is_per_key_and_windowed(self):
        limiter = InMemoryRateLimiter(2, 10)
        self.assertTrue(limiter.allow("198.51.100.1", now=100))
        self.assertTrue(limiter.allow("198.51.100.1", now=101))
        self.assertFalse(limiter.allow("198.51.100.1", now=102))
        self.assertTrue(limiter.allow("198.51.100.2", now=102))
        self.assertTrue(limiter.allow("198.51.100.1", now=111))

    def test_password_has_upper_bound(self):
        database = Database(self.config.database_path, Path(__file__).resolve().parents[1] / "schema.sql")
        database.initialize()
        service = SeeCutService(self.config, database)
        with self.assertRaises(ApiError) as context:
            service.register("long@example.test", "x" * 129)
        self.assertEqual(context.exception.code, "INVALID_PASSWORD")

    def test_reference_image_upload_is_limited_to_configured_size(self):
        database = Database(self.config.database_path, Path(__file__).resolve().parents[1] / "schema.sql")
        database.initialize()
        service = SeeCutService(self.config, database)
        registration = service.register("image@example.test", "long-enough-password")
        with self.assertRaises(ApiError) as context:
            service.prepare_upload(
                registration["user_id"], "generation_input", "large.png", "image/png", 21
            )
        self.assertEqual(context.exception.code, "REFERENCE_IMAGE_TOO_LARGE")

    def test_alipay_response_signature_requires_rsa2_and_signature(self):
        database = Database(self.config.database_path, Path(__file__).resolve().parents[1] / "schema.sql")
        database.initialize()
        service = SeeCutService(self.config, database)
        payload = {
            "alipay_trade_query_response": {"code": "10000", "trade_status": "TRADE_SUCCESS"},
            "sign": "signature",
            "sign_type": "RSA2",
        }
        with patch.object(service, "_verify_alipay_signature_content", return_value=True) as verify:
            self.assertTrue(service._verify_alipay_response_signature(payload))
            verify.assert_called_once()
        self.assertFalse(service._verify_alipay_response_signature({**payload, "sign_type": "RSA"}))
        self.assertFalse(service._verify_alipay_response_signature({"alipay_trade_query_response": {}}))

    def test_provider_result_redirect_is_rejected_before_following(self):
        database = Database(self.config.database_path, Path(__file__).resolve().parents[1] / "schema.sql")
        database.initialize()
        service = SeeCutService(self.config, database)
        redirect = urllib.error.HTTPError(
            "https://xiangxinai123.xyz/result.mp4", 302, "Found", {"Location": "https://evil.test"}, None
        )
        with patch("seecut_server.service.open_no_redirect", side_effect=redirect):
            with self.assertRaises(XiangxinError) as context:
                service._download_provider_result("https://xiangxinai123.xyz/result.mp4", "video")
        redirect.close()
        self.assertEqual(context.exception.code, "PROVIDER_RESULT_UNAVAILABLE")


if __name__ == "__main__":
    unittest.main()
