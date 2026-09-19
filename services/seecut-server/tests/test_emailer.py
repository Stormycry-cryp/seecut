from __future__ import annotations

import os
import ssl
import unittest
from unittest.mock import patch

from seecut_server.config import Config, ConfigError
from seecut_server.emailer import EmailSender


class FakeSmtpBase:
    instance = None

    def __init__(self, host: str, port: int, timeout: int, local_hostname: str, **kwargs):
        self.host = host
        self.port = port
        self.timeout = timeout
        self.local_hostname = local_hostname
        self.options = kwargs
        self.events = []
        type(self).instance = self

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, traceback):
        return False

    def starttls(self, *, context):
        self.events.append(("starttls", context))

    def login(self, username: str, password: str):
        self.events.append(("login", username, password))

    def ehlo_or_helo_if_needed(self):
        self.events.append(("ehlo",))

    def auth(self, mechanism, authobject, *, initial_response_ok):
        username = authobject(b"Username:")
        password = authobject(b"Password:")
        self.events.append(("auth", mechanism, username, password, initial_response_ok))

    def send_message(self, message):
        self.events.append(("send", message))


class FakeSmtp(FakeSmtpBase):
    pass


class FakeSmtpSsl(FakeSmtpBase):
    pass


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
        self.assertEqual(smtp.local_hostname, "seecut.stormycry.cloud")
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

    def test_tencent_smtp_uses_verified_implicit_tls_and_explicit_login(self):
        with patch.dict(
            os.environ,
            {
                "SECUT_ENV": "test",
                "SECUT_EMAIL_PROVIDER": "smtp",
                "SECUT_EMAIL_FROM": "seecut@mail.stormycry.cloud",
                "SECUT_SMTP_HOST": "gz-smtp.qcloudmail.com",
                "SECUT_SMTP_PORT": "465",
                "SECUT_SMTP_USERNAME": "seecut-user",
                "SECUT_SMTP_PASSWORD": "seecut-password",
                "SECUT_SMTP_SSL": "true",
                "SECUT_SMTP_STARTTLS": "false",
                "SECUT_SMTP_AUTH_METHOD": "login",
            },
            clear=True,
        ):
            sender = EmailSender(Config.from_env())

        with patch("seecut_server.emailer.smtplib.SMTP_SSL", FakeSmtpSsl):
            sender.send_token("person@example.test", "reset_password", "reset-123456")

        smtp = FakeSmtpSsl.instance
        self.assertEqual(
            (smtp.host, smtp.port, smtp.timeout, smtp.local_hostname),
            ("gz-smtp.qcloudmail.com", 465, 15, "seecut.stormycry.cloud"),
        )
        context = smtp.options["context"]
        self.assertEqual(context.verify_mode, ssl.CERT_REQUIRED)
        self.assertTrue(context.check_hostname)
        self.assertEqual([event[0] for event in smtp.events], ["ehlo", "auth", "send"])
        self.assertEqual(
            smtp.events[1],
            ("auth", "LOGIN", "seecut-user", "seecut-password", False),
        )
        message = smtp.events[2][1]
        self.assertEqual(message["Subject"], "重置 SeeCut 密码")
        self.assertIn("重置密码验证码：reset-123456", message.get_content())

    def test_production_smtp_authentication_requires_tls(self):
        with patch.dict(
            os.environ,
            {
                "SECUT_ENV": "production",
                "SECUT_PUBLIC_BASE_URL": "https://seecut.stormycry.cloud",
                "SECUT_SIGNING_SECRET": "a-production-secret-with-32-characters",
                "SECUT_SMTP_USERNAME": "seecut-user",
                "SECUT_SMTP_SSL": "false",
                "SECUT_SMTP_STARTTLS": "false",
            },
            clear=True,
        ):
            with self.assertRaisesRegex(ConfigError, "requires TLS"):
                Config.from_env()


if __name__ == "__main__":
    unittest.main()
