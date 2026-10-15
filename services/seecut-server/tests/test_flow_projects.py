from __future__ import annotations

import hashlib
import http.client
import io
import json
import os
import socket
import sqlite3
import tempfile
import threading
import unittest
import zipfile
from concurrent.futures import ThreadPoolExecutor
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch
from urllib.parse import urlencode
from uuid import uuid4

from seecut_server.config import Config
from seecut_server.db import Database
from seecut_server.errors import ApiError
from seecut_server.http import SeeCutHandler
from seecut_server.security import token_digest
from seecut_server.service import SeeCutService


def snapshot(text):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        archive.writestr("canvas.json", text)
    return output.getvalue()


class FlowFixture(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        with patch.dict(os.environ, {
            "SECUT_ENV": "test", "SECUT_DATABASE_PATH": str(root / "account.sqlite3"),
            "SECUT_STORAGE_PATH": str(root / "storage"),
            "SECUT_SIGNING_SECRET": "test-signing-secret-that-is-long-enough",
            "SECUT_MAX_UPLOAD_BYTES": "2097152",
        }, clear=True):
            config = Config.from_env()
        self.db = Database(config.database_path, Path(__file__).resolve().parents[1] / "schema.sql")
        self.db.initialize()
        self.service = SeeCutService(config, self.db, clock=lambda: 1_700_000_000)
        self.flow = self.service.flow_projects
        self.project = str(uuid4())
        self.data = snapshot('{"nodes":[]}')
        with self.db.transaction() as connection:
            for user in ("one", "two"):
                connection.execute("INSERT INTO users(id,email,password_hash,email_verified_at,created_at,updated_at) VALUES(?,?,?,1,1,1)", (user, user + "@example.test", "unused-test-hash"))
                connection.execute("INSERT INTO sessions(id,user_id,token_hash,expires_at,created_at) VALUES(?,?,?,?,1)", (user, user, token_digest("token-" + user), 1_800_000_000))

    def save(self, *, user="one", data=None, base=0, key="save-one", name="Project"):
        data = self.data if data is None else data
        return self.flow.save(user, self.project, io.BytesIO(data), len(data), hashlib.sha256(data).hexdigest(), key, base, name)


class FlowProjectsTest(FlowFixture):
    def test_account_isolation_and_same_uuid_independent_ownership(self):
        first = self.save()
        self.assertEqual(self.flow.list("two"), {"items": []})
        for operation in (self.flow.get, self.flow.download):
            with self.assertRaises(ApiError) as error:
                operation("two", self.project)
            self.assertEqual((error.exception.status, error.exception.code), (404, "FLOW_PROJECT_NOT_FOUND"))
        second = self.save(user="two", data=snapshot("other user"))
        self.assertEqual(second["revision"], 1)
        self.assertNotEqual(first["sha256"], second["sha256"])
        self.assertEqual(self.flow.download("one", self.project)[0].read_bytes(), self.data)

    def test_concurrent_compare_and_swap_only_commits_one_version(self):
        self.save()
        gate = threading.Barrier(2)

        def save_competing(index):
            gate.wait(timeout=3)
            try:
                return self.save(base=1, key=f"compete-{index}", data=snapshot(str(index)))
            except ApiError as error:
                return error

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(save_competing, [1, 2]))
        receipts = [value for value in results if isinstance(value, dict)]
        errors = [value for value in results if isinstance(value, ApiError)]
        self.assertEqual(len(receipts), 1)
        self.assertEqual(len(errors), 1)
        self.assertEqual((errors[0].status, errors[0].code, errors[0].details), (409, "FLOW_REVISION_CONFLICT", {"current_revision": 2}))
        self.assertEqual(self.flow.get("one", self.project), receipts[0])

    def test_idempotency_retains_original_revision_and_rejects_different_payload(self):
        first = self.save()
        newer = self.save(base=1, key="save-two", data=snapshot("newer"))
        self.assertEqual(self.save(), first)
        self.assertEqual(self.flow.get("one", self.project, 1), first)
        self.assertEqual(self.flow.get("one", self.project), newer)
        self.assertEqual(self.flow.download("one", self.project, 1)[0].read_bytes(), self.data)
        for changes in ({"data": snapshot("changed")}, {"base": 1}, {"name": "Changed"}):
            with self.assertRaises(ApiError) as error:
                self.save(**changes)
            self.assertEqual(error.exception.code, "FLOW_IDEMPOTENCY_CONFLICT")
        with self.assertRaises(ApiError) as error:
            self.flow.get("one", self.project, 3)
        self.assertEqual(error.exception.code, "FLOW_SNAPSHOT_NOT_FOUND")

    def test_invalid_upload_does_not_change_previous_snapshot(self):
        first = self.save()
        cases = [
            (io.BytesIO(self.data[:-1]), len(self.data), hashlib.sha256(self.data).hexdigest(), "FLOW_INCOMPLETE_UPLOAD"),
            (io.BytesIO(self.data), len(self.data), "0" * 64, "FLOW_HASH_MISMATCH"),
            (io.BytesIO(b"notzip"), 6, hashlib.sha256(b"notzip").hexdigest(), "FLOW_INVALID_SNAPSHOT"),
        ]
        for stream, length, digest, code in cases:
            with self.assertRaises(ApiError) as error:
                self.flow.save("one", self.project, stream, length, digest, "save-two", 1, "Project")
            self.assertEqual(error.exception.code, code)
            self.assertEqual(self.flow.get("one", self.project), first)
        self.assertEqual(list((self.flow.storage / "flow/.incoming").iterdir()), [])

    def test_database_failure_after_blob_creation_preserves_previous_snapshot(self):
        first = self.save()
        with self.db.connect() as connection:
            connection.execute("CREATE TRIGGER reject_flow_receipt BEFORE INSERT ON flow_snapshot_requests BEGIN SELECT RAISE(ABORT,'simulated failure'); END")
        with self.assertRaises(sqlite3.IntegrityError):
            self.save(base=1, key="save-two", data=snapshot("newer"))
        self.assertEqual(self.flow.get("one", self.project), first)
        self.assertEqual(self.flow.download("one", self.project)[0].read_bytes(), self.data)
        with self.db.connect() as connection:
            self.assertEqual(connection.execute("SELECT count(*) FROM flow_snapshots").fetchone()[0], 1)
        self.assertEqual(len(list((self.flow.storage / "flow").rglob("*.zip"))), 2)

    def test_stream_is_chunked_and_input_validation_is_bounded(self):
        class BoundedStream(io.BytesIO):
            def read(self, size=-1):
                if not 0 < size <= 1024 * 1024:
                    raise AssertionError("unbounded read")
                return super().read(size)

        data = snapshot("a" * (1024 * 1024 + 5))
        result = self.flow.save("one", self.project, BoundedStream(data), len(data), hashlib.sha256(data).hexdigest(), "large", 0, "\x00  Name\n")
        self.assertEqual(result["name"], "Name")
        for identity, revision, sha, key in [("bad", 0, "0" * 64, "key"), (self.project, -1, "0" * 64, "key"), (self.project, 0, "bad", "key"), (self.project, 0, "0" * 64, "")]:
            with self.assertRaises(ApiError):
                self.flow.save("one", identity, io.BytesIO(data), len(data), sha, key, revision, "Name")
        with self.assertRaises(ApiError) as error:
            self.flow.save("one", self.project, io.BytesIO(b""), self.flow.max_snapshot_bytes + 1, "0" * 64, "key", 1, "Name")
        self.assertEqual(error.exception.status, 413)


class FlowProjectsHTTPTest(FlowFixture):
    def setUp(self):
        super().setUp()
        handler = type("FlowTestHandler", (SeeCutHandler,), {"service": self.service, "log_message": lambda *args: None})
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_server)

    def stop_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(2)

    def request(self, method, path, *, body=None, token="token-one", headers=None):
        request_headers = {"Authorization": "Bearer " + token} if token else {}
        request_headers.update(headers or {})
        connection = http.client.HTTPConnection(*self.server.server_address, timeout=3)
        try:
            connection.request(method, path, body, request_headers)
            response = connection.getresponse()
            raw = response.read()
            return response.status, json.loads(raw) if "application/json" in response.getheader("Content-Type", "") else raw, dict(response.getheaders())
        finally:
            connection.close()

    def upload(self, *, data=None, base=0, key="http-one", token="token-one", extra_headers=None):
        data = self.data if data is None else data
        headers = {"Content-Type": "application/zip", "X-Content-SHA256": hashlib.sha256(data).hexdigest(), "Idempotency-Key": key}
        headers.update(extra_headers or {})
        return self.request("PUT", f"/api/flow/projects/{self.project}/snapshot?" + urlencode({"base_revision": base, "name": "测试项目"}), body=data, token=token, headers=headers)

    def test_http_roundtrip_authentication_and_immutable_receipts(self):
        self.assertEqual(self.upload(token="")[0], 401)
        capability = self.request("GET", "/api/capabilities", token="")[1]["flow_projects"]
        self.assertEqual((capability["version"], capability["max_snapshot_bytes"]), (1, 2097152))
        status, first, _ = self.upload()
        self.assertEqual(status, 200)
        for route in (f"/api/flow/projects/{self.project}", first["download_url"]):
            self.assertEqual(self.request("GET", route, token="token-two")[0], 404)
            self.assertEqual(self.request("GET", route, token="")[0], 401)
        self.assertEqual(self.request("GET", "/api/flow/projects", token="token-two")[1], {"items": []})
        self.assertEqual(self.request("GET", "/api/flow/projects")[1]["items"][0]["id"], self.project)
        self.assertEqual(self.upload(base=1, key="http-two", data=snapshot("newer"))[0], 200)
        self.assertEqual(self.upload()[1], first)
        status, data, headers = self.request("GET", first["download_url"])
        self.assertEqual((status, data), (200, self.data))
        self.assertEqual(headers["X-Content-SHA256"], first["sha256"])
        self.assertEqual(headers["X-Flow-Revision"], "1")
        self.assertEqual(self.request("GET", f"/api/flow/projects/{self.project}?revision=1")[1], first)
        self.assertEqual(self.upload(data=snapshot("conflict"))[1]["error"]["code"], "FLOW_IDEMPOTENCY_CONFLICT")
        stale = self.upload(base=0, key="stale")[1]["error"]
        self.assertEqual((stale["code"], stale["current_revision"]), ("FLOW_REVISION_CONFLICT", 2))

    def test_http_rejects_hash_type_oversize_and_truncated_body(self):
        self.assertEqual(self.upload(extra_headers={"X-Content-SHA256": "0" * 64})[1]["error"]["code"], "FLOW_HASH_MISMATCH")
        self.assertEqual(self.upload(extra_headers={"Content-Type": "application/json"})[0], 415)
        self.assertEqual(self.upload(extra_headers={"Content-Length": str(self.flow.max_snapshot_bytes + 1)})[0], 413)
        connection = http.client.HTTPConnection(*self.server.server_address, timeout=3)
        try:
            connection.putrequest("PUT", f"/api/flow/projects/{self.project}/snapshot?base_revision=0")
            for key, value in {"Authorization": "Bearer token-one", "Content-Type": "application/zip", "Content-Length": str(len(self.data)), "X-Content-SHA256": hashlib.sha256(self.data).hexdigest(), "Idempotency-Key": "truncated"}.items():
                connection.putheader(key, value)
            connection.endheaders(self.data[:-8])
            connection.sock.shutdown(socket.SHUT_WR)
            response = connection.getresponse()
            self.assertEqual(response.status, 400)
            self.assertEqual(json.loads(response.read())["error"]["code"], "FLOW_INCOMPLETE_UPLOAD")
        finally:
            connection.close()
        self.assertEqual(self.flow.list("one"), {"items": []})

    def test_http_concurrent_cas_and_same_key_replay(self):
        self.assertEqual(self.upload()[0], 200)
        gate = threading.Barrier(2)

        def competing(index):
            gate.wait(timeout=3)
            return self.upload(base=1, key=f"parallel-{index}", data=snapshot(str(index)))

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(competing, (1, 2)))
        self.assertEqual(sorted(result[0] for result in results), [200, 409])
        conflict = next(result[1]["error"] for result in results if result[0] == 409)
        self.assertEqual((conflict["code"], conflict["current_revision"]), ("FLOW_REVISION_CONFLICT", 2))
        with ThreadPoolExecutor(max_workers=2) as executor:
            repeats = list(executor.map(lambda _: self.upload(base=2, key="same-key", data=snapshot("shared")), (1, 2)))
        self.assertEqual([result[0] for result in repeats], [200, 200])
        self.assertEqual(repeats[0][1], repeats[1][1])
        self.assertEqual(repeats[0][1]["revision"], 3)


if __name__ == "__main__":
    unittest.main()
