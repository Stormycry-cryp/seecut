from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any

from .config import Config
from .network import open_no_redirect, read_limited


class XiangxinError(RuntimeError):
    def __init__(self, code: str, message: str, retryable: bool = False):
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable


class XiangxinClient:
    def __init__(self, config: Config):
        self.config = config

    @property
    def configured(self) -> bool:
        return bool(self.config.xiangxin_api_key)

    def models(self) -> dict[str, Any]:
        return self._request("GET", "/v1/models")

    def create_image(self, body: dict[str, Any]) -> dict[str, Any]:
        return self._request("POST", "/v1/images/generations", body)

    def create_video(self, body: dict[str, Any]) -> dict[str, Any]:
        return self._request("POST", "/v1/videos", body)

    def get_video(self, task_id: str) -> dict[str, Any]:
        return self._request("GET", f"/v1/videos/{task_id}")

    def register_asset(self, source_url: str, asset_type: str) -> dict[str, Any]:
        if asset_type not in {"Image", "Video", "Audio"}:
            raise ValueError("asset_type must be Image, Video or Audio")
        return self._request(
            "POST", "/v1/videos/assets", {"assetType": asset_type, "url": source_url}
        )

    def _request(
        self, method: str, path: str, body: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        if not self.config.xiangxin_api_key:
            raise XiangxinError("XIANGXIN_NOT_CONFIGURED", "Xiangxin API is not configured")
        data = None if body is None else json.dumps(body).encode("utf-8")
        request = urllib.request.Request(
            f"{self.config.xiangxin_base_url}{path}",
            data=data,
            method=method,
            headers={
                "Authorization": f"Bearer {self.config.xiangxin_api_key}",
                "Content-Type": "application/json",
                "Accept": "application/json",
            },
        )
        try:
            with open_no_redirect(request, timeout=60) as response:
                payload = json.loads(
                    read_limited(response, self.config.max_provider_response_bytes).decode("utf-8")
                )
                if not isinstance(payload, dict):
                    raise XiangxinError("XIANGXIN_INVALID_RESPONSE", "Xiangxin returned an invalid response")
                return payload
        except urllib.error.HTTPError as exc:
            message = "Xiangxin request failed"
            try:
                payload = json.loads(exc.read(self.config.max_provider_response_bytes).decode("utf-8"))
                message = str(payload.get("error") or payload.get("message") or message)
            except (ValueError, UnicodeDecodeError):
                pass
            raise XiangxinError("XIANGXIN_HTTP_ERROR", message, retryable=exc.code >= 500) from exc
        except (urllib.error.URLError, TimeoutError) as exc:
            raise XiangxinError(
                "XIANGXIN_UNAVAILABLE", "Xiangxin is temporarily unavailable", retryable=True
            ) from exc
        except (ValueError, UnicodeDecodeError) as exc:
            raise XiangxinError("XIANGXIN_INVALID_RESPONSE", "Xiangxin returned an invalid response") from exc
