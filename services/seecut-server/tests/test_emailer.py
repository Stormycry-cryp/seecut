from __future__ import annotations

import os
import ssl
import unittest
from unittest.mock import patch

from seecut_server.config import Config
from seecut_server.emailer import EmailSender


class FakeSmtp:
    instance = None

    def __init__(self, host: str, port: int, timeout: int):
        self.host = host
        self.port = port
        self.timeout = timeout
        self.events = []
        FakeSmtp.instance = self

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, traceback):
        return False

    def starttls(self, *, context):
        self.events.append(("starttls", context))

    def login(self, username: str, password: str):
        self.events.append(("login", username, password))

    def send_message(self, message):
        self.events.append(("send", message))


class EmailSenderTest(unittest.TestCase):
    def test_verification_email_uses_verified_tls_and_chinese_content(self):
        with patch.dict(
            os.environ,
            {
                "SECUT_ENV": "test",
                "SECUT_EMAIL_PROVIDER": "smtp",
                "SECUT_EMAIL_FROM": "seecut@mail.stormycry.cloud",
                "SECUT_SMTP_HOST": "smtp.qcloudmail.com",
                "SECUT_SMTP_PORT": "587",
                "SECUT_SMTP_USERNAME": "seecut-user",
                "SECUT_SMTP_PASSWORD": "seecut-password",
                "SECUT_SMTP_STARTTLS": "true",
            },
            clear=True,
        ):
            sender = EmailSender(Config.from_env())

        with patch("seecut_server.emailer.smtplib.SMTP", FakeSmtp):
            sender.send_token("person@example.test", "verify_email", "验证-123456")

        smtp = FakeSmtp.instance
        self.assertEqual((smtp.host, smtp.port, smtp.timeout), ("smtp.qcloudmail.com", 587, 15))
        self.assertEqual([event[0] for event in smtp.events], ["starttls", "login", "send"])
        context = smtp.events[0][1]
        self.assertEqual(context.verify_mode, ssl.CERT_REQUIRED)
        self.assertTrue(context.check_hostname)
        message = smtp.events[2][1]
        self.assertEqual(message["From"], "seecut@mail.stormycry.cloud")
        self.assertEqual(message["To"], "person@example.test")
        self.assertEqual(message["Subject"], "验证你的 SeeCut 邮箱")
        self.assertIn("验证邮箱验证码：验证-123456", message.get_content())
        self.assertIn("30 分钟后失效", message.get_content())


if __name__ == "__main__":
    unittest.main()
