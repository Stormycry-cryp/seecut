from __future__ import annotations

import os
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
import json
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from seecut_server.config import Config
from seecut_server.db import Database
from seecut_server.emailer import EmailDeliveryError
from seecut_server.errors import ApiError
from seecut_server.http import SeeCutHandler
from seecut_server.service import SeeCutService


class Clock:
    def __init__(self, value: int = 1_700_000_000):
        self.value = value

    def __call__(self) -> int:
        return self.value


class RecordingEmailSender:
    configured = True

    def __init__(self):
        self.sent: list[tuple[str, str, str]] = []
        self.fail = False

    def send_token(self, email: str, purpose: str, token: str) -> None:
        if self.fail:
            raise EmailDeliveryError("unavailable")
        self.sent.append((email, purpose, token))


class EmailCodeContractTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        with patch.dict(
            os.environ,
            {
                "SECUT_ENV": "test",
                "SECUT_DATABASE_PATH": str(root / "database.sqlite3"),
                "SECUT_STORAGE_PATH": str(root / "storage"),
                "SECUT_SIGNING_SECRET": "test-signing-secret-that-is-long-enough",
                "SECUT_EXPOSE_TEST_TOKENS": "true",
            },
            clear=True,
        ):
            config = Config.from_env()
        self.database = Database(config.database_path, Path(__file__).resolve().parents[1] / "schema.sql")
        self.database.initialize()
        self.clock = Clock()
        self.emailer = RecordingEmailSender()
        self.service = SeeCutService(
            config, self.database, emailer=self.emailer, clock=self.clock
        )

    def tearDown(self):
        self.directory.cleanup()

    def register(self, email: str = "person@example.test") -> dict:
        return self.service.register(email, "password-long-enough")

    def test_codes_are_six_digits_and_bound_to_email_and_purpose(self):
        with patch("seecut_server.security.secrets.randbelow", side_effect=[123456, 654321, 222222]):
            first = self.register("first@example.test")
            second = self.register("second@example.test")
            self.assertEqual(first["verification_token"], "123456")
            self.assertEqual(second["verification_token"], "654321")

            with self.assertRaises(ApiError) as context:
                self.service.verify_email("second@example.test", first["verification_token"])
            self.assertEqual(context.exception.code, "INVALID_OR_EXPIRED_TOKEN")
            self.service.verify_email("first@example.test", first["verification_token"])

            reset = self.service.forgot_password("first@example.test")
            with self.assertRaises(ApiError):
                self.service.verify_email("first@example.test", reset["reset_token"])

    def test_five_incorrect_attempts_lock_the_code(self):
        with patch("seecut_server.security.secrets.randbelow", return_value=123456):
            registration = self.register()
        for _ in range(5):
            with self.assertRaises(ApiError):
                self.service.verify_email("person@example.test", "654321")
        with self.assertRaises(ApiError):
            self.service.verify_email(
                "person@example.test", registration["verification_token"]
            )
        with self.database.connect() as connection:
            attempts = connection.execute(
                "SELECT failed_attempts FROM email_tokens"
            ).fetchone()["failed_attempts"]
        self.assertEqual(attempts, 5)

    def test_resend_and_forgot_enforce_sixty_second_cooldown(self):
        registration = self.register()
        with self.assertRaises(ApiError) as context:
            self.service.resend_verification("person@example.test")
        self.assertEqual(context.exception.code, "EMAIL_CODE_COOLDOWN")
        self.assertEqual(context.exception.details["retry_after"], 60)

        self.clock.value += 60
        resent = self.service.resend_verification("person@example.test")
        self.assertRegex(resent["verification_token"], r"^\d{6}$")
        reset = self.service.forgot_password("person@example.test")
        with self.assertRaises(ApiError) as reset_context:
            self.service.forgot_password("person@example.test")
        self.assertEqual(reset_context.exception.code, "EMAIL_CODE_COOLDOWN")
        self.assertRegex(reset["reset_token"], r"^\d{6}$")

    def test_delivery_failure_returns_503_and_unknown_email_stays_accepted(self):
        self.emailer.fail = True
        with self.assertRaises(ApiError) as context:
            self.register()
        self.assertEqual(context.exception.status, 503)
        self.assertEqual(context.exception.code, "EMAIL_DELIVERY_UNAVAILABLE")
        with self.database.connect() as connection:
            self.assertEqual(connection.execute("SELECT COUNT(*) FROM users").fetchone()[0], 1)
            self.assertEqual(connection.execute("SELECT COUNT(*) FROM email_tokens").fetchone()[0], 0)

        self.assertEqual(
            self.service.resend_verification("unknown@example.test"), {"accepted": True}
        )
        self.assertEqual(
            self.service.forgot_password("unknown@example.test"), {"accepted": True}
        )
        with self.assertRaises(ApiError) as resend_context:
            self.service.resend_verification("person@example.test")
        self.assertEqual(resend_context.exception.status, 503)

    def test_reset_does_not_mark_an_unverified_email_as_verified(self):
        self.register()
        reset = self.service.forgot_password("person@example.test")
        self.service.reset_password(
            "person@example.test", reset["reset_token"], "replacement-password-long"
        )
        with self.database.connect() as connection:
            verified_at = connection.execute(
                "SELECT email_verified_at FROM users WHERE email='person@example.test'"
            ).fetchone()["email_verified_at"]
        self.assertIsNone(verified_at)


class EmailTokenMigrationTest(unittest.TestCase):
    def test_existing_email_token_rows_survive_failed_attempts_migration(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "database.sqlite3"
            connection = sqlite3.connect(path)
            connection.executescript(
                """
                CREATE TABLE users (
                    id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL,
                    email_verified_at INTEGER, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
                );
                CREATE TABLE email_tokens (
                    id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id),
                    purpose TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE,
                    expires_at INTEGER NOT NULL, consumed_at INTEGER, created_at INTEGER NOT NULL
                );
                INSERT INTO users VALUES ('usr_old','old@example.test','hash',NULL,1,1);
                INSERT INTO email_tokens VALUES ('emt_old','usr_old','verify_email','digest',99,NULL,1);
                """
            )
            connection.commit()
            connection.close()

            database = Database(path, Path(__file__).resolve().parents[1] / "schema.sql")
            database.initialize()
            with database.connect() as migrated:
                row = migrated.execute(
                    "SELECT id,failed_attempts FROM email_tokens WHERE id='emt_old'"
                ).fetchone()
            self.assertEqual(dict(row), {"id": "emt_old", "failed_attempts": 0})


class EmailCodeHttpFlowTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        with patch.dict(
            os.environ,
            {
                "SECUT_ENV": "test",
                "SECUT_DATABASE_PATH": str(root / "database.sqlite3"),
                "SECUT_STORAGE_PATH": str(root / "storage"),
                "SECUT_SIGNING_SECRET": "test-signing-secret-that-is-long-enough",
                "SECUT_EXPOSE_TEST_TOKENS": "false",
            },
            clear=True,
        ):
            config = Config.from_env()
        database = Database(config.database_path, Path(__file__).resolve().parents[1] / "schema.sql")
        database.initialize()
        self.clock = Clock()
        self.emailer = RecordingEmailSender()
        service = SeeCutService(config, database, emailer=self.emailer, clock=self.clock)

        handler = type(
            "EmailCodeTestHandler",
            (SeeCutHandler,),
            {"service": service, "log_message": lambda *args: None},
        )
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.directory.cleanup()

    def request(
        self, method: str, path: str, body: dict | None = None, token: str | None = None
    ) -> tuple[int, dict]:
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        request = urllib.request.Request(
            self.base_url + path,
            data=json.dumps(body or {}).encode("utf-8") if body is not None else None,
            method=method,
            headers=headers,
        )
        try:
            with urllib.request.urlopen(request, timeout=2) as response:
                return response.status, json.loads(response.read().decode("utf-8") or "{}")
        except urllib.error.HTTPError as error:
            try:
                return error.code, json.loads(error.read().decode("utf-8"))
            finally:
                error.close()

    def test_complete_email_account_flow_and_code_lifecycle(self):
        email = "flow@example.test"
        password = "original-password-long"
        with patch(
            "seecut_server.security.secrets.randbelow",
            side_effect=[111111, 222222, 333333, 444444],
        ):
            status, registration = self.request(
                "POST", "/api/auth/register", {"email": email, "password": password}
            )
            self.assertEqual(status, 201)
            self.assertNotIn("verification_token", registration)
            first_code = self.emailer.sent[-1][2]
            self.assertEqual(first_code, "111111")

            self.clock.value += 60
            status, resend = self.request(
                "POST", "/api/auth/resend-verification", {"email": email}
            )
            self.assertEqual((status, resend), (202, {"accepted": True}))
            second_code = self.emailer.sent[-1][2]
            self.assertEqual(second_code, "222222")

            status, _ = self.request(
                "POST", "/api/auth/verify-email", {"email": email, "token": first_code}
            )
            self.assertEqual(status, 400)
            status, verified = self.request(
                "POST", "/api/auth/verify-email", {"email": email, "token": second_code}
            )
            self.assertEqual((status, verified), (200, {"verified": True}))
            status, _ = self.request(
                "POST", "/api/auth/verify-email", {"email": email, "token": second_code}
            )
            self.assertEqual(status, 400)

            status, login = self.request(
                "POST", "/api/auth/login", {"email": email, "password": password}
            )
            self.assertEqual(status, 200)
            old_session = login["access_token"]

            status, forgot = self.request(
                "POST", "/api/auth/forgot-password", {"email": email}
            )
            self.assertEqual((status, forgot), (202, {"accepted": True}))
            reset_code = self.emailer.sent[-1][2]
            self.assertEqual(reset_code, "333333")
            status, reset = self.request(
                "POST",
                "/api/auth/reset-password",
                {"email": email, "token": reset_code, "password": "replacement-password-long"},
            )
            self.assertEqual((status, reset), (200, {"reset": True}))
            status, _ = self.request(
                "POST",
                "/api/auth/reset-password",
                {"email": email, "token": reset_code, "password": "another-password-long"},
            )
            self.assertEqual(status, 400)
            status, _ = self.request("GET", "/api/auth/me", token=old_session)
            self.assertEqual(status, 401)
            status, _ = self.request(
                "POST", "/api/auth/login", {"email": email, "password": password}
            )
            self.assertEqual(status, 401)
            status, new_login = self.request(
                "POST",
                "/api/auth/login",
                {"email": email, "password": "replacement-password-long"},
            )
            self.assertEqual(status, 200)
            self.assertEqual(new_login["user"]["email"], email)

            expired_email = "expired@example.test"
            status, expired_registration = self.request(
                "POST",
                "/api/auth/register",
                {"email": expired_email, "password": password},
            )
            self.assertEqual(status, 201)
            self.assertNotIn("verification_token", expired_registration)
            expired_code = self.emailer.sent[-1][2]
            self.assertEqual(expired_code, "444444")
            self.clock.value += 1801
            status, _ = self.request(
                "POST",
                "/api/auth/verify-email",
                {"email": expired_email, "token": expired_code},
            )
            self.assertEqual(status, 400)
