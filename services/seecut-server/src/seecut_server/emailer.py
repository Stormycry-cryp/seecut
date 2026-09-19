from __future__ import annotations

import smtplib
import ssl
from email.message import EmailMessage

from .config import Config

SMTP_LOCAL_HOSTNAME = "seecut.stormycry.cloud"


class EmailDeliveryError(RuntimeError):
    pass


class EmailSender:
    def __init__(self, config: Config):
        self.config = config

    @property
    def configured(self) -> bool:
        return self.config.email_configured()

    def send_token(self, email: str, purpose: str, token: str) -> None:
        if not self.config.email_configured():
            raise EmailDeliveryError("email provider is not configured")
        subject = "验证你的 SeeCut 邮箱" if purpose == "verify_email" else "重置 SeeCut 密码"
        action = "验证邮箱" if purpose == "verify_email" else "重置密码"
        message = EmailMessage()
        message["From"] = self.config.email_from
        message["To"] = email
        message["Subject"] = subject
        message.set_content(f"{action}验证码：{token}\n\n该验证码将在 30 分钟后失效。")
        try:
            context = ssl.create_default_context()
            smtp_class = smtplib.SMTP_SSL if self.config.smtp_ssl else smtplib.SMTP
            smtp_options = {
                "timeout": 15,
                "local_hostname": SMTP_LOCAL_HOSTNAME,
            }
            if self.config.smtp_ssl:
                smtp_options["context"] = context
            with smtp_class(self.config.smtp_host, self.config.smtp_port, **smtp_options) as client:
                if self.config.smtp_starttls:
                    client.starttls(context=context)
                if self.config.smtp_username:
                    if self.config.smtp_auth_method == "login":
                        client.ehlo_or_helo_if_needed()
                        responses = iter((self.config.smtp_username, self.config.smtp_password))
                        client.auth("LOGIN", lambda _challenge: next(responses), initial_response_ok=False)
                    else:
                        client.login(self.config.smtp_username, self.config.smtp_password)
                client.send_message(message)
        except (OSError, smtplib.SMTPException) as exc:
            raise EmailDeliveryError("email delivery failed") from exc
