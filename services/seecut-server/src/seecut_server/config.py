from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path


class ConfigError(ValueError):
    pass


def _bool(name: str, default: bool) -> bool:
    value = os.getenv(name)
    if value is None:
        return default
    return value.lower() in {"1", "true", "yes", "on"}


@dataclass(frozen=True)
class Config:
    env: str
    host: str
    port: int
    public_base_url: str
    database_path: Path
    storage_path: Path
    signing_secret: str
    session_ttl_seconds: int
    invite_ttl_seconds: int
    upload_ttl_seconds: int
    max_upload_bytes: int
    model_prices: dict[str, int]
    email_provider: str
    email_from: str
    smtp_host: str
    smtp_port: int
    smtp_username: str
    smtp_password: str
    smtp_ssl: bool
    smtp_starttls: bool
    smtp_auth_method: str
    expose_test_tokens: bool
    xiangxin_base_url: str
    xiangxin_api_key: str
    image2_base_url: str
    image2_model: str
    image2_api_key: str
    worker_interval_seconds: int
    provider_timeout_seconds: int
    max_provider_response_bytes: int
    max_reference_image_bytes: int
    auth_rate_limit_per_minute: int
    auth_rate_limit_window_seconds: int
    generation_output_ttl_seconds: int
    cleanup_interval_seconds: int
    provider_result_hosts: tuple[str, ...]
    alipay_app_id: str
    alipay_gateway: str
    alipay_public_key_path: str
    alipay_merchant_private_key_path: str
    alipay_notify_url: str
    alipay_seller_id: str

    @classmethod
    def from_env(cls) -> "Config":
        try:
            prices_raw = json.loads(os.getenv("SECUT_MODEL_PRICES_JSON", "{}"))
        except json.JSONDecodeError as exc:
            raise ConfigError("SECUT_MODEL_PRICES_JSON must be valid JSON") from exc
        if not isinstance(prices_raw, dict) or any(
            not isinstance(key, str) or not isinstance(value, int) or value < 0
            for key, value in prices_raw.items()
        ):
            raise ConfigError("SECUT_MODEL_PRICES_JSON must map model names to non-negative integers")

        env = os.getenv("SECUT_ENV", "development")
        public_base_url = os.getenv("SECUT_PUBLIC_BASE_URL", "http://127.0.0.1:8787").rstrip("/")
        secret = os.getenv("SECUT_SIGNING_SECRET", "")
        image2_model = os.getenv("SECUT_IMAGE2_MODEL", "gpt-image-2.5-flare").strip()
        if not image2_model:
            raise ConfigError("SECUT_IMAGE2_MODEL must not be empty")
        if env not in {"development", "test"} and len(secret) < 32:
            raise ConfigError("SECUT_SIGNING_SECRET must contain at least 32 characters")
        if env not in {"development", "test"} and not public_base_url.startswith("https://"):
            raise ConfigError("SECUT_PUBLIC_BASE_URL must use HTTPS outside development and test")
        if not secret:
            secret = "development-only-signing-secret-change-me"

        smtp_ssl = _bool("SECUT_SMTP_SSL", False)
        smtp_starttls = _bool("SECUT_SMTP_STARTTLS", True)
        smtp_username = os.getenv("SECUT_SMTP_USERNAME", "")
        smtp_auth_method = os.getenv("SECUT_SMTP_AUTH_METHOD", "auto").strip().lower()
        if smtp_ssl and smtp_starttls:
            raise ConfigError("SECUT_SMTP_SSL and SECUT_SMTP_STARTTLS cannot both be enabled")
        if smtp_auth_method not in {"auto", "login"}:
            raise ConfigError("SECUT_SMTP_AUTH_METHOD must be auto or login")
        if env not in {"development", "test"} and smtp_username and not (
            smtp_ssl or smtp_starttls
        ):
            raise ConfigError("SMTP authentication requires TLS outside development and test")

        return cls(
            env=env,
            host=os.getenv("SECUT_HOST", "127.0.0.1"),
            port=int(os.getenv("SECUT_PORT", "8787")),
            public_base_url=public_base_url,
            database_path=Path(os.getenv("SECUT_DATABASE_PATH", "./data/seecut.sqlite3")),
            storage_path=Path(os.getenv("SECUT_STORAGE_PATH", "./data/storage")),
            signing_secret=secret,
            session_ttl_seconds=int(os.getenv("SECUT_SESSION_TTL_SECONDS", "2592000")),
            invite_ttl_seconds=int(os.getenv("SECUT_INVITE_TTL_SECONDS", "86400")),
            upload_ttl_seconds=int(os.getenv("SECUT_UPLOAD_TTL_SECONDS", "3600")),
            max_upload_bytes=int(os.getenv("SECUT_MAX_UPLOAD_BYTES", "536870912")),
            model_prices=dict(prices_raw),
            email_provider=os.getenv("SECUT_EMAIL_PROVIDER", "disabled"),
            email_from=os.getenv("SECUT_EMAIL_FROM", ""),
            smtp_host=os.getenv("SECUT_SMTP_HOST", ""),
            smtp_port=int(os.getenv("SECUT_SMTP_PORT", "587")),
            smtp_username=smtp_username,
            smtp_password=os.getenv("SECUT_SMTP_PASSWORD", ""),
            smtp_ssl=smtp_ssl,
            smtp_starttls=smtp_starttls,
            smtp_auth_method=smtp_auth_method,
            expose_test_tokens=_bool("SECUT_EXPOSE_TEST_TOKENS", False),
            xiangxin_base_url=os.getenv("SECUT_XIANGXIN_BASE_URL", "https://xiangxinai123.xyz").rstrip("/"),
            xiangxin_api_key=os.getenv("SECUT_XIANGXIN_API_KEY", ""),
            image2_base_url="https://qwe.g-aisc.com/v1",
            image2_model=image2_model,
            image2_api_key=os.getenv("SECUT_IMAGE2_API_KEY", ""),
            worker_interval_seconds=int(os.getenv("SECUT_WORKER_INTERVAL_SECONDS", "2")),
            provider_timeout_seconds=int(os.getenv("SECUT_PROVIDER_TIMEOUT_SECONDS", "600")),
            max_provider_response_bytes=int(
                os.getenv("SECUT_MAX_PROVIDER_RESPONSE_BYTES", str(32 * 1024 * 1024))
            ),
            max_reference_image_bytes=int(
                os.getenv("SECUT_MAX_REFERENCE_IMAGE_BYTES", str(20 * 1024 * 1024))
            ),
            auth_rate_limit_per_minute=int(os.getenv("SECUT_AUTH_RATE_LIMIT_PER_MINUTE", "30")),
            auth_rate_limit_window_seconds=int(os.getenv("SECUT_AUTH_RATE_LIMIT_WINDOW_SECONDS", "60")),
            generation_output_ttl_seconds=int(
                os.getenv("SECUT_GENERATION_OUTPUT_TTL_SECONDS", "86400")
            ),
            cleanup_interval_seconds=int(os.getenv("SECUT_CLEANUP_INTERVAL_SECONDS", "60")),
            provider_result_hosts=tuple(
                value.strip().lower()
                for value in os.getenv(
                    "SECUT_PROVIDER_RESULT_HOSTS", "xiangxinai123.xyz,qwe.g-aisc.com"
                ).split(",")
                if value.strip()
            ),
            alipay_app_id=os.getenv("SECUT_ALIPAY_APP_ID", ""),
            alipay_gateway=os.getenv("SECUT_ALIPAY_GATEWAY", "https://openapi.alipay.com/gateway.do"),
            alipay_public_key_path=os.getenv("SECUT_ALIPAY_PUBLIC_KEY_PATH", ""),
            alipay_merchant_private_key_path=os.getenv("SECUT_ALIPAY_MERCHANT_PRIVATE_KEY_PATH", ""),
            alipay_notify_url=os.getenv("SECUT_ALIPAY_NOTIFY_URL", ""),
            alipay_seller_id=os.getenv("SECUT_ALIPAY_SELLER_ID", ""),
        )

    def email_configured(self) -> bool:
        if self.email_provider == "disabled":
            return False
        if self.email_provider != "smtp":
            return False
        return bool(self.email_from and self.smtp_host)

    def alipay_configured(self) -> bool:
        paths = [self.alipay_public_key_path, self.alipay_merchant_private_key_path]
        return bool(
            self.alipay_app_id
            and self.alipay_seller_id
            and self.alipay_notify_url
            and all(path and Path(path).is_file() for path in paths)
        )
