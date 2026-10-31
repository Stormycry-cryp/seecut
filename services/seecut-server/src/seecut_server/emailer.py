from __future__ import annotations

import datetime
import hashlib
import hmac
import json
import smtplib
import ssl
import time
import urllib.error
import urllib.request
from email.message import EmailMessage

from .config import Config
from .network import open_no_redirect, read_limited

SMTP_LOCAL_HOSTNAME = "seecut.stormycry.cloud"
TENCENT_SES_ENDPOINT = "https://ses.tencentcloudapi.com/"
TENCENT_SES_HOST = "ses.tencentcloudapi.com"
TENCENT_SES_SERVICE = "ses"
TENCENT_SES_VERSION = "2020-10-02"
TENCENT_SES_RESPONSE_LIMIT = 64 * 1024


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
        if purpose not in {"verify_email", "reset_password"}:
            raise EmailDeliveryError("unsupported email purpose")
        if self.config.email_provider == "tencent_ses":
            self._send_tencent_ses(email, purpose, token)
            return
        self._send_smtp(email, purpose, token)

    def _send_smtp(self, email: str, purpose: str, token: str) -> None:
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

    def _send_tencent_ses(self, email: str, purpose: str, token: str) -> None:
        subject = "验证你的 SeeCut 邮箱" if purpose == "verify_email" else "重置 SeeCut 密码"
        template_id = (
            self.config.tencent_ses_verify_template_id
            if purpose == "verify_email"
            else self.config.tencent_ses_reset_template_id
        )
        body = {
            "FromEmailAddress": self.config.email_from,
            "Destination": [email],
            "Subject": subject,
            "Template": {
                "TemplateID": template_id,
                "TemplateData": json.dumps({"code": token}, ensure_ascii=False),
            },
            "TriggerType": 1,
        }
        payload = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        timestamp = int(time.time())
        request = urllib.request.Request(
            TENCENT_SES_ENDPOINT,
            data=payload,
            method="POST",
            headers=self._tencent_ses_headers(payload, timestamp),
        )
        try:
            with open_no_redirect(request, timeout=15) as response:
                result = json.loads(read_limited(response, TENCENT_SES_RESPONSE_LIMIT).decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, ValueError, UnicodeDecodeError) as exc:
            raise EmailDeliveryError("email delivery failed") from exc
        response = result.get("Response") if isinstance(result, dict) else None
        if not isinstance(response, dict) or response.get("Error") or not response.get("MessageId"):
            raise EmailDeliveryError("email delivery failed")

    def _tencent_ses_headers(self, payload: bytes, timestamp: int) -> dict[str, str]:
        content_type = "application/json"
        canonical_headers = f"content-type:{content_type}\nhost:{TENCENT_SES_HOST}\n"
        signed_headers = "content-type;host"
        hashed_payload = hashlib.sha256(payload).hexdigest()
        canonical_request = (
            f"POST\n/\n\n{canonical_headers}\n{signed_headers}\n{hashed_payload}"
        )
        date = datetime.datetime.fromtimestamp(timestamp, datetime.timezone.utc).strftime("%Y-%m-%d")
        credential_scope = f"{date}/{TENCENT_SES_SERVICE}/tc3_request"
        string_to_sign = (
            "TC3-HMAC-SHA256\n"
            f"{timestamp}\n"
            f"{credential_scope}\n"
            f"{hashlib.sha256(canonical_request.encode('utf-8')).hexdigest()}"
        )
        secret_date = hmac.new(
            f"TC3{self.config.tencent_ses_secret_key}".encode("utf-8"),
            date.encode("utf-8"),
            hashlib.sha256,
        ).digest()
        secret_service = hmac.new(
            secret_date, TENCENT_SES_SERVICE.encode("utf-8"), hashlib.sha256
        ).digest()
        secret_signing = hmac.new(secret_service, b"tc3_request", hashlib.sha256).digest()
        signature = hmac.new(
            secret_signing, string_to_sign.encode("utf-8"), hashlib.sha256
        ).hexdigest()
        authorization = (
            "TC3-HMAC-SHA256 "
            f"Credential={self.config.tencent_ses_secret_id}/{credential_scope}, "
            f"SignedHeaders={signed_headers}, Signature={signature}"
        )
        return {
            "Authorization": authorization,
            "Content-Type": content_type,
            "Host": TENCENT_SES_HOST,
            "X-TC-Action": "SendEmail",
            "X-TC-Region": self.config.tencent_ses_region,
            "X-TC-Timestamp": str(timestamp),
            "X-TC-Version": TENCENT_SES_VERSION,
        }
