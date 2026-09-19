from __future__ import annotations

import smtplib
from email.message import EmailMessage

from .config import Config


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
            with smtplib.SMTP(self.config.smtp_host, self.config.smtp_port, timeout=15) as client:
                if self.config.smtp_starttls:
                    client.starttls()
                if self.config.smtp_username:
                    client.login(self.config.smtp_username, self.config.smtp_password)
                client.send_message(message)
        except (OSError, smtplib.SMTPException) as exc:
            raise EmailDeliveryError("email delivery failed") from exc

