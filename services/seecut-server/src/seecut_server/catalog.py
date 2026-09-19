from __future__ import annotations

from copy import deepcopy
from typing import Any

from .errors import ApiError


CATALOG_VERSION = "2026-09-19.3"

MODELS: list[dict[str, Any]] = [
    {
        "id": "gpt-image-2.5-flare",
        "kind": "image",
        "provider": "image2",
        "display_name": "Flare",
        "operations": ["generate", "edit"],
        "parameters": {
            "prompt": {"type": "string", "required": True, "max_length": 12000},
            "size": {
                "type": "enum",
                "required": True,
                "default": "auto",
                "values": ["auto", "1024x1024", "1536x1024", "1024x1536"],
            },
            "quality": {
                "type": "enum",
                "required": True,
                "default": "high",
                "values": ["high"],
            },
            "reference_asset_ids": {
                "type": "asset_list",
                "required_for": ["edit"],
                "min_items": 1,
                "max_items": 4,
                "accepted_media": ["image/*"],
            },
        },
        "fixed_parameters": {"n": 1, "output_format": "png"},
        "billing_dimensions": ["operation", "size", "quality"],
    },
    {
        "id": "sd_2.0_mini_special",
        "kind": "video",
        "provider": "xiangxin",
        "display_name": "Seedance 2.0 Mini 特价版",
        "operations": ["generate"],
        "parameters": {
            "prompt": {"type": "string", "required": True, "max_length": 12000},
            "resolution": {
                "type": "enum",
                "required": True,
                "default": "720p",
                "values": ["720p"],
            },
            "duration": {
                "type": "enum",
                "value_type": "integer",
                "required": True,
                "default": 5,
                "values": list(range(4, 16)),
            },
            "aspect_ratio": {
                "type": "enum",
                "required": True,
                "values": ["16:9", "9:16", "1:1", "4:3", "3:4", "21:9", "adaptive"],
            },
            "generate_audio": {
                "type": "boolean",
                "required": True,
                "default": True,
            },
            "reference_asset_ids": {
                "type": "asset_list",
                "required": False,
                "min_items": 0,
                "max_items": 9,
                "accepted_media": ["image/*", "video/*", "audio/*"],
                "max_per_kind": {"image": 9, "video": 3, "audio": 3},
                "duration_seconds": {
                    "video": {"min_per_item": 2, "max_per_item": 15, "max_total": 15},
                    "audio": {"min_per_item": 2, "max_per_item": 15, "max_total": 15},
                },
                "requires_visual_when_audio": True,
            },
        },
        "fixed_parameters": {},
        "billing_dimensions": ["resolution", "duration", "aspect_ratio", "reference_count"],
    },
]

_BY_ID = {item["id"]: item for item in MODELS}


def _model_for_request(kind: str, model_id: Any, image_model_id: str | None) -> dict[str, Any] | None:
    if kind == "image" and image_model_id:
        template = next((item for item in MODELS if item["kind"] == "image"), None)
        if template and model_id == image_model_id:
            model = deepcopy(template)
            model["id"] = image_model_id
            return model
        return None
    model = _BY_ID.get(model_id)
    return deepcopy(model) if model else None


def public_catalog(image_model_id: str | None = None) -> dict[str, Any]:
    models = deepcopy(MODELS)
    if image_model_id:
        for model in models:
            if model["kind"] == "image":
                model["id"] = image_model_id
                model["display_name"] = "Flare" if image_model_id == "gpt-image-2.5-flare" else image_model_id
    return {"catalog_version": CATALOG_VERSION, "models": models}


def validate_request(
    kind: str, body: dict[str, Any], image_model_id: str | None = None
) -> tuple[dict[str, Any], dict[str, Any]]:
    model_id = body.get("model")
    model = _model_for_request(kind, model_id, image_model_id)
    if not model or model["kind"] != kind:
        raise ApiError(422, "UNSUPPORTED_MODEL", "该生成模型未开放")
    operation = body.get("operation", "generate")
    if operation not in model["operations"]:
        raise ApiError(422, "UNSUPPORTED_OPERATION", "该模型不支持此操作")
    allowed = {"model", "operation", *model["parameters"].keys()}
    unknown = sorted(set(body) - allowed)
    if unknown:
        raise ApiError(422, "UNSUPPORTED_PARAMETERS", "请求包含未开放参数", {"parameters": unknown})

    normalized: dict[str, Any] = {"model": model_id, "operation": operation}
    for name, spec in model["parameters"].items():
        value = body.get(name, spec.get("default"))
        required = spec.get("required") or operation in spec.get("required_for", [])
        if required and (value is None or value == "" or value == []):
            raise ApiError(422, "MISSING_PARAMETER", f"缺少参数：{name}", {"parameter": name})
        if value is None:
            continue
        if spec["type"] == "string":
            if not isinstance(value, str) or not value.strip() or len(value) > spec["max_length"]:
                raise ApiError(422, "INVALID_PARAMETER", f"参数无效：{name}", {"parameter": name})
            value = value.strip()
        elif spec["type"] == "boolean":
            if not isinstance(value, bool):
                raise ApiError(422, "INVALID_PARAMETER", f"参数无效：{name}", {"parameter": name})
        elif spec["type"] == "enum":
            if (
                spec.get("value_type") == "integer" and type(value) is not int
            ) or value not in spec["values"]:
                raise ApiError(422, "INVALID_PARAMETER", f"参数无效：{name}", {"parameter": name})
        elif spec["type"] == "asset_list":
            if not isinstance(value, list) or not all(isinstance(item, str) and item for item in value):
                raise ApiError(422, "INVALID_PARAMETER", f"参数无效：{name}", {"parameter": name})
            if not spec["min_items"] <= len(value) <= spec["max_items"]:
                raise ApiError(422, "INVALID_PARAMETER", f"参数数量无效：{name}", {"parameter": name})
            if len(set(value)) != len(value):
                raise ApiError(422, "INVALID_PARAMETER", f"参数包含重复素材：{name}", {"parameter": name})
        normalized[name] = value
    normalized.update(model["fixed_parameters"])
    return model, normalized


def billing_key(model: dict[str, Any], normalized: dict[str, Any]) -> str:
    dimensions: list[str] = []
    for name in model["billing_dimensions"]:
        # SeeCut currently prices video references as absent/present. Keep the
        # established key name while collapsing 1-9 references into the
        # existing reference_count=1 tier.
        value = (
            int(bool(normalized.get("reference_asset_ids", [])))
            if name == "reference_count"
            else normalized.get(name)
        )
        dimensions.append(f"{name}={value}")
    return ":".join([model["provider"], model["id"], *dimensions])
