from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path

from .assistant_billing import rate_denominator


class ConfigError(ValueError):
    pass


def _bool(name: str, default: bool) -> bool:
    value = os.getenv(name)
    if value is None:
        return default
    return value.lower() in {"1", "true", "yes", "on"}


def _optional_positive_int(name: str) -> int | None:
    value = os.getenv(name, "").strip()
    if not value:
        return None
    try:
        parsed = int(value)
    except ValueError as exc:
        raise ConfigError(f"{name} must be a positive integer") from exc
    if parsed <= 0:
        raise ConfigError(f"{name} must be a positive integer")
    return parsed


def _assistant_integer(name: str, default: int, maximum: int) -> int:
    raw = os.getenv(name, "").strip()
    if not raw:
        return default
    try:
        value = int(raw)
    except ValueError as exc:
        raise ConfigError(f"{name} must be a positive integer") from exc
    if not 1 <= value <= maximum:
        raise ConfigError(f"{name} must be between 1 and {maximum}")
    return value


def _assistant_rates():
    old = ('SECUT_ASSISTANT_INPUT_CREDITS_PER_1000', 'SECUT_ASSISTANT_OUTPUT_CREDITS_PER_1000')
    new = ('SECUT_ASSISTANT_INPUT_CREDITS_PER_1000000', 'SECUT_ASSISTANT_OUTPUT_CREDITS_PER_1000000')
    old_set = [bool(os.getenv(name, '').strip()) for name in old]
    new_set = [bool(os.getenv(name, '').strip()) for name in new]
    try:
        denominator = rate_denominator(int(os.getenv('SECUT_ASSISTANT_RATE_DENOMINATOR', '').strip() or '1000'))
    except ValueError as error:
        raise ConfigError('Invalid Agent rate denominator') from error
    if any(old_set) and any(new_set) or denominator == 1000 and any(new_set) or denominator == 1_000_000 and any(old_set):
        raise ConfigError('Agent rate units cannot be mixed')
    if any(new_set) and not all(new_set):
        raise ConfigError('Both million-token Agent rates are required')
    names = old if denominator == 1000 else new
    return denominator, *(_assistant_integer(name, 0, 1_000_000) for name in names)


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
    tencent_ses_region: str
    tencent_ses_secret_id: str
    tencent_ses_secret_key: str
    tencent_ses_verify_template_id: int | None
    tencent_ses_reset_template_id: int | None
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

    assistant_base_url: str = ""
    assistant_model: str = ""
    assistant_api_key: str = ""
    assistant_pricing_version: str = ""
    assistant_input_rate: int = 0
    assistant_output_rate: int = 0
    assistant_context_window: int = 32768
    assistant_max_output_tokens: int = 2048
    assistant_max_steps: int = 8
    assistant_max_turn_credits: int = 0
    assistant_images: bool = False
    assistant_models: tuple = ()
    assistant_rate_denominator: int = 1000
    efforts: tuple = ()

    def __post_init__(self):
        rate_denominator(self.assistant_rate_denominator)

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

        denominator, input_rate, output_rate = _assistant_rates()
        config = cls(
            assistant_base_url=os.getenv("SECUT_ASSISTANT_BASE_URL", "").rstrip("/"),
            assistant_model=os.getenv("SECUT_ASSISTANT_MODEL", ""),
            assistant_models=_assistant_model_catalog(),
            efforts=_effort_policies(),
            assistant_api_key=os.getenv("SECUT_ASSISTANT_API_KEY", ""),
            assistant_pricing_version=os.getenv("SECUT_ASSISTANT_PRICING_VERSION", ""),
            assistant_input_rate=input_rate,
            assistant_output_rate=output_rate,
            assistant_rate_denominator=denominator,
            assistant_context_window=_assistant_integer("SECUT_ASSISTANT_CONTEXT_WINDOW", 32768, 1_000_000),
            assistant_max_output_tokens=_assistant_integer("SECUT_ASSISTANT_MAX_OUTPUT_TOKENS", 2048, 131072),
            assistant_max_steps=_assistant_integer("SECUT_ASSISTANT_MAX_STEPS", 8, 8),
            assistant_max_turn_credits=_assistant_integer("SECUT_ASSISTANT_MAX_TURN_CREDITS", 0, 1_000_000_000),
            assistant_images=_bool("SECUT_ASSISTANT_IMAGES", False),
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
            tencent_ses_region=os.getenv("SECUT_TENCENT_SES_REGION", "ap-guangzhou").strip(),
            tencent_ses_secret_id=os.getenv("SECUT_TENCENT_SES_SECRET_ID", ""),
            tencent_ses_secret_key=os.getenv("SECUT_TENCENT_SES_SECRET_KEY", ""),
            tencent_ses_verify_template_id=_optional_positive_int(
                "SECUT_TENCENT_SES_VERIFY_TEMPLATE_ID"
            ),
            tencent_ses_reset_template_id=_optional_positive_int(
                "SECUT_TENCENT_SES_RESET_TEMPLATE_ID"
            ),
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
        from .efforts import validate_efforts
        try:
            validate_efforts(config)
        except ValueError as error:
            raise ConfigError('Invalid effort policies') from error
        return config

    def email_configured(self) -> bool:
        if self.email_provider == "disabled":
            return False
        if self.email_provider == "smtp":
            return bool(self.email_from and self.smtp_host)
        if self.email_provider == "tencent_ses":
            return bool(
                self.email_from
                and self.tencent_ses_region
                and self.tencent_ses_secret_id
                and self.tencent_ses_secret_key
                and self.tencent_ses_verify_template_id
                and self.tencent_ses_reset_template_id
            )
        return False

    def alipay_configured(self) -> bool:
        paths = [self.alipay_public_key_path, self.alipay_merchant_private_key_path]
        return bool(
            self.alipay_app_id
            and self.alipay_seller_id
            and self.alipay_notify_url
            and all(path and Path(path).is_file() for path in paths)
        )


def _effort_policies():
    from .efforts import parse_efforts
    try:
        return parse_efforts(os.getenv('SECUT_EFFORTS_JSON', '[]'))
    except ValueError as error:
        raise ConfigError('Invalid effort policies') from error


def _assistant_model_catalog():
    from .assistant_models import parse_models
    try:
        return parse_models(os.getenv("SECUT_ASSISTANT_MODELS_JSON", "[]"))
    except ValueError as error:
        raise ConfigError("Invalid Agent model catalog") from error
