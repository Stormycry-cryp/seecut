from __future__ import annotations

import hashlib
import hmac
import json
import os
import sqlite3
import threading
import time
import uuid
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable

PROTOCOL_VERSION = "2025-06-18"
MAX_BODY_BYTES = 64 * 1024
MAX_RESPONSE_BYTES = 256 * 1024
MAX_RANGE_DAYS = 90


class McpError(Exception):
    def __init__(self, code: int, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


@dataclass(frozen=True)
class McpConfig:
    host: str
    port: int
    snapshot_path: Path
    token_file: Path
    audit_log_path: Path
    key_id: str
    requests_per_minute: int
    query_timeout_ms: int
    max_snapshot_age_seconds: int

    @classmethod
    def from_env(cls) -> "McpConfig":
        token_file = Path(os.getenv("SECUT_MCP_TOKEN_FILE", ""))
        if not str(token_file) or not token_file.is_file():
            raise ValueError("SECUT_MCP_TOKEN_FILE must point to a readable token file")
        mode = token_file.stat().st_mode & 0o777
        if mode & 0o077:
            raise ValueError("SECUT_MCP_TOKEN_FILE must not be accessible by group or others")
        return cls(
            host=os.getenv("SECUT_MCP_HOST", "127.0.0.1"),
            port=int(os.getenv("SECUT_MCP_PORT", "8799")),
            snapshot_path=Path(os.getenv("SECUT_MCP_SNAPSHOT_PATH", "/var/lib/seecut/mcp/seecut-readonly.sqlite3")),
            token_file=token_file,
            audit_log_path=Path(os.getenv("SECUT_MCP_AUDIT_LOG", "/var/log/seecut/mcp-audit.jsonl")),
            key_id=os.getenv("SECUT_MCP_KEY_ID", "chatgpt-production-readonly"),
            requests_per_minute=int(os.getenv("SECUT_MCP_REQUESTS_PER_MINUTE", "30")),
            query_timeout_ms=int(os.getenv("SECUT_MCP_QUERY_TIMEOUT_MS", "1500")),
            max_snapshot_age_seconds=int(os.getenv("SECUT_MCP_MAX_SNAPSHOT_AGE_SECONDS", "900")),
        )

    def token(self) -> str:
        token = self.token_file.read_text(encoding="utf-8").strip()
        if len(token) < 32:
            raise ValueError("MCP token must contain at least 32 characters")
        return token


def _tool(name: str, description: str, properties: dict[str, Any], required: list[str] | None = None) -> dict[str, Any]:
    return {
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required or [],
            "additionalProperties": False,
        },
    }


TOOLS = [
    _tool("get_system_summary", "Return aggregate account, team, task and asset counts from the read-only snapshot.", {}),
    _tool("get_generation_summary", "Return generation counts grouped by status, kind and provider for a bounded time range.", {"days": {"type": "integer", "minimum": 1, "maximum": MAX_RANGE_DAYS, "default": 7}}),
    _tool("get_asset_summary", "Return aggregate team asset counts, sizes and media types for a bounded time range.", {"days": {"type": "integer", "minimum": 1, "maximum": MAX_RANGE_DAYS, "default": 30}}),
    _tool("get_commerce_summary", "Return aggregate order, credit and ledger totals for a bounded time range. No user or payment identifiers are returned.", {"days": {"type": "integer", "minimum": 1, "maximum": MAX_RANGE_DAYS, "default": 30}}),
    _tool("get_generation_task_status", "Return safe operational metadata for one exact generation task ID. Prompt, request, upstream response, user ID and object keys are excluded.", {"task_id": {"type": "string", "minLength": 8, "maxLength": 128}}, ["task_id"]),
]


class SnapshotQueries:
    def __init__(self, config: McpConfig):
        self.config = config

    def _connect(self) -> sqlite3.Connection:
        path = self.config.snapshot_path.resolve()
        if not path.is_file():
            raise McpError(-32010, "data snapshot is unavailable")
        connection = sqlite3.connect(f"file:{path.as_posix()}?mode=ro&immutable=1", uri=True, timeout=0.1)
        connection.row_factory = sqlite3.Row
        deadline = time.monotonic() + self.config.query_timeout_ms / 1000
        connection.set_progress_handler(lambda: 1 if time.monotonic() > deadline else 0, 1000)
        connection.execute("PRAGMA query_only=ON")
        return connection

    def _snapshot(self, connection: sqlite3.Connection) -> dict[str, Any]:
        row = connection.execute("SELECT created_at,source_schema_version FROM mcp_snapshot_metadata LIMIT 1").fetchone()
        if row is None:
            raise McpError(-32010, "snapshot metadata is unavailable")
        age = max(0, int(time.time()) - row["created_at"])
        if age > self.config.max_snapshot_age_seconds:
            raise McpError(-32011, "data snapshot is stale")
        return {"created_at": row["created_at"], "age_seconds": age, "schema_version": row["source_schema_version"]}

    @staticmethod
    def _days(arguments: dict[str, Any], default: int) -> int:
        if set(arguments) - {"days"}:
            raise McpError(-32602, "days is the only accepted argument")
        value = arguments.get("days", default)
        if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= MAX_RANGE_DAYS:
            raise McpError(-32602, f"days must be an integer from 1 to {MAX_RANGE_DAYS}")
        return value

    def call(self, name: str, arguments: dict[str, Any]) -> tuple[dict[str, Any], int]:
        handlers: dict[str, Callable[[sqlite3.Connection, dict[str, Any]], tuple[dict[str, Any], int]]] = {
            "get_system_summary": self._system,
            "get_generation_summary": self._generation,
            "get_asset_summary": self._assets,
            "get_commerce_summary": self._commerce,
            "get_generation_task_status": self._task,
        }
        if name not in handlers:
            raise McpError(-32602, "unknown tool")
        if not isinstance(arguments, dict):
            raise McpError(-32602, "arguments must be an object")
        try:
            with self._connect() as connection:
                snapshot = self._snapshot(connection)
                result, rows = handlers[name](connection, arguments)
                result["snapshot"] = snapshot
                return result, rows
        except sqlite3.OperationalError as exc:
            if "interrupted" in str(exc).lower():
                raise McpError(-32012, "query time limit exceeded") from None
            raise McpError(-32010, "data snapshot query failed") from None

    def _system(self, db: sqlite3.Connection, arguments: dict[str, Any]) -> tuple[dict[str, Any], int]:
        if arguments:
            raise McpError(-32602, "this tool accepts no arguments")
        counts = {}
        for key, table, where in (
            ("users", "users", ""), ("teams", "teams", ""),
            ("active_team_assets", "team_assets", " WHERE deleted_at IS NULL"),
            ("generation_tasks", "generation_tasks", ""), ("orders", "orders", ""),
        ):
            counts[key] = db.execute(f"SELECT COUNT(*) FROM {table}{where}").fetchone()[0]
        return {"counts": counts}, len(counts)

    def _generation(self, db: sqlite3.Connection, arguments: dict[str, Any]) -> tuple[dict[str, Any], int]:
        days = self._days(arguments, 7)
        since = int(time.time()) - days * 86400
        rows = db.execute("SELECT status,kind,provider,COUNT(*) count,COALESCE(SUM(quoted_credits),0) quoted_credits FROM generation_tasks WHERE created_at>=? GROUP BY status,kind,provider ORDER BY count DESC", (since,)).fetchall()
        return {"days": days, "groups": [dict(row) for row in rows]}, len(rows)

    def _assets(self, db: sqlite3.Connection, arguments: dict[str, Any]) -> tuple[dict[str, Any], int]:
        days = self._days(arguments, 30)
        since = int(time.time()) - days * 86400
        rows = db.execute("SELECT CASE WHEN content_type LIKE 'image/%' THEN 'image' WHEN content_type LIKE 'video/%' THEN 'video' WHEN content_type LIKE 'audio/%' THEN 'audio' ELSE 'other' END media_type,COUNT(*) count,COALESCE(SUM(size_bytes),0) size_bytes FROM team_assets WHERE created_at>=? AND deleted_at IS NULL GROUP BY media_type ORDER BY count DESC", (since,)).fetchall()
        return {"days": days, "groups": [dict(row) for row in rows]}, len(rows)

    def _commerce(self, db: sqlite3.Connection, arguments: dict[str, Any]) -> tuple[dict[str, Any], int]:
        days = self._days(arguments, 30)
        since = int(time.time()) - days * 86400
        order_rows = db.execute("SELECT status,COUNT(*) count,COALESCE(SUM(amount_fen),0) amount_fen,COALESCE(SUM(credits),0) credits FROM orders WHERE created_at>=? GROUP BY status ORDER BY count DESC", (since,)).fetchall()
        ledger_rows = db.execute("SELECT kind,COUNT(*) count,COALESCE(SUM(delta_available),0) delta_available,COALESCE(SUM(delta_held),0) delta_held FROM ledger_entries WHERE created_at>=? GROUP BY kind ORDER BY count DESC", (since,)).fetchall()
        wallet = db.execute("SELECT COUNT(*) wallets,COALESCE(SUM(available_credits),0) available_credits,COALESCE(SUM(held_credits),0) held_credits FROM wallets").fetchone()
        return {"days": days, "orders": [dict(row) for row in order_rows], "ledger": [dict(row) for row in ledger_rows], "wallets": dict(wallet)}, len(order_rows) + len(ledger_rows) + 1

    def _task(self, db: sqlite3.Connection, arguments: dict[str, Any]) -> tuple[dict[str, Any], int]:
        if set(arguments) != {"task_id"}:
            raise McpError(-32602, "task_id is the only accepted argument")
        task_id = arguments["task_id"]
        if not isinstance(task_id, str) or not 8 <= len(task_id) <= 128:
            raise McpError(-32602, "task_id is invalid")
        row = db.execute("SELECT id,kind,provider,operation,model,quoted_credits,status,error_code,created_at,updated_at,attempt_count FROM generation_tasks WHERE id=?", (task_id,)).fetchone()
        return ({"task": dict(row) if row else None}, 1 if row else 0)


class RateLimiter:
    def __init__(self, limit: int):
        self.limit = limit
        self._lock = threading.Lock()
        self._requests: dict[str, list[float]] = {}

    def allow(self, key: str) -> bool:
        now = time.monotonic()
        with self._lock:
            recent = [stamp for stamp in self._requests.get(key, []) if now - stamp < 60]
            if len(recent) >= self.limit:
                self._requests[key] = recent
                return False
            recent.append(now)
            self._requests[key] = recent
            return True


class AuditLogger:
    def __init__(self, path: Path):
        self.path = path
        self._lock = threading.Lock()
        path.parent.mkdir(parents=True, exist_ok=True)

    def write(self, record: dict[str, Any]) -> None:
        line = json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n"
        with self._lock:
            with self.path.open("a", encoding="utf-8") as stream:
                os.chmod(self.path, 0o600)
                stream.write(line)


class ReadonlyMcpHandler(BaseHTTPRequestHandler):
    config: McpConfig
    queries: SnapshotQueries
    limiter: RateLimiter
    audit: AuditLogger
    expected_token: str
    server_version = "SeeCutReadonlyMCP/0.1"

    def log_message(self, format: str, *args: Any) -> None:
        return

    def do_GET(self) -> None:
        if self.path == "/health":
            self._send(200, {"status": "ok"})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self) -> None:
        request_id = str(uuid.uuid4())
        started = time.monotonic()
        tool = None
        rows = 0
        status = "error"
        error = None
        authenticated = False
        arguments: dict[str, Any] = {}
        try:
            if self.path != "/mcp":
                error = "not_found"
                self._audit(request_id, started, tool, arguments, status, error, rows, authenticated)
                self._send(404, {"error": "not found"})
                return
            supplied = self.headers.get("Authorization", "")
            if not supplied.startswith("Bearer ") or not hmac.compare_digest(supplied[7:], self.expected_token):
                error = "unauthorized"
                self._audit(request_id, started, tool, arguments, status, error, rows, authenticated)
                self._send(401, {"error": "unauthorized"}, {"WWW-Authenticate": "Bearer"})
                return
            authenticated = True
            if not self.limiter.allow(self.config.key_id):
                error = "rate_limited"
                self._audit(request_id, started, tool, arguments, status, error, rows, authenticated)
                self._send(429, {"error": "rate limit exceeded"}, {"Retry-After": "60"})
                return
            length = int(self.headers.get("Content-Length", "0"))
            if length <= 0 or length > MAX_BODY_BYTES:
                raise McpError(-32600, "invalid request size")
            message = json.loads(self.rfile.read(length))
            if (
                isinstance(message, dict)
                and message.get("jsonrpc") == "2.0"
                and message.get("method") == "notifications/initialized"
                and "id" not in message
            ):
                status = "ok"
                self._audit(request_id, started, tool, arguments, status, error, rows, authenticated)
                self.send_response(HTTPStatus.ACCEPTED)
                self.send_header("Cache-Control", "no-store")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            params = message.get("params", {}) if isinstance(message, dict) else {}
            arguments = params.get("arguments", {}) if isinstance(params, dict) else {}
            if not isinstance(arguments, dict):
                arguments = {}
            response, tool, rows = self._dispatch(message)
            status = "ok"
            self._audit(request_id, started, tool, arguments, status, error, rows, authenticated)
            self._send(200, response)
        except (json.JSONDecodeError, ValueError):
            error = "invalid_request"
            self._audit(request_id, started, tool, arguments, status, error, rows, authenticated)
            self._send(400, self._rpc_error(None, -32700, "invalid JSON"))
        except McpError as exc:
            error = f"rpc_{exc.code}"
            message_id = locals().get("message", {}).get("id") if isinstance(locals().get("message"), dict) else None
            self._audit(request_id, started, tool, arguments, status, error, rows, authenticated)
            self._send(200, self._rpc_error(message_id, exc.code, exc.message))

    def _audit(
        self,
        request_id: str,
        started: float,
        tool: str | None,
        arguments: dict[str, Any],
        status: str,
        error: str | None,
        rows: int,
        authenticated: bool,
    ) -> None:
        digest = hashlib.sha256(
            json.dumps(arguments, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()[:16]
        self.audit.write({
            "at": int(time.time()),
            "request_id": request_id,
            "key_id": self.config.key_id if authenticated else None,
            "tool": tool,
            "argument_digest": digest,
            "status": status,
            "error": error,
            "rows": rows,
            "duration_ms": round((time.monotonic() - started) * 1000),
        })

    def _dispatch(self, message: Any) -> tuple[dict[str, Any], str | None, int]:
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0" or "id" not in message:
            raise McpError(-32600, "invalid JSON-RPC request")
        message_id = message["id"]
        method = message.get("method")
        if method == "initialize":
            return {"jsonrpc": "2.0", "id": message_id, "result": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {"tools": {"listChanged": False}}, "serverInfo": {"name": "seecut-production-readonly", "version": "0.1.0"}}}, None, 0
        if method == "tools/list":
            return {"jsonrpc": "2.0", "id": message_id, "result": {"tools": TOOLS}}, None, len(TOOLS)
        if method == "tools/call":
            params = message.get("params", {})
            name = params.get("name")
            if not isinstance(name, str):
                raise McpError(-32602, "tool name is required")
            result, rows = self.queries.call(name, params.get("arguments", {}))
            text = json.dumps(result, ensure_ascii=False, separators=(",", ":"))
            return {"jsonrpc": "2.0", "id": message_id, "result": {"content": [{"type": "text", "text": text}], "structuredContent": result, "isError": False}}, name, rows
        raise McpError(-32601, "method not found")

    @staticmethod
    def _rpc_error(message_id: Any, code: int, message: str) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": message_id, "error": {"code": code, "message": message}}

    def _send(self, status: int, body: dict[str, Any], headers: dict[str, str] | None = None) -> None:
        payload = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode()
        if len(payload) > MAX_RESPONSE_BYTES:
            status, payload = 500, b'{"error":"response limit exceeded"}'
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Length", str(len(payload)))
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(payload)


def main() -> None:
    config = McpConfig.from_env()
    handler = type("ConfiguredReadonlyMcpHandler", (ReadonlyMcpHandler,), {
        "config": config, "queries": SnapshotQueries(config),
        "limiter": RateLimiter(config.requests_per_minute),
        "audit": AuditLogger(config.audit_log_path), "expected_token": config.token(),
    })
    server = ThreadingHTTPServer((config.host, config.port), handler)
    print(f"SeeCut read-only MCP listening on http://{config.host}:{config.port}/mcp")
    try:
        server.serve_forever()
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
