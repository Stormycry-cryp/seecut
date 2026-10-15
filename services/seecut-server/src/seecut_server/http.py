from __future__ import annotations

import json
import mimetypes
import re
import urllib.parse
from ipaddress import ip_address
from urllib.parse import quote
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler
from pathlib import Path
from typing import Any

from .security import verify_path_signature
from .errors import ApiError
from .service import SeeCutService
from .xiangxin import XiangxinError


def _rate_limit_client_ip(peer_ip: str, real_ip_headers: list[str]) -> str:
    try:
        peer = ip_address(peer_ip)
    except ValueError:
        return "unknown"

    mapped_peer = getattr(peer, "ipv4_mapped", None)
    if not (peer.is_loopback or mapped_peer is not None and mapped_peer.is_loopback):
        return str(peer)
    if len(real_ip_headers) != 1:
        return str(peer)
    try:
        return str(ip_address(real_ip_headers[0]))
    except ValueError:
        return str(peer)


class SeeCutHandler(BaseHTTPRequestHandler):
    service: SeeCutService
    server_version = "SeeCutServer/0.1"

    def log_message(self, format: str, *args: Any) -> None:
        clean_path = urllib.parse.urlsplit(self.path).path
        print(f'{self.address_string()} - "{self.command} {clean_path}" {args[1] if len(args) > 1 else ""}')

    def do_GET(self) -> None:
        self._dispatch()

    def do_POST(self) -> None:
        self._dispatch()

    def do_PUT(self) -> None:
        self._dispatch()

    def do_DELETE(self) -> None:
        self._dispatch()

    def _dispatch(self) -> None:
        try:
            result = self._route()
            if result is not None:
                status, body = result
                self._json(status, body)
        except ApiError as exc:
            self._json(exc.status, {"error": {"code": exc.code, "message": exc.message, **exc.details}})
        except XiangxinError as exc:
            status = 503 if exc.code in {"XIANGXIN_NOT_CONFIGURED", "XIANGXIN_UNAVAILABLE"} else 502
            self._json(status, {"error": {"code": exc.code, "message": exc.message}})
        except json.JSONDecodeError:
            self._json(400, {"error": {"code": "INVALID_JSON", "message": "请求内容不是有效 JSON"}})
        except Exception:
            self._json(500, {"error": {"code": "INTERNAL_ERROR", "message": "服务器处理请求时发生错误"}})

    def _route(self) -> tuple[int, Any] | None:
        parsed = urllib.parse.urlsplit(self.path)
        path = parsed.path.rstrip("/") or "/"
        query = urllib.parse.parse_qs(parsed.query)
        method = self.command

        if path.startswith("/api/auth/"):
            limiter = getattr(self.server, "auth_rate_limiter", None)
            peer_ip = self.client_address[0] if self.client_address else ""
            client_ip = _rate_limit_client_ip(
                peer_ip,
                self.headers.get_all("X-Real-IP", []),
            )
            if limiter is not None and not limiter.allow(client_ip):
                retry_after = getattr(self.server, "auth_rate_limit_window_seconds", 60)
                raise ApiError(429, "AUTH_RATE_LIMITED", "请求过于频繁，请稍后再试", {"retry_after": retry_after})

        if method == "GET" and path == "/health":
            return 200, {"status": "ok"}
        if method == "GET" and path == "/api/capabilities":
            return 200, self.service.capabilities()

        if method == "POST" and path == "/api/auth/register":
            body = self._json_body()
            return 201, self.service.register(str(body.get("email", "")), str(body.get("password", "")))
        if method == "POST" and path == "/api/auth/verify-email":
            body = self._json_body()
            return 200, self.service.verify_email(
                str(body.get("email", "")), str(body.get("token", ""))
            )
        if method == "POST" and path == "/api/auth/resend-verification":
            return 202, self.service.resend_verification(str(self._json_body().get("email", "")))
        if method == "POST" and path == "/api/auth/login":
            body = self._json_body()
            return 200, self.service.login(str(body.get("email", "")), str(body.get("password", "")))
        if method == "POST" and path == "/api/auth/forgot-password":
            return 202, self.service.forgot_password(str(self._json_body().get("email", "")))
        if method == "POST" and path == "/api/auth/reset-password":
            body = self._json_body()
            return 200, self.service.reset_password(
                str(body.get("email", "")),
                str(body.get("token", "")),
                str(body.get("password", "")),
            )

        if method == "POST" and path == "/api/payments/alipay/notify":
            fields = self._form_body()
            response = self.service.process_alipay_notification(fields)
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.end_headers()
            self.wfile.write(response.encode("utf-8"))
            return None

        upload_match = re.fullmatch(r"/api/storage/uploads/([^/]+)", path)
        if method == "PUT" and upload_match:
            self._require_signature(path, query)
            content_type = self.headers.get("Content-Type", "application/octet-stream").split(";", 1)[0]
            content_length = self._content_length(self.service.config.max_upload_bytes)
            self.service.save_upload_stream(
                upload_match.group(1), self.rfile, content_length, content_type
            )
            return 200, {"uploaded": True}

        staging_file_match = re.fullmatch(r"/api/storage/staging/([^/]+)", path)
        if method == "GET" and staging_file_match:
            self._require_signature(path, query)
            file_path, content_type, filename = self.service.storage_upload(staging_file_match.group(1))
            self._file(file_path, content_type, filename)
            return None

        user, raw_token = self._auth()
        user_id = user["id"]
        if method == "GET" and path == "/api/prompts/capabilities":
            return 200, self.service.prompt_assistance.capabilities()
        if method == "POST" and path in {"/api/prompts/recommend", "/api/prompts/rewrite"}:
            return 200, self.service.prompt_assistance.run(user_id, path.rsplit("/", 1)[1], self.headers.get("Idempotency-Key", ""), self._json_body())
        if method == "GET" and path == "/api/flow/projects":
            return 200, self.service.flow_projects.list(user_id)
        flow_match = re.fullmatch(r"/api/flow/projects/([^/]+)(/snapshot)?", path)
        if flow_match:
            query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
            flow = self.service.flow_projects
            project_id, snapshot = flow_match.groups()
            if any(len(query.get(key, [])) > 1 for key in ("revision", "base_revision", "name")):
                raise ApiError(400, "FLOW_INVALID_REQUEST", "Flow 请求参数重复")
            revision = query.get("revision", [None])[0]
            if method == "GET" and not snapshot:
                return 200, flow.get(user_id, project_id, revision)
            if method == "GET" and snapshot:
                file_path, metadata = flow.download(user_id, project_id, revision)
                self._file(file_path, "application/zip", f"{metadata['id']}-r{metadata['revision']}.zip", headers={
                    "X-Content-SHA256": metadata["sha256"], "X-Flow-Revision": str(metadata["revision"]),
                    "Cache-Control": "no-store", "ETag": f'"{metadata["sha256"]}"',
                })
                return None
            if method == "PUT" and snapshot:
                # This route consumes a bounded binary stream, never JSON or
                # the media-upload purpose/ledger pipeline.
                self.close_connection = True
                if any(len(self.headers.get_all(header, [])) > 1 for header in ("Content-Length", "Content-Type", "X-Content-SHA256", "Idempotency-Key")):
                    raise ApiError(400, "FLOW_INVALID_REQUEST", "Flow 上传请求头重复")
                if self.headers.get("Content-Type", "").split(";", 1)[0].strip().lower() != "application/zip":
                    raise ApiError(415, "FLOW_CONTENT_TYPE_REQUIRED", "Flow 快照需要 application/zip")
                if self.headers.get("Transfer-Encoding"):
                    raise ApiError(400, "FLOW_INVALID_REQUEST", "Flow 上传需要明确的 Content-Length")
                if not self.headers.get("Content-Length"):
                    raise ApiError(411, "FLOW_LENGTH_REQUIRED", "Flow 上传缺少 Content-Length")
                length = self._content_length(flow.max_snapshot_bytes)
                old_timeout = self.connection.gettimeout()
                self.connection.settimeout(30)
                try:
                    return 200, flow.save(user_id, project_id, self.rfile, length,
                                          self.headers.get("X-Content-SHA256", ""), self.headers.get("Idempotency-Key", ""),
                                          query.get("base_revision", [None])[0], query.get("name", [""])[0])
                finally:
                    self.connection.settimeout(old_timeout)
            raise ApiError(404, "NOT_FOUND", "接口不存在")
        if path.startswith("/api/assistant/"):
            assistant = self.service.assistant
            key = self.headers.get("Idempotency-Key", "")
            if method == "GET" and path == "/api/assistant/capabilities":
                return 200, assistant.capabilities()
            if method == "POST" and path == "/api/assistant/turn-budgets":
                return 200, assistant.turn_budget(user_id, self._json_body(), key)
            close_budget = re.fullmatch(r"/api/assistant/turn-budgets/([^/]+)/close", path)
            if method == "POST" and close_budget:
                return 200, assistant.close_budget(user_id, close_budget.group(1), self._json_body(), key)
            if method == "POST" and path == "/api/assistant/quotes":
                return 200, assistant.quote(user_id, self._json_body(), key)
            if method == "POST" and path == "/api/assistant/operations":
                snapshot, created = assistant.create(user_id, self._json_body(), key)
                return (202 if created else 200), snapshot
            by_request = re.fullmatch(r"/api/assistant/operations/by-request/([^/]+)", path)
            if method == "GET" and by_request:
                return 200, assistant.by_request(user_id, by_request.group(1))
            operation = re.fullmatch(r"/api/assistant/operations/([^/]+)(?:/(events|cancel|reconcile))?", path)
            if operation:
                operation_id, action = operation.groups()
                if method == "GET" and action is None:
                    return 200, assistant.get(user_id, operation_id)
                if method == "GET" and action == "events":
                    try:
                        after = int(query.get("after", ["0"])[0])
                    except ValueError:
                        raise ApiError(400, "ASSISTANT_INVALID_CURSOR", "Agent 事件游标无效")
                    return 200, assistant.events(user_id, operation_id, after)
                if method == "POST" and action == "cancel":
                    return 200, assistant.cancel(user_id, operation_id, self._json_body(), key)
                if method == "POST" and action == "reconcile":
                    return 200, assistant.reconcile(user_id, operation_id, self._json_body(), key)
            raise ApiError(404, "NOT_FOUND", "接口不存在")
        if method == "POST" and path == "/api/auth/logout":
            self.service.logout(raw_token)
            return 204, None
        if method == "GET" and path == "/api/auth/me":
            return 200, {"id": user_id, "email": user["email"]}

        if method == "POST" and path == "/api/teams":
            return 201, self.service.create_team(user_id, str(self._json_body().get("name", "")))
        if method == "GET" and path == "/api/teams":
            return 200, {"items": self.service.list_teams(user_id)}
        team_invites = re.fullmatch(r"/api/teams/([^/]+)/invites", path)
        if method == "POST" and team_invites:
            return 201, self.service.create_invite(user_id, team_invites.group(1))
        revoke_invite = re.fullmatch(r"/api/teams/([^/]+)/invites/([^/]+)", path)
        if method == "DELETE" and revoke_invite:
            self.service.revoke_invite(user_id, revoke_invite.group(1), revoke_invite.group(2))
            return 204, None
        if method == "POST" and path == "/api/team-invites/accept":
            return 200, self.service.accept_invite(user_id, str(self._json_body().get("token", "")))
        team_members = re.fullmatch(r"/api/teams/([^/]+)/members", path)
        if method == "GET" and team_members:
            return 200, {"items": self.service.list_members(user_id, team_members.group(1))}
        remove_member = re.fullmatch(r"/api/teams/([^/]+)/members/([^/]+)", path)
        if method == "DELETE" and remove_member:
            self.service.remove_member(user_id, remove_member.group(1), remove_member.group(2))
            return 204, None

        if method == "POST" and path == "/api/uploads":
            body = self._json_body()
            return 201, self.service.prepare_upload(
                user_id=user_id,
                purpose=str(body.get("purpose", "")),
                filename=str(body.get("filename", "")),
                content_type=str(body.get("content_type", "")),
                size_bytes=body.get("size_bytes"),
                sha256=body.get("sha256"),
                team_id=body.get("team_id"),
            )
        team_assets = re.fullmatch(r"/api/teams/([^/]+)/assets", path)
        if method == "GET" and team_assets:
            trash = query.get("trash", ["false"])[0].lower() == "true"
            return 200, {"items": self.service.list_assets(user_id, team_assets.group(1), trash)}
        if method == "POST" and team_assets:
            body = self._json_body()
            return 201, self.service.complete_team_asset(
                user_id, team_assets.group(1), str(body.get("upload_id", ""))
            )
        asset_download = re.fullmatch(r"/api/teams/([^/]+)/assets/([^/]+)/download", path)
        if method == "POST" and asset_download:
            return 200, self.service.asset_download(user_id, asset_download.group(1), asset_download.group(2))
        asset_content = re.fullmatch(r"/api/teams/([^/]+)/assets/([^/]+)/content", path)
        if method == "GET" and asset_content:
            self.service._require_member(user_id, asset_content.group(1))
            file_path, content_type, filename = self.service.storage_asset(
                asset_content.group(1), asset_content.group(2)
            )
            self._file(file_path, content_type, filename)
            return None
        asset_delete = re.fullmatch(r"/api/teams/([^/]+)/assets/([^/]+)", path)
        if method == "DELETE" and asset_delete:
            self.service.delete_asset(user_id, asset_delete.group(1), asset_delete.group(2))
            return 204, None
        asset_restore = re.fullmatch(r"/api/teams/([^/]+)/assets/([^/]+)/restore", path)
        if method == "POST" and asset_restore:
            return 200, self.service.restore_asset(user_id, asset_restore.group(1), asset_restore.group(2))

        if method == "GET" and path == "/api/wallet":
            return 200, self.service.wallet(user_id, include_history=query.get('balance_only', ['0'])[0] != '1')
        if method == "GET" and path == "/api/wallet/ledger":
            return 200, self.service.wallet_ledger(user_id, category=query.get('category', ['all'])[0],
                cursor=query.get('cursor', [''])[0], limit=query.get('limit', ['50'])[0])
        if method == "GET" and path == "/api/credit-plans":
            return 200, {"items": self.service.list_plans()}
        if method == "POST" and path == "/api/orders":
            return 201, self.service.create_order(
                user_id, str(self._json_body().get("plan_id", "")),
                self.headers.get("Idempotency-Key", ""),
            )
        if method == "GET" and path == "/api/orders":
            return 200, self.service.orders(user_id, cursor=query.get('cursor', [''])[0], limit=query.get('limit', ['50'])[0])
        order_match = re.fullmatch(r"/api/orders/([^/]+)", path)
        if method == "GET" and order_match:
            return 200, self.service.order(user_id, order_match.group(1))
        order_refresh = re.fullmatch(r"/api/orders/([^/]+)/refresh", path)
        if method == "POST" and order_refresh:
            return 200, self.service.refresh_order(user_id, order_refresh.group(1))

        if method == "GET" and path == "/api/generation/models":
            return 200, self.service.generation_capabilities()
        if method == "GET" and path == "/api/generation/capabilities":
            return 200, self.service.generation_capabilities()
        if method == "POST" and path == "/api/generation/quote":
            body = self._json_body()
            return 200, self.service.quote_generation(user_id, body)
        if method == "POST" and path == "/api/generation/assets":
            return 200, self.service.register_generation_asset(
                user_id, str(self._json_body().get("upload_id", ""))
            )
        if method == "POST" and path in {"/api/generation/images", "/api/generation/videos"}:
            kind = "image" if path.endswith("images") else "video"
            key = self.headers.get("Idempotency-Key", "")
            return 202, self.service.create_generation_task(user_id, kind, self._json_body(), key)
        if method == "GET" and path == "/api/generation/tasks":
            return 200, {"items": self.service.list_generation_tasks(user_id)}
        if method == "GET" and path == "/api/generation/tasks/by-request":
            return 200, self.service.get_generation_task_by_request(
                user_id, self.headers.get("Idempotency-Key", "")
            )
        if method == "POST" and path == "/api/generation/tasks/trash":
            return 200, self.service.trash_generation_tasks(user_id, self._json_body().get("ids"))
        task_match = re.fullmatch(r"/api/generation/tasks/([^/]+)", path)
        if method == "GET" and task_match:
            refresh = query.get("refresh", ["false"])[0].lower() == "true"
            return 200, self.service.get_generation_task(user_id, task_match.group(1), refresh)
        output_match = re.fullmatch(r"/api/generation/tasks/([^/]+)/outputs/([^/]+)/content", path)
        if method == "GET" and output_match:
            file_path, content_type, filename = self.service.generation_output(
                user_id, output_match.group(1), output_match.group(2)
            )
            self._file(file_path, content_type, filename)
            return None

        raise ApiError(404, "NOT_FOUND", "接口不存在")

    def _auth(self) -> tuple[dict[str, Any], str]:
        header = self.headers.get("Authorization", "")
        if not header.startswith("Bearer "):
            raise ApiError(401, "AUTH_REQUIRED", "请先登录")
        token = header[7:].strip()
        return self.service.authenticate(token), token

    def _require_signature(self, path: str, query: dict[str, list[str]]) -> None:
        try:
            expires = int(query.get("expires", [""])[0])
        except ValueError:
            raise ApiError(403, "INVALID_STORAGE_SIGNATURE", "文件地址无效或已过期")
        signature = query.get("signature", [""])[0]
        if not verify_path_signature(self.service.config.signing_secret, path, expires, signature):
            raise ApiError(403, "INVALID_STORAGE_SIGNATURE", "文件地址无效或已过期")

    def _json_body(self) -> dict[str, Any]:
        body = self._raw_body(1024 * 1024)
        value = json.loads(body.decode("utf-8") or "{}")
        if not isinstance(value, dict):
            raise ApiError(400, "INVALID_JSON_OBJECT", "请求内容必须是 JSON 对象")
        return value

    def _form_body(self) -> dict[str, str]:
        value = urllib.parse.parse_qs(self._raw_body(1024 * 1024).decode("utf-8"), keep_blank_values=True)
        return {key: items[-1] for key, items in value.items()}

    def _content_length(self, maximum: int) -> int:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            raise ApiError(400, "INVALID_CONTENT_LENGTH", "请求长度无效")
        if length < 0 or length > maximum:
            raise ApiError(413, "REQUEST_TOO_LARGE", "上传内容超出允许范围")
        return length

    def _raw_body(self, maximum: int) -> bytes:
        length = self._content_length(maximum)
        return self.rfile.read(length)

    def _json(self, status: int, body: Any) -> None:
        if status == HTTPStatus.NO_CONTENT:
            self.send_response(status)
            self.end_headers()
            return
        payload = json.dumps(body, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _file(self, path: Path, content_type: str, filename: str, *, headers: dict[str, str] | None = None) -> None:
        size = path.stat().st_size
        self.send_response(200)
        self.send_header("Content-Type", content_type or mimetypes.guess_type(filename)[0] or "application/octet-stream")
        self.send_header("Content-Length", str(size))
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        safe_filename = Path(filename).name.replace("\r", "").replace("\n", "").replace('"', "")
        self.send_header(
            "Content-Disposition",
            f"attachment; filename=download; filename*=UTF-8''{quote(safe_filename)}",
        )
        self.end_headers()
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                self.wfile.write(chunk)
