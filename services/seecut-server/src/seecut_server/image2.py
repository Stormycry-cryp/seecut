from __future__ import annotations

import base64
import binascii
import json
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any

from .config import Config
from .network import open_no_redirect, read_limited


class Image2Error(RuntimeError):
    def __init__(self, code: str, message: str, retryable: bool = False, upstream_task_id: str | None = None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable
        self.upstream_task_id = upstream_task_id


class Image2Client:
    def __init__(self, config: Config):
        self.config = config

    @property
    def configured(self) -> bool:
        return bool(self.config.image2_api_key)

    @property
    def model(self) -> str:
        return self.config.image2_model

    def create(self, body: dict[str, Any]) -> list[bytes]:
        if not self.config.image2_api_key:
            raise Image2Error("IMAGE2_NOT_CONFIGURED", "Image2 API is not configured")
        if body.get("model") != self.config.image2_model:
            raise Image2Error("IMAGE2_MODEL_MISMATCH", "Image2 请求模型与服务器配置不一致")
        request = urllib.request.Request(
            f"{self.config.image2_base_url}/images/generations",
            data=json.dumps(body, ensure_ascii=False).encode("utf-8"),
            method="POST",
            headers={
                "Authorization": f"Bearer {self.config.image2_api_key}",
                "Content-Type": "application/json",
                "Accept": "application/json",
            },
        )
        try:
            with open_no_redirect(request, timeout=self.config.provider_timeout_seconds) as response:
                payload = json.loads(
                    read_limited(response, self.config.max_provider_response_bytes).decode("utf-8")
                )
        except urllib.error.HTTPError as exc:
            raise Image2Error("IMAGE2_HTTP_ERROR", "Image2 generation failed", exc.code >= 500) from exc
        except (urllib.error.URLError, TimeoutError) as exc:
            raise Image2Error("IMAGE2_UNAVAILABLE", "Image2 is temporarily unavailable", True) from exc
        except (ValueError, UnicodeDecodeError) as exc:
            raise Image2Error("IMAGE2_INVALID_RESPONSE", "Image2 returned invalid JSON") from exc
        if not isinstance(payload, dict):
            raise Image2Error("IMAGE2_INVALID_RESPONSE", "Image2 response did not include image data")
        if not isinstance(payload.get("data"), list):
            raise self._pending_or_invalid(payload)
        outputs: list[bytes] = []
        for item in payload["data"]:
            if not isinstance(item, dict) or not isinstance(item.get("b64_json"), str):
                raise self._pending_or_invalid(payload)
            try:
                decoded = base64.b64decode(item["b64_json"], validate=True)
            except (ValueError, binascii.Error) as exc:
                raise Image2Error("IMAGE2_INVALID_IMAGE", "Image2 returned invalid image data") from exc
            if not self._looks_like_image(decoded):
                raise Image2Error("IMAGE2_INVALID_IMAGE", "Image2 result is not a supported image")
            outputs.append(decoded)
        if not outputs:
            raise Image2Error("IMAGE2_INVALID_RESPONSE", "Image2 returned no images")
        return outputs

    def edit(self, body: dict[str, Any], images: list[tuple[Path, str]]) -> list[bytes]:
        if not self.config.image2_api_key:
            raise Image2Error("IMAGE2_NOT_CONFIGURED", "Image2 API is not configured")
        if body.get("model") != self.config.image2_model:
            raise Image2Error("IMAGE2_MODEL_MISMATCH", "Image2 请求模型与服务器配置不一致")
        boundary = "seecut-" + uuid.uuid4().hex
        multipart = bytearray()
        for key, value in body.items():
            multipart.extend(f"--{boundary}\r\n".encode())
            multipart.extend(f'Content-Disposition: form-data; name="{key}"\r\n\r\n'.encode())
            multipart.extend(str(value).encode("utf-8"))
            multipart.extend(b"\r\n")
        for index, (path, content_type) in enumerate(images):
            if not content_type.startswith("image/") or path.stat().st_size > self.config.max_reference_image_bytes:
                raise Image2Error("REFERENCE_IMAGE_TOO_LARGE", "参考图片单文件不能超过 20MB")
            multipart.extend(f"--{boundary}\r\n".encode())
            multipart.extend(
                f'Content-Disposition: form-data; name="image"; filename="reference-{index}"\r\n'.encode()
            )
            multipart.extend(f"Content-Type: {content_type}\r\n\r\n".encode())
            with path.open("rb") as source:
                while chunk := source.read(1024 * 1024):
                    multipart.extend(chunk)
            multipart.extend(b"\r\n")
        multipart.extend(f"--{boundary}--\r\n".encode())
        request = urllib.request.Request(
            f"{self.config.image2_base_url}/images/edits",
            data=multipart,
            method="POST",
            headers={
                "Authorization": f"Bearer {self.config.image2_api_key}",
                "Content-Type": f"multipart/form-data; boundary={boundary}",
                "Accept": "application/json",
            },
        )
        try:
            with open_no_redirect(request, timeout=self.config.provider_timeout_seconds) as response:
                payload = json.loads(
                    read_limited(response, self.config.max_provider_response_bytes).decode("utf-8")
                )
        except urllib.error.HTTPError as exc:
            raise Image2Error("IMAGE2_HTTP_ERROR", "Image2 edit failed", exc.code >= 500) from exc
        except (urllib.error.URLError, TimeoutError) as exc:
            raise Image2Error("IMAGE2_UNAVAILABLE", "Image2 is temporarily unavailable", True) from exc
        except (ValueError, UnicodeDecodeError) as exc:
            raise Image2Error("IMAGE2_INVALID_RESPONSE", "Image2 returned invalid JSON") from exc
        return self._decode_outputs(payload)

    def _decode_outputs(self, payload: Any) -> list[bytes]:
        if not isinstance(payload, dict) or not isinstance(payload.get("data"), list):
            if isinstance(payload, dict):
                raise self._pending_or_invalid(payload)
            raise Image2Error("IMAGE2_INVALID_RESPONSE", "Image2 response did not include image data")
        outputs: list[bytes] = []
        for item in payload["data"]:
            if not isinstance(item, dict) or not isinstance(item.get("b64_json"), str):
                raise self._pending_or_invalid(payload)
            try:
                decoded = base64.b64decode(item["b64_json"], validate=True)
            except (ValueError, binascii.Error) as exc:
                raise Image2Error("IMAGE2_INVALID_IMAGE", "Image2 returned invalid image data") from exc
            if not self._looks_like_image(decoded):
                raise Image2Error("IMAGE2_INVALID_IMAGE", "Image2 result is not a supported image")
            outputs.append(decoded)
        if not outputs:
            raise Image2Error("IMAGE2_INVALID_RESPONSE", "Image2 returned no images")
        return outputs

    @staticmethod
    def _pending_or_invalid(payload: dict[str, Any]) -> Image2Error:
        data = payload.get("data") if isinstance(payload.get("data"), dict) else {}
        task_id = payload.get("id") or payload.get("task_id") or data.get("id") or data.get("task_id")
        status = str(payload.get("status") or data.get("status") or "").lower()
        if task_id or status in {"queued", "processing", "pending", "running"}:
            return Image2Error(
                "IMAGE2_ASYNC_PENDING",
                "Image2 任务已提交，结果仍在处理中",
                True,
                str(task_id) if task_id else None,
            )
        return Image2Error("IMAGE2_INVALID_RESPONSE", "Image2 response did not include completed image data")

    @staticmethod
    def _looks_like_image(data: bytes) -> bool:
        return (
            data.startswith(b"\x89PNG\r\n\x1a\n")
            or data.startswith(b"\xff\xd8\xff")
            or data.startswith(b"RIFF") and data[8:12] == b"WEBP"
        )
