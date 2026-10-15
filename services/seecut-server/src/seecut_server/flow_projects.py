from __future__ import annotations

import hashlib
import json
import os
import re
import tempfile
import unicodedata
from pathlib import Path
from typing import Any, BinaryIO
from uuid import UUID, uuid4

from .errors import ApiError


class FlowProjects:
    """Account-owned immutable ZIP snapshots with transactional version receipts."""

    def __init__(self, service):
        self.service = service
        self.db = service.db
        self.storage = service.config.storage_path
        self.max_snapshot_bytes = min(service.config.max_upload_bytes, 512 * 1024 * 1024)

    def capabilities(self):
        return {"version": 1, "snapshots": True, "max_snapshot_bytes": self.max_snapshot_bytes}

    @staticmethod
    def project_id(value: str) -> str:
        try:
            parsed = UUID(value)
        except (ValueError, TypeError, AttributeError):
            raise ApiError(400, "FLOW_INVALID_REQUEST", "Flow 项目编号无效")
        if str(parsed) != value.lower():
            raise ApiError(400, "FLOW_INVALID_REQUEST", "Flow 项目编号需要完整 UUID")
        return str(parsed)

    @staticmethod
    def revision(value: Any, *, allow_zero=False) -> int:
        if not isinstance(value, (int, str)) or isinstance(value, bool) or not re.fullmatch(r"[0-9]+", str(value)):
            raise ApiError(400, "FLOW_INVALID_REQUEST", "Flow 版本号无效")
        number = int(value)
        if number < (0 if allow_zero else 1) or number >= 2**63 - 1:
            raise ApiError(400, "FLOW_INVALID_REQUEST", "Flow 版本号超出范围")
        return number

    @staticmethod
    def metadata(row, *, download=False):
        result = {key: row[key] for key in ("id", "name", "revision", "sha256", "size_bytes", "updated_at")}
        if download:
            result["download_url"] = f"/api/flow/projects/{row['id']}/snapshot?revision={row['revision']}"
        return result

    def list(self, user_id: str):
        with self.db.connect() as connection:
            rows = connection.execute("SELECT * FROM flow_projects WHERE user_id=? ORDER BY updated_at DESC,id", (user_id,)).fetchall()
        return {"items": [self.metadata(row) for row in rows]}

    def _read(self, user_id: str, project_id: str, revision=None):
        project_id = self.project_id(project_id)
        selected_revision = None if revision is None else self.revision(revision)
        with self.db.connect() as connection:
            project = connection.execute("SELECT * FROM flow_projects WHERE user_id=? AND id=?", (user_id, project_id)).fetchone()
            if project is None:
                raise ApiError(404, "FLOW_PROJECT_NOT_FOUND", "Flow 项目不存在")
            row = connection.execute(
                "SELECT project_id AS id,* FROM flow_snapshots WHERE user_id=? AND project_id=? AND revision=?",
                (user_id, project_id, project["revision"] if selected_revision is None else selected_revision),
            ).fetchone()
        if row is None:
            raise ApiError(404, "FLOW_SNAPSHOT_NOT_FOUND", "Flow 快照版本不存在")
        return row

    def get(self, user_id: str, project_id: str, revision=None):
        return self.metadata(self._read(user_id, project_id, revision), download=True)

    def download(self, user_id: str, project_id: str, revision=None):
        row = self._read(user_id, project_id, revision)
        path = self.storage / row["object_key"]
        if not path.is_file() or path.stat().st_size != row["size_bytes"]:
            raise ApiError(503, "FLOW_SNAPSHOT_UNAVAILABLE", "Flow 快照暂时无法读取，请稍后重试")
        return path, self.metadata(row, download=True)

    def save(self, user_id: str, project_id: str, stream: BinaryIO, content_length: int,
             expected_sha256: str, key: str, base_revision, name: str):
        project_id = self.project_id(project_id)
        base_revision = self.revision(base_revision, allow_zero=True)
        if not isinstance(name, str):
            raise ApiError(400, "FLOW_INVALID_REQUEST", "Flow 项目名称无效")
        name = "".join(char for char in name if not unicodedata.category(char).startswith("C")).strip()[:128] or "未命名 Flow"
        if not isinstance(key, str) or not key.strip() or len(key) > 128 or any(unicodedata.category(char).startswith("C") for char in key):
            raise ApiError(400, "FLOW_INVALID_REQUEST", "请提供不超过128字符的 Idempotency-Key")
        if not isinstance(expected_sha256, str) or not re.fullmatch(r"[0-9a-fA-F]{64}", expected_sha256):
            raise ApiError(400, "FLOW_INVALID_SHA256", "请提供完整的 X-Content-SHA256")
        expected_sha256 = expected_sha256.lower()
        if not isinstance(content_length, int) or isinstance(content_length, bool) or content_length < 4:
            raise ApiError(400, "FLOW_INVALID_SNAPSHOT", "Flow 快照内容为空或不完整")
        if content_length > self.max_snapshot_bytes:
            raise ApiError(413, "REQUEST_TOO_LARGE", "Flow 快照超出允许大小")
        incoming = self.storage / "flow/.incoming"
        incoming.mkdir(parents=True, exist_ok=True)
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=incoming, prefix="snapshot-", delete=False) as output:
                temporary = Path(output.name)
                remaining = content_length
                digest = hashlib.sha256()
                prefix = b""
                while remaining:
                    try:
                        chunk = stream.read(min(1024 * 1024, remaining))
                    except (OSError, TimeoutError) as error:
                        raise ApiError(400, "FLOW_INCOMPLETE_UPLOAD", "Flow 快照上传未完成") from error
                    if not chunk or len(chunk) > remaining:
                        raise ApiError(400, "FLOW_INCOMPLETE_UPLOAD", "Flow 快照上传长度不完整")
                    prefix = (prefix + chunk[:4])[:4]
                    output.write(chunk)
                    digest.update(chunk)
                    remaining -= len(chunk)
                if digest.hexdigest() != expected_sha256:
                    raise ApiError(400, "FLOW_HASH_MISMATCH", "Flow 快照 SHA256 校验失败")
                if prefix not in (b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"):
                    raise ApiError(400, "FLOW_INVALID_SNAPSHOT", "Flow 快照需要 ZIP 格式")
                output.flush()
                os.fsync(output.fileno())
            fingerprint = hashlib.sha256(json.dumps(
                {"base_revision": base_revision, "name": name, "sha256": expected_sha256, "size_bytes": content_length},
                ensure_ascii=False, sort_keys=True, separators=(",", ":"),
            ).encode()).hexdigest()
            with self.db.transaction(immediate=True) as connection:
                receipt = connection.execute(
                    "SELECT fingerprint,response_json FROM flow_snapshot_requests WHERE user_id=? AND project_id=? AND key=?",
                    (user_id, project_id, key),
                ).fetchone()
                if receipt is not None:
                    if receipt["fingerprint"] != fingerprint:
                        raise ApiError(409, "FLOW_IDEMPOTENCY_CONFLICT", "同一保存请求编号不能用于其他快照")
                    return json.loads(receipt["response_json"])
                project = connection.execute("SELECT revision FROM flow_projects WHERE user_id=? AND id=?", (user_id, project_id)).fetchone()
                current_revision = project["revision"] if project else 0
                if current_revision != base_revision:
                    raise ApiError(409, "FLOW_REVISION_CONFLICT", "Flow 项目已有其他版本，请先读取云端版本", {"current_revision": current_revision})
                revision = current_revision + 1
                now = self.service.now()
                owner = hashlib.sha256(user_id.encode()).hexdigest()
                object_key = f"flow/{owner}/{project_id}/{uuid4()}.zip"
                target = self.storage / object_key
                target.parent.mkdir(parents=True, exist_ok=True)
                os.replace(temporary, target)
                # Blob becomes durable before committing a reference to it. A
                # failed transaction can leave an orphan, never replace an old blob.
                directory_fd = os.open(target.parent, os.O_RDONLY)
                try:
                    os.fsync(directory_fd)
                finally:
                    os.close(directory_fd)
                result = {"id": project_id, "name": name, "revision": revision, "sha256": expected_sha256,
                          "size_bytes": content_length, "updated_at": now,
                          "download_url": f"/api/flow/projects/{project_id}/snapshot?revision={revision}"}
                if project is None:
                    connection.execute("INSERT INTO flow_projects(user_id,id,name,revision,sha256,size_bytes,updated_at) VALUES(?,?,?,?,?,?,?)",
                                       (user_id, project_id, name, revision, expected_sha256, content_length, now))
                else:
                    changed = connection.execute("UPDATE flow_projects SET name=?,revision=?,sha256=?,size_bytes=?,updated_at=? WHERE user_id=? AND id=? AND revision=?",
                                                 (name, revision, expected_sha256, content_length, now, user_id, project_id, base_revision)).rowcount
                    if changed != 1:
                        raise ApiError(409, "FLOW_REVISION_CONFLICT", "Flow 项目版本已变化，请重新读取")
                connection.execute("INSERT INTO flow_snapshots(user_id,project_id,revision,name,sha256,size_bytes,updated_at,object_key) VALUES(?,?,?,?,?,?,?,?)",
                                   (user_id, project_id, revision, name, expected_sha256, content_length, now, object_key))
                connection.execute("INSERT INTO flow_snapshot_requests(user_id,project_id,key,fingerprint,response_json) VALUES(?,?,?,?,?)",
                                   (user_id, project_id, key, fingerprint, json.dumps(result, ensure_ascii=False)))
                return result
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
