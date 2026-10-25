from __future__ import annotations

import json
import os
import sqlite3
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer
from pathlib import Path

from seecut_server.mcp_snapshot import create_snapshot
from seecut_server.readonly_mcp import (
    AuditLogger,
    McpConfig,
    McpError,
    RateLimiter,
    ReadonlyMcpHandler,
    SnapshotQueries,
)


class ReadonlyMcpTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.source = root / "production.sqlite3"
        self.snapshot = root / "snapshot.sqlite3"
        self.audit = root / "audit.jsonl"
        self.token = root / "token"
        self.token.write_text("x" * 48, encoding="utf-8")
        os.chmod(self.token, 0o600)
        schema = Path(__file__).resolve().parents[1] / "schema.sql"
        with sqlite3.connect(self.source) as db:
            db.executescript(schema.read_text(encoding="utf-8"))
            now = int(time.time())
            db.execute("INSERT INTO users VALUES(?,?,?,?,?,?)", ("user-secret", "person@example.com", "password-secret", now, now, now))
            db.execute("INSERT INTO teams VALUES(?,?,?,?)", ("team-secret", "Private team", "user-secret", now))
            db.execute("INSERT INTO wallets VALUES(?,?,?,?)", ("user-secret", 100, 5, now))
            db.execute("INSERT INTO generation_tasks(id,user_id,kind,provider,operation,model,prompt,request_json,idempotency_key,quoted_credits,status,upstream_task_id,upstream_response_json,error_code,error_message,created_at,updated_at,next_attempt_at,attempt_count,lease_owner,lease_expires_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", ("task-12345678", "user-secret", "image", "image2", "generate", "safe-model", "secret prompt", '{"api_key":"secret-key"}', "idem-secret", 4, "failed", "upstream-secret", '{"secret":"raw-provider-payload"}', "PROVIDER_ERROR", "private error detail", now, now, now, 1, None, None))
            db.commit()
        create_snapshot(self.source, self.snapshot)
        self.config = McpConfig("127.0.0.1", 0, self.snapshot, self.token, self.audit, "test-key", 10, 1000, 900)
        self.queries = SnapshotQueries(self.config)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_snapshot_is_independent_and_source_is_unchanged(self) -> None:
        before = self.source.read_bytes()
        result, _ = self.queries.call("get_system_summary", {})
        self.assertEqual(result["counts"]["users"], 1)
        self.assertEqual(self.source.read_bytes(), before)
        self.assertEqual(self.snapshot.stat().st_mode & 0o777, 0o440)

    def test_task_response_excludes_sensitive_columns(self) -> None:
        result, rows = self.queries.call("get_generation_task_status", {"task_id": "task-12345678"})
        encoded = json.dumps(result)
        self.assertEqual(rows, 1)
        for secret in ("person@example.com", "password-secret", "secret prompt", "secret-key", "upstream-secret", "raw-provider-payload", "private error detail", "user-secret"):
            self.assertNotIn(secret, encoded)
        self.assertEqual(result["task"]["error_code"], "PROVIDER_ERROR")

    def test_unknown_arguments_and_sql_tools_are_rejected(self) -> None:
        with self.assertRaises(McpError):
            self.queries.call("get_generation_summary", {"days": 7, "sql": "DELETE FROM users"})
        with self.assertRaises(McpError):
            self.queries.call("execute_sql", {"sql": "SELECT * FROM users"})

    def test_stale_snapshot_is_rejected(self) -> None:
        os.chmod(self.snapshot, 0o600)
        with sqlite3.connect(f"file:{self.snapshot}?mode=rw", uri=True) as db:
            db.execute("UPDATE mcp_snapshot_metadata SET created_at=0")
            db.commit()
        with self.assertRaisesRegex(McpError, "stale"):
            self.queries.call("get_system_summary", {})

    def test_http_auth_and_audit_do_not_disclose_credentials_or_arguments(self) -> None:
        handler = type("TestMcpHandler", (ReadonlyMcpHandler,), {
            "config": self.config,
            "queries": self.queries,
            "limiter": RateLimiter(10),
            "audit": AuditLogger(self.audit),
            "expected_token": "x" * 48,
        })
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        endpoint = f"http://127.0.0.1:{server.server_port}/mcp"
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "get_generation_task_status", "arguments": {"task_id": "task-12345678"}}}).encode()
        try:
            with self.assertRaises(urllib.error.HTTPError) as unauthorized:
                urllib.request.urlopen(urllib.request.Request(endpoint, data=body, method="POST"))
            self.assertEqual(unauthorized.exception.code, 401)
            unauthorized.exception.close()
            request = urllib.request.Request(endpoint, data=body, method="POST", headers={"Authorization": "Bearer " + "x" * 48, "Content-Type": "application/json"})
            with urllib.request.urlopen(request) as response:
                payload = json.load(response)
            self.assertEqual(payload["result"]["structuredContent"]["task"]["status"], "failed")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
        audit = self.audit.read_text(encoding="utf-8")
        self.assertIn('"key_id":"test-key"', audit)
        self.assertIn('"key_id":null', audit)
        self.assertNotIn("task-12345678", audit)
        self.assertNotIn("x" * 48, audit)


if __name__ == "__main__":
    unittest.main()
