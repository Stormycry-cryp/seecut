"""Bounded public model data. Editor authority and provider routing stay elsewhere."""
from __future__ import annotations

import base64
import hashlib
import json
import math
import re

from .errors import ApiError

# Public transport vocabulary only. The local MCP host still authorizes every
# read/write against the current document, selection and native approval.
TOOLS = frozenset({
    "capabilities", "context", "project", "preview", "move_selected_image",
    "library_images", "library_assets", "insert_library_image", "transform_canvas_layer", "canvas_edit",
    "save_canvas_project", "export_canvas_png", "canvas_operation",
    "retry_canvas_registration", "clip_edit", "clip_operation",
    "flow_get_workspace", "flow_read_canvas", "flow_list_node_types", "flow_get_node_tools", "flow_get_node_config", "flow_get_task", "flow_get_operation",
    "flow_add_node", "flow_update_node", "flow_connect_nodes", "flow_delete_nodes", "flow_switch_canvas", "flow_execute_node",
})
RUNTIME_TOOLS = frozenset({"task_status", "task_checkpoint"})
LIVE_TOOLS = TOOLS | RUNTIME_TOOLS
# Retired names remain readable only inside stored conversation history.
HISTORICAL_TOOLS = LIVE_TOOLS | {"flow_read", "flow_write"}
IDENTITY = re.compile(r"[A-Za-z0-9_.:-]{1,128}\Z")


def invalid(message="Agent 请求格式无效"):
    raise ApiError(400, "ASSISTANT_INVALID_REQUEST", message)


def fields(value, required, optional=()):
    if not isinstance(value, dict) or not set(required) <= value.keys() or value.keys() - set(required) - set(optional):
        invalid()


def identity(value):
    if not isinstance(value, str) or not IDENTITY.fullmatch(value):
        invalid("Agent 请求标识无效")
    return value


def text(value, limit=131072):
    if not isinstance(value, str) or len(value.encode("utf-8")) > limit:
        invalid()
    return value


def integer(value, minimum=0, maximum=1_000_000):
    if type(value) is not int or not minimum <= value <= maximum:
        invalid()
    return value


def canonical(value):
    def visit(item, depth=0):
        if depth > 24:
            invalid()
        if isinstance(item, dict):
            for key, child in item.items():
                text(key, 1024)
                visit(child, depth + 1)
        elif isinstance(item, list):
            for child in item:
                visit(child, depth + 1)
        elif isinstance(item, float) and not math.isfinite(item):
            invalid()
        elif item is not None and not isinstance(item, (str, int, float, bool)):
            invalid()
    visit(value)
    data = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False)
    if len(data.encode("utf-8")) > 1024 * 1024:
        invalid("Agent 请求过大")
    return data


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def message(value, images=False, *, historical=False):
    tools = HISTORICAL_TOOLS if historical else LIVE_TOOLS
    fields(value, {"id", "role", "content"}, {"toolCalls", "toolCallId", "toolName", "isError"})
    identity(value["id"])
    if not isinstance(value["role"], str) or value["role"] not in {"user", "assistant", "tool"} or not isinstance(value["content"], list) or len(value["content"]) > 64:
        invalid()
    for part in value["content"]:
        if not isinstance(part, dict):
            invalid()
        if part.get("type") == "text":
            fields(part, {"type", "text"})
            text(part["text"])
        elif part.get("type") == "image" and images:
            fields(part, {"type", "data", "mimeType"})
            mime = part["mimeType"]
            if not isinstance(mime, str) or mime not in {"image/png", "image/jpeg", "image/webp"}:
                invalid()
            try:
                raw = base64.b64decode(text(part["data"], 512 * 1024), validate=True)
            except ValueError:
                invalid()
            signatures = {"image/png": raw.startswith(b"\x89PNG\r\n\x1a\n"), "image/jpeg": raw.startswith(b"\xff\xd8\xff"),
                          "image/webp": raw.startswith(b"RIFF") and raw[8:12] == b"WEBP"}
            if not signatures[mime]:
                invalid()
        else:
            invalid()
    if "toolCalls" in value:
        if value["role"] != "assistant" or not isinstance(value["toolCalls"], list) or len(value["toolCalls"]) > 16:
            invalid()
        ids = set()
        for call in value["toolCalls"]:
            fields(call, {"id", "name", "arguments"})
            identity(call["id"])
            if call["id"] in ids or not isinstance(call["name"], str) or call["name"] not in tools or not isinstance(call["arguments"], dict):
                invalid()
            ids.add(call["id"])
            canonical(call["arguments"])
    if value["role"] == "tool":
        identity(value.get("toolCallId"))
    for name in ("toolCallId", "toolName", "isError"):
        if name in value and value["role"] != "tool":
            invalid()
    if "toolName" in value and (not isinstance(value["toolName"], str) or value["toolName"] not in tools):
        invalid()
    if "isError" in value and type(value["isError"]) is not bool:
        invalid()


def model_request(value, config):
    fields(value, {"system", "messages", "tools", "maxOutputTokens"})
    canonical(value)
    text(value["system"])
    integer(value["maxOutputTokens"], 1, config.assistant_max_output_tokens)
    if not isinstance(value["messages"], list) or len(value["messages"]) > 256:
        invalid()
    for item in value["messages"]:
        message(item, config.assistant_images, historical=True)
    if not isinstance(value["tools"], list) or len(value["tools"]) > len(LIVE_TOOLS):
        invalid()
    names = set()
    for tool in value["tools"]:
        fields(tool, {"name", "description", "parameters"})
        if not isinstance(tool["name"], str) or tool["name"] not in LIVE_TOOLS or tool["name"] in names or not isinstance(tool["parameters"], dict):
            invalid()
        text(tool["description"], 8192)
        canonical(tool["parameters"])
        names.add(tool["name"])
    return value


def inference_body(body, config, creation=False):
    fields(body, {"client_request_id", "protocol_version", "local_session_id", "turn_id", "step_id", "budget_id", "request"},
           {"quote_id"} if creation else ())
    if type(body["protocol_version"]) is not int or body["protocol_version"] != 1:
        invalid()
    for key in ("client_request_id", "local_session_id", "turn_id", "step_id", "budget_id"):
        identity(body[key])
    if creation:
        identity(body.get("quote_id"))
    model_request(body["request"], config)
    return {key: value for key, value in body.items() if key != "quote_id"}


def usage(value):
    fields(value, {"input", "output"})
    integer(value["input"], 0, 10_000_000)
    integer(value["output"], 0, 10_000_000)
    return value


def model_event(value):
    canonical(value)
    if not isinstance(value, dict):
        invalid()
    kind = value.get("type")
    if kind == "text_delta":
        fields(value, {"type", "text"})
        text(value["text"], 16384)
    elif kind == "tool_delta":
        fields(value, {"type", "id", "text"})
        identity(value["id"])
        text(value["text"], 16384)
    elif kind == "done":
        fields(value, {"type", "message", "stopReason"}, {"usage"})
        message(value["message"])
        if value["message"]["role"] != "assistant" or not isinstance(value["stopReason"], str) or value["stopReason"] not in {"stop", "tools", "length"}:
            invalid()
        if bool(value["message"].get("toolCalls")) != (value["stopReason"] == "tools"):
            invalid()
        if "usage" in value:
            usage(value["usage"])
        if len(canonical(value).encode()) > 196608:
            invalid()
    else:
        invalid()
    return value
