from __future__ import annotations

import base64
import hashlib
import io
import json
import re
import sqlite3
import subprocess
import tempfile
import time
import urllib.parse
import urllib.error
import urllib.request
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any, Callable

from .config import Config
from .catalog import billing_key, public_catalog, validate_request
from .db import Database
from .emailer import EmailDeliveryError, EmailSender
from .errors import ApiError
from .security import (
    hash_password,
    new_id,
    new_token,
    sign_path,
    token_digest,
    verify_password,
)
from .xiangxin import XiangxinClient, XiangxinError
from .image2 import Image2Client, Image2Error
from .network import open_no_redirect, read_limited


EMAIL_PATTERN = re.compile(r"^[^\s@]+@[^\s@]+\.[^\s@]+$")


class SeeCutService:
    def __init__(
        self,
        config: Config,
        database: Database,
        emailer: EmailSender | None = None,
        xiangxin: XiangxinClient | None = None,
        image2: Image2Client | None = None,
        clock: Callable[[], float] = time.time,
    ):
        self.config = config
        self.db = database
        self.emailer = emailer or EmailSender(config)
        self.xiangxin = xiangxin or XiangxinClient(config)
        self.image2 = image2 or Image2Client(config)
        self.clock = clock
        self.config.storage_path.mkdir(parents=True, exist_ok=True)

    def now(self) -> int:
        return int(self.clock())

    def capabilities(self) -> dict[str, Any]:
        return {
            "email": {"provider": self.config.email_provider, "configured": self.emailer.configured},
            "generation": {
                "configured": self.xiangxin.configured or self.image2.configured,
                "catalog_version": public_catalog(self.config.image2_model)["catalog_version"],
            },
            "payment": {
                "provider": "alipay",
                "configured": self.config.alipay_configured(),
                "live_initiation_enabled": self.config.alipay_configured(),
            },
            "storage": {"provider": "local_signed_url"},
        }

    # Accounts and sessions

    def register(self, email: str, password: str) -> dict[str, Any]:
        normalized = email.strip().lower()
        if not EMAIL_PATTERN.match(normalized):
            raise ApiError(400, "INVALID_EMAIL", "请输入有效的邮箱地址")
        try:
            password_hash = hash_password(password)
        except ValueError as exc:
            raise ApiError(400, "INVALID_PASSWORD", "密码长度需要在 10 到 128 个字符之间") from exc
        now = self.now()
        user_id = new_id("usr")
        raw_token = new_token()
        try:
            with self.db.transaction(immediate=True) as connection:
                connection.execute(
                    "INSERT INTO users(id,email,password_hash,created_at,updated_at) VALUES(?,?,?,?,?)",
                    (user_id, normalized, password_hash, now, now),
                )
                connection.execute(
                    "INSERT INTO wallets(user_id,available_credits,held_credits,updated_at) VALUES(?,0,0,?)",
                    (user_id, now),
                )
                self._insert_email_token(connection, user_id, "verify_email", raw_token, now)
        except sqlite3.IntegrityError as exc:
            raise ApiError(409, "EMAIL_ALREADY_REGISTERED", "该邮箱已注册") from exc

        delivery = "sent"
        try:
            self.emailer.send_token(normalized, "verify_email", raw_token)
        except EmailDeliveryError:
            delivery = "not_configured" if not self.emailer.configured else "failed"
        result: dict[str, Any] = {"user_id": user_id, "email": normalized, "email_delivery": delivery}
        if self.config.expose_test_tokens and self.config.env in {"development", "test"}:
            result["verification_token"] = raw_token
        return result

    def verify_email(self, token: str) -> dict[str, Any]:
        now = self.now()
        with self.db.transaction(immediate=True) as connection:
            row = connection.execute(
                "SELECT * FROM email_tokens WHERE token_hash=? AND purpose='verify_email'",
                (token_digest(token),),
            ).fetchone()
            if not row or row["consumed_at"] is not None or row["expires_at"] < now:
                raise ApiError(400, "INVALID_OR_EXPIRED_TOKEN", "验证链接无效或已过期")
            connection.execute("UPDATE email_tokens SET consumed_at=? WHERE id=?", (now, row["id"]))
            connection.execute(
                "UPDATE users SET email_verified_at=?, updated_at=? WHERE id=?",
                (now, now, row["user_id"]),
            )
        return {"verified": True}

    def resend_verification(self, email: str) -> dict[str, Any]:
        normalized = email.strip().lower()
        result: dict[str, Any] = {"accepted": True}
        with self.db.connect() as connection:
            user = connection.execute(
                "SELECT id,email,email_verified_at FROM users WHERE email=?", (normalized,)
            ).fetchone()
        if not user or user["email_verified_at"] is not None:
            return result
        token = new_token()
        now = self.now()
        with self.db.transaction(immediate=True) as connection:
            self._insert_email_token(connection, user["id"], "verify_email", token, now)
        try:
            self.emailer.send_token(user["email"], "verify_email", token)
        except EmailDeliveryError:
            pass
        if self.config.expose_test_tokens and self.config.env in {"development", "test"}:
            result["verification_token"] = token
        return result

    def login(self, email: str, password: str) -> dict[str, Any]:
        normalized = email.strip().lower()
        with self.db.connect() as connection:
            user = connection.execute("SELECT * FROM users WHERE email=?", (normalized,)).fetchone()
        if not user or not verify_password(password, user["password_hash"]):
            raise ApiError(401, "INVALID_CREDENTIALS", "邮箱或密码不正确")
        if user["email_verified_at"] is None:
            raise ApiError(403, "EMAIL_NOT_VERIFIED", "请先验证邮箱")
        raw_token = new_token()
        now = self.now()
        session_id = new_id("ses")
        with self.db.transaction() as connection:
            connection.execute(
                "INSERT INTO sessions(id,user_id,token_hash,expires_at,created_at) VALUES(?,?,?,?,?)",
                (session_id, user["id"], token_digest(raw_token), now + self.config.session_ttl_seconds, now),
            )
        return {
            "access_token": raw_token,
            "token_type": "Bearer",
            "expires_at": now + self.config.session_ttl_seconds,
            "user": {"id": user["id"], "email": user["email"]},
        }

    def logout(self, raw_token: str) -> None:
        with self.db.transaction() as connection:
            connection.execute(
                "UPDATE sessions SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL",
                (self.now(), token_digest(raw_token)),
            )

    def authenticate(self, raw_token: str) -> dict[str, Any]:
        now = self.now()
        with self.db.connect() as connection:
            row = connection.execute(
                """SELECT users.id, users.email, users.email_verified_at
                   FROM sessions JOIN users ON users.id=sessions.user_id
                   WHERE sessions.token_hash=? AND sessions.revoked_at IS NULL
                     AND sessions.expires_at>?""",
                (token_digest(raw_token), now),
            ).fetchone()
        if not row:
            raise ApiError(401, "INVALID_SESSION", "登录已失效，请重新登录")
        return dict(row)

    def forgot_password(self, email: str) -> dict[str, Any]:
        normalized = email.strip().lower()
        with self.db.connect() as connection:
            user = connection.execute("SELECT id,email FROM users WHERE email=?", (normalized,)).fetchone()
        result: dict[str, Any] = {"accepted": True}
        if not user:
            return result
        raw_token = new_token()
        now = self.now()
        with self.db.transaction(immediate=True) as connection:
            self._insert_email_token(connection, user["id"], "reset_password", raw_token, now)
        try:
            self.emailer.send_token(user["email"], "reset_password", raw_token)
        except EmailDeliveryError:
            pass
        if self.config.expose_test_tokens and self.config.env in {"development", "test"}:
            result["reset_token"] = raw_token
        return result

    def reset_password(self, token: str, password: str) -> dict[str, Any]:
        try:
            encoded = hash_password(password)
        except ValueError as exc:
            raise ApiError(400, "INVALID_PASSWORD", "密码长度需要在 10 到 128 个字符之间") from exc
        now = self.now()
        with self.db.transaction(immediate=True) as connection:
            row = connection.execute(
                "SELECT * FROM email_tokens WHERE token_hash=? AND purpose='reset_password'",
                (token_digest(token),),
            ).fetchone()
            if not row or row["consumed_at"] is not None or row["expires_at"] < now:
                raise ApiError(400, "INVALID_OR_EXPIRED_TOKEN", "重置链接无效或已过期")
            connection.execute("UPDATE email_tokens SET consumed_at=? WHERE id=?", (now, row["id"]))
            connection.execute(
                "UPDATE users SET password_hash=?,updated_at=? WHERE id=?",
                (encoded, now, row["user_id"]),
            )
            connection.execute(
                "UPDATE sessions SET revoked_at=? WHERE user_id=? AND revoked_at IS NULL",
                (now, row["user_id"]),
            )
        return {"reset": True}

    def _insert_email_token(
        self, connection: sqlite3.Connection, user_id: str, purpose: str, token: str, now: int
    ) -> None:
        connection.execute(
            "UPDATE email_tokens SET consumed_at=? WHERE user_id=? AND purpose=? AND consumed_at IS NULL",
            (now, user_id, purpose),
        )
        connection.execute(
            "INSERT INTO email_tokens(id,user_id,purpose,token_hash,expires_at,created_at) VALUES(?,?,?,?,?,?)",
            (new_id("emt"), user_id, purpose, token_digest(token), now + 1800, now),
        )

    # Teams and one-time invitations

    def create_team(self, user_id: str, name: str) -> dict[str, Any]:
        clean_name = name.strip()
        if not clean_name or len(clean_name) > 80:
            raise ApiError(400, "INVALID_TEAM_NAME", "团队名称需要 1 到 80 个字符")
        team_id = new_id("team")
        now = self.now()
        with self.db.transaction(immediate=True) as connection:
            connection.execute(
                "INSERT INTO teams(id,name,owner_user_id,created_at) VALUES(?,?,?,?)",
                (team_id, clean_name, user_id, now),
            )
            connection.execute(
                "INSERT INTO team_members(team_id,user_id,role,joined_at) VALUES(?,?,?,?)",
                (team_id, user_id, "owner", now),
            )
        return {"id": team_id, "name": clean_name, "role": "owner", "member_count": 1}

    def list_teams(self, user_id: str) -> list[dict[str, Any]]:
        with self.db.connect() as connection:
            rows = connection.execute(
                """SELECT teams.id,teams.name,team_members.role,
                          (SELECT COUNT(*) FROM team_members m WHERE m.team_id=teams.id) member_count
                   FROM team_members JOIN teams ON teams.id=team_members.team_id
                   WHERE team_members.user_id=? ORDER BY teams.created_at DESC""",
                (user_id,),
            ).fetchall()
        return [dict(row) for row in rows]

    def create_invite(self, user_id: str, team_id: str) -> dict[str, Any]:
        self._require_owner(user_id, team_id)
        token = new_token()
        now = self.now()
        invite_id = new_id("inv")
        with self.db.transaction() as connection:
            connection.execute(
                """INSERT INTO invite_links
                   (id,team_id,creator_user_id,token_hash,expires_at,created_at)
                   VALUES(?,?,?,?,?,?)""",
                (invite_id, team_id, user_id, token_digest(token), now + self.config.invite_ttl_seconds, now),
            )
        return {
            "id": invite_id,
            "token": token,
            "invite_url": f"seecut://team-invite/{token}",
            "expires_at": now + self.config.invite_ttl_seconds,
        }

    def accept_invite(self, user_id: str, token: str) -> dict[str, Any]:
        now = self.now()
        with self.db.transaction(immediate=True) as connection:
            invite = connection.execute(
                "SELECT * FROM invite_links WHERE token_hash=?", (token_digest(token),)
            ).fetchone()
            if (
                not invite
                or invite["consumed_at"] is not None
                or invite["revoked_at"] is not None
                or invite["expires_at"] < now
            ):
                raise ApiError(410, "INVITE_UNAVAILABLE", "邀请链接无效或已过期")
            existing = connection.execute(
                "SELECT 1 FROM team_members WHERE team_id=? AND user_id=?",
                (invite["team_id"], user_id),
            ).fetchone()
            if existing:
                raise ApiError(409, "ALREADY_A_MEMBER", "你已经在这个团队中")
            connection.execute(
                "INSERT INTO team_members(team_id,user_id,role,joined_at) VALUES(?,?,?,?)",
                (invite["team_id"], user_id, "member", now),
            )
            connection.execute(
                "UPDATE invite_links SET consumed_at=?,consumed_by_user_id=? WHERE id=?",
                (now, user_id, invite["id"]),
            )
            team = connection.execute(
                "SELECT id,name FROM teams WHERE id=?", (invite["team_id"],)
            ).fetchone()
        return {"id": team["id"], "name": team["name"], "role": "member"}

    def revoke_invite(self, user_id: str, team_id: str, invite_id: str) -> None:
        self._require_owner(user_id, team_id)
        with self.db.transaction() as connection:
            updated = connection.execute(
                """UPDATE invite_links SET revoked_at=?
                   WHERE id=? AND team_id=? AND consumed_at IS NULL AND revoked_at IS NULL""",
                (self.now(), invite_id, team_id),
            ).rowcount
        if not updated:
            raise ApiError(404, "INVITE_NOT_FOUND", "邀请不存在或已经失效")

    def list_members(self, user_id: str, team_id: str) -> list[dict[str, Any]]:
        self._require_member(user_id, team_id)
        with self.db.connect() as connection:
            rows = connection.execute(
                """SELECT users.id,users.email,team_members.role,team_members.joined_at
                   FROM team_members JOIN users ON users.id=team_members.user_id
                   WHERE team_members.team_id=? ORDER BY team_members.joined_at""",
                (team_id,),
            ).fetchall()
        return [dict(row) for row in rows]

    def remove_member(self, owner_user_id: str, team_id: str, member_user_id: str) -> None:
        self._require_owner(owner_user_id, team_id)
        if owner_user_id == member_user_id:
            raise ApiError(400, "OWNER_CANNOT_BE_REMOVED", "团队创建者不能移除自己")
        with self.db.transaction(immediate=True) as connection:
            removed = connection.execute(
                "DELETE FROM team_members WHERE team_id=? AND user_id=? AND role='member'",
                (team_id, member_user_id),
            ).rowcount
        if not removed:
            raise ApiError(404, "TEAM_MEMBER_NOT_FOUND", "团队成员不存在")

    def _require_member(self, user_id: str, team_id: str) -> str:
        with self.db.connect() as connection:
            row = connection.execute(
                "SELECT role FROM team_members WHERE team_id=? AND user_id=?", (team_id, user_id)
            ).fetchone()
        if not row:
            raise ApiError(403, "TEAM_ACCESS_DENIED", "你没有该团队的访问权限")
        return str(row["role"])

    def _require_owner(self, user_id: str, team_id: str) -> None:
        if self._require_member(user_id, team_id) != "owner":
            raise ApiError(403, "TEAM_OWNER_REQUIRED", "只有团队创建者可以执行此操作")

    # Signed uploads and team asset metadata

    def prepare_upload(
        self,
        user_id: str,
        purpose: str,
        filename: str,
        content_type: str,
        size_bytes: int,
        sha256: str | None = None,
        team_id: str | None = None,
    ) -> dict[str, Any]:
        if purpose not in {"team_asset", "generation_input"}:
            raise ApiError(400, "INVALID_UPLOAD_PURPOSE", "上传用途无效")
        if purpose == "team_asset":
            if not team_id:
                raise ApiError(400, "TEAM_REQUIRED", "保存团队资产时必须指定团队")
            self._require_member(user_id, team_id)
        elif team_id is not None:
            raise ApiError(400, "TEAM_NOT_ALLOWED", "生成临时素材不属于团队资产")
        clean_name = Path(filename).name.strip()
        if not clean_name or len(clean_name) > 255:
            raise ApiError(400, "INVALID_FILENAME", "文件名无效")
        if not isinstance(size_bytes, int) or size_bytes <= 0 or size_bytes > self.config.max_upload_bytes:
            raise ApiError(400, "INVALID_FILE_SIZE", "文件大小超出允许范围")
        if not content_type.startswith(("image/", "video/", "audio/")):
            raise ApiError(400, "UNSUPPORTED_MEDIA_TYPE", "仅支持图片、视频和音频")
        if (
            purpose == "generation_input"
            and content_type.startswith("image/")
            and size_bytes > self.config.max_reference_image_bytes
        ):
            raise ApiError(413, "REFERENCE_IMAGE_TOO_LARGE", "参考图片单文件不能超过 20MB")
        if sha256 and not re.fullmatch(r"[a-fA-F0-9]{64}", sha256):
            raise ApiError(400, "INVALID_SHA256", "文件摘要格式无效")
        upload_id = new_id("upl")
        scope = "team" if purpose == "team_asset" else "staging"
        object_key = f"{scope}/{upload_id}/{clean_name}"
        now = self.now()
        expires_at = now + self.config.upload_ttl_seconds
        with self.db.transaction() as connection:
            connection.execute(
                """INSERT INTO uploads
                   (id,owner_user_id,team_id,purpose,object_key,filename,content_type,
                    expected_size,sha256,expires_at,created_at)
                   VALUES(?,?,?,?,?,?,?,?,?,?,?)""",
                (
                    upload_id,
                    user_id,
                    team_id,
                    purpose,
                    object_key,
                    clean_name,
                    content_type,
                    size_bytes,
                    sha256.lower() if sha256 else None,
                    expires_at,
                    now,
                ),
            )
        path = f"/api/storage/uploads/{upload_id}"
        signature = sign_path(self.config.signing_secret, path, expires_at)
        return {
            "upload_id": upload_id,
            "method": "PUT",
            "upload_url": f"{self.config.public_base_url}{path}?expires={expires_at}&signature={signature}",
            "expires_at": expires_at,
            "required_headers": {"Content-Type": content_type},
        }

    def save_upload(self, upload_id: str, body: bytes, content_type: str) -> None:
        self.save_upload_stream(upload_id, io.BytesIO(body), len(body), content_type)

    def save_upload_stream(
        self, upload_id: str, stream: Any, content_length: int, content_type: str
    ) -> None:
        now = self.now()
        claim = new_token()
        with self.db.connect() as connection:
            row = connection.execute("SELECT * FROM uploads WHERE id=?", (upload_id,)).fetchone()
        if not row or row["expires_at"] < now or row["completed_at"] is not None:
            raise ApiError(410, "UPLOAD_UNAVAILABLE", "上传地址无效或已过期")
        if row["content_type"] != content_type:
            raise ApiError(400, "CONTENT_TYPE_MISMATCH", "文件类型与申请上传时不一致")
        if content_length != row["expected_size"]:
            raise ApiError(400, "FILE_SIZE_MISMATCH", "文件大小与申请上传时不一致")
        with self.db.transaction(immediate=True) as connection:
            claimed = connection.execute(
                """UPDATE uploads SET write_token=? WHERE id=? AND completed_at IS NULL
                   AND write_token IS NULL AND expires_at>=?""", (claim, upload_id, now)
            ).rowcount
            if not claimed:
                raise ApiError(409, "UPLOAD_IN_PROGRESS", "文件正在上传")
        target = self.config.storage_path / row["object_key"]
        target.parent.mkdir(parents=True, exist_ok=True)
        temp_target = target.with_suffix(target.suffix + ".part." + token_digest(claim)[:12])
        digest = hashlib.sha256()
        remaining = content_length
        try:
            with temp_target.open("wb") as output:
                while remaining:
                    chunk = stream.read(min(1024 * 1024, remaining))
                    if not chunk:
                        raise ApiError(400, "INCOMPLETE_UPLOAD", "文件上传未完成")
                    output.write(chunk)
                    digest.update(chunk)
                    remaining -= len(chunk)
            actual_sha256 = digest.hexdigest()
            if row["sha256"] and row["sha256"] != actual_sha256:
                raise ApiError(400, "FILE_HASH_MISMATCH", "文件校验失败")
        except Exception:
            temp_target.unlink(missing_ok=True)
            with self.db.transaction(immediate=True) as connection:
                connection.execute(
                    "UPDATE uploads SET write_token=NULL WHERE id=? AND write_token=? AND completed_at IS NULL",
                    (upload_id, claim),
                )
            raise
        temp_target.replace(target)
        with self.db.transaction(immediate=True) as connection:
            updated = connection.execute(
                "UPDATE uploads SET completed_at=?,sha256=?,write_token=NULL WHERE id=? AND completed_at IS NULL AND write_token=?",
                (now, actual_sha256, upload_id, claim),
            ).rowcount
        if not updated:
            target.unlink(missing_ok=True)
            raise ApiError(409, "UPLOAD_ALREADY_COMPLETED", "上传已经完成")

    def complete_team_asset(self, user_id: str, team_id: str, upload_id: str) -> dict[str, Any]:
        self._require_member(user_id, team_id)
        now = self.now()
        asset_id = new_id("ast")
        with self.db.transaction(immediate=True) as connection:
            upload = connection.execute(
                """SELECT * FROM uploads WHERE id=? AND owner_user_id=? AND team_id=?
                   AND purpose='team_asset'""",
                (upload_id, user_id, team_id),
            ).fetchone()
            if not upload or upload["completed_at"] is None:
                raise ApiError(400, "UPLOAD_NOT_COMPLETED", "请先完成文件上传")
            try:
                connection.execute(
                    """INSERT INTO team_assets
                       (id,team_id,uploader_user_id,upload_id,object_key,filename,
                        content_type,size_bytes,sha256,created_at)
                       VALUES(?,?,?,?,?,?,?,?,?,?)""",
                    (
                        asset_id,
                        team_id,
                        user_id,
                        upload_id,
                        upload["object_key"],
                        upload["filename"],
                        upload["content_type"],
                        upload["expected_size"],
                        upload["sha256"],
                        now,
                    ),
                )
            except sqlite3.IntegrityError:
                existing = connection.execute(
                    "SELECT * FROM team_assets WHERE upload_id=?", (upload_id,)
                ).fetchone()
                return self._asset_dict(existing)
            asset = connection.execute("SELECT * FROM team_assets WHERE id=?", (asset_id,)).fetchone()
        return self._asset_dict(asset)

    def list_assets(self, user_id: str, team_id: str, trash: bool = False) -> list[dict[str, Any]]:
        self._require_member(user_id, team_id)
        with self.db.connect() as connection:
            rows = connection.execute(
                """SELECT team_assets.*,users.email uploader_email
                   FROM team_assets JOIN users ON users.id=team_assets.uploader_user_id
                   WHERE team_id=? AND deleted_at IS %s ORDER BY created_at DESC""" % ("NOT NULL" if trash else "NULL"),
                (team_id,),
            ).fetchall()
        return [self._asset_dict(row) for row in rows]

    def asset_download(self, user_id: str, team_id: str, asset_id: str) -> dict[str, Any]:
        self._require_member(user_id, team_id)
        with self.db.connect() as connection:
            row = connection.execute(
                "SELECT * FROM team_assets WHERE id=? AND team_id=? AND deleted_at IS NULL",
                (asset_id, team_id),
            ).fetchone()
        if not row:
            raise ApiError(404, "ASSET_NOT_FOUND", "资产不存在")
        expires_at = self.now() + 600
        path = f"/api/storage/assets/{asset_id}"
        signature = sign_path(self.config.signing_secret, path, expires_at)
        return {
            "download_url": f"/api/teams/{team_id}/assets/{asset_id}/content",
            "expires_at": expires_at,
        }

    def delete_asset(self, user_id: str, team_id: str, asset_id: str) -> None:
        self._require_owner(user_id, team_id)
        with self.db.transaction(immediate=True) as connection:
            updated = connection.execute(
                "UPDATE team_assets SET deleted_at=? WHERE id=? AND team_id=? AND deleted_at IS NULL",
                (self.now(), asset_id, team_id),
            ).rowcount
        if not updated:
            raise ApiError(404, "ASSET_NOT_FOUND", "资产不存在")

    def restore_asset(self, user_id: str, team_id: str, asset_id: str) -> dict[str, Any]:
        self._require_owner(user_id, team_id)
        with self.db.transaction(immediate=True) as connection:
            updated = connection.execute(
                "UPDATE team_assets SET deleted_at=NULL WHERE id=? AND team_id=? AND deleted_at IS NOT NULL",
                (asset_id, team_id),
            ).rowcount
            row = connection.execute("SELECT * FROM team_assets WHERE id=? AND team_id=?", (asset_id, team_id)).fetchone()
        if not updated or not row:
            raise ApiError(404, "ASSET_NOT_FOUND", "回收站中不存在该资产")
        return self._asset_dict(row)

    def storage_asset(self, team_id: str, asset_id: str) -> tuple[Path, str, str]:
        with self.db.connect() as connection:
            row = connection.execute(
                """SELECT object_key,content_type,filename FROM team_assets
                   WHERE id=? AND team_id=? AND deleted_at IS NULL""",
                (asset_id, team_id),
            ).fetchone()
        if not row:
            raise ApiError(404, "ASSET_NOT_FOUND", "资产不存在")
        path = self.config.storage_path / row["object_key"]
        if not path.is_file():
            raise ApiError(404, "ASSET_FILE_MISSING", "资产文件不可用")
        return path, row["content_type"], row["filename"]

    def storage_upload(self, upload_id: str) -> tuple[Path, str, str]:
        with self.db.connect() as connection:
            row = connection.execute(
                """SELECT object_key,content_type,filename,expires_at,completed_at
                   FROM uploads WHERE id=? AND purpose='generation_input'""",
                (upload_id,),
            ).fetchone()
        if not row or row["completed_at"] is None or row["expires_at"] < self.now():
            raise ApiError(404, "STAGING_FILE_NOT_FOUND", "临时素材不存在或已过期")
        path = self.config.storage_path / row["object_key"]
        if not path.is_file():
            raise ApiError(404, "STAGING_FILE_MISSING", "临时素材文件不可用")
        return path, row["content_type"], row["filename"]

    def register_generation_asset(self, user_id: str, upload_id: str) -> dict[str, Any]:
        with self.db.connect() as connection:
            row = connection.execute(
                """SELECT id,expires_at,completed_at FROM uploads
                   WHERE id=? AND owner_user_id=? AND purpose='generation_input'""",
                (upload_id, user_id),
            ).fetchone()
        if not row or row["completed_at"] is None or row["expires_at"] < self.now():
            raise ApiError(400, "STAGING_UPLOAD_UNAVAILABLE", "临时素材不存在或尚未上传完成")
        generation_asset_id = new_id("gasset")
        expires_at = min(row["expires_at"], self.now() + 1800)
        with self.db.transaction(immediate=True) as connection:
            connection.execute(
                """INSERT OR IGNORE INTO generation_assets
                   (id,user_id,upload_id,provider_asset_id,expires_at,created_at)
                   VALUES(?,?,?,?,?,?)""",
                (generation_asset_id, user_id, upload_id, None, expires_at, self.now()),
            )
        with self.db.connect() as connection:
            row = connection.execute(
                """SELECT id,expires_at FROM generation_assets
                   WHERE user_id=? AND upload_id=?""", (user_id, upload_id)
            ).fetchone()
        return {"id": row["id"], "upload_id": upload_id, "expires_at": row["expires_at"]}

    def _asset_dict(self, row: sqlite3.Row) -> dict[str, Any]:
        result = {
            "id": row["id"],
            "team_id": row["team_id"],
            "filename": row["filename"],
            "content_type": row["content_type"],
            "size_bytes": row["size_bytes"],
            "sha256": row["sha256"],
            "uploader_user_id": row["uploader_user_id"],
            "created_at": row["created_at"],
        }
        if "uploader_email" in row.keys():
            result["uploader_email"] = row["uploader_email"]
        return result

    # Wallets, credit plans and Alipay notifications

    def wallet(self, user_id: str) -> dict[str, Any]:
        with self.db.connect() as connection:
            wallet = connection.execute("SELECT * FROM wallets WHERE user_id=?", (user_id,)).fetchone()
            ledger = connection.execute(
                """SELECT id,kind,delta_available,delta_held,reference_type,reference_id,created_at
                   FROM ledger_entries WHERE user_id=? ORDER BY created_at DESC LIMIT 100""",
                (user_id,),
            ).fetchall()
        return {
            "available_credits": wallet["available_credits"],
            "held_credits": wallet["held_credits"],
            "ledger": [dict(row) for row in ledger],
        }

    def list_plans(self) -> list[dict[str, Any]]:
        with self.db.connect() as connection:
            rows = connection.execute(
                "SELECT id,name,price_fen,credits FROM plans WHERE active=1 ORDER BY price_fen"
            ).fetchall()
        return [dict(row) for row in rows]

    def create_order(self, user_id: str, plan_id: str) -> dict[str, Any]:
        if not self.config.alipay_configured():
            raise ApiError(
                503,
                "ALIPAY_NOT_CONFIGURED",
                "支付宝商户配置尚未完成",
                {"required": ["app_id", "notify_url", "public_key", "merchant_private_key"]},
            )
        with self.db.connect() as connection:
            plan = connection.execute(
                "SELECT * FROM plans WHERE id=? AND active=1", (plan_id,)
            ).fetchone()
        if not plan:
            raise ApiError(404, "PLAN_NOT_FOUND", "积分套餐不存在")
        order_id = new_id("ord")
        now = self.now()
        with self.db.transaction() as connection:
            connection.execute(
                """INSERT INTO orders
                   (id,user_id,plan_id,provider,amount_fen,credits,status,created_at,updated_at)
                   VALUES(?,?,?,'alipay',?,?, 'pending',?,?)""",
                (order_id, user_id, plan_id, plan["price_fen"], plan["credits"], now, now),
            )
        payment_url = self._alipay_page_url(order_id, plan["price_fen"], plan["name"])
        return {
            "id": order_id,
            "status": "pending",
            "provider": "alipay",
            "amount_fen": plan["price_fen"],
            "credits": plan["credits"],
            "payment_action": {"type": "open_url", "url": payment_url, "expires_at": now + 1800},
        }

    def _alipay_page_url(self, order_id: str, amount_fen: int, subject: str) -> str:
        if not self.config.alipay_configured():
            raise ApiError(503, "ALIPAY_NOT_CONFIGURED", "支付宝商户配置尚未完成")
        params = {
            "app_id": self.config.alipay_app_id,
            "method": "alipay.trade.page.pay",
            "format": "JSON",
            "charset": "utf-8",
            "sign_type": "RSA2",
            "timestamp": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(self.now())),
            "version": "1.0",
            "notify_url": self.config.alipay_notify_url,
            "biz_content": json.dumps(
                {
                    "out_trade_no": order_id,
                    "product_code": "FAST_INSTANT_TRADE_PAY",
                    "total_amount": f"{amount_fen / 100:.2f}",
                    "subject": subject,
                },
                ensure_ascii=False,
                separators=(",", ":"),
            ),
        }
        sign_content = "&".join(f"{key}={params[key]}" for key in sorted(params))
        try:
            result = subprocess.run(
                ["openssl", "dgst", "-sha256", "-sign", self.config.alipay_merchant_private_key_path],
                input=sign_content.encode("utf-8"),
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=5,
                check=True,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            raise ApiError(503, "ALIPAY_SIGNING_UNAVAILABLE", "支付宝签名服务暂不可用") from exc
        params["sign"] = base64.b64encode(result.stdout).decode("ascii")
        return self.config.alipay_gateway + "?" + urllib.parse.urlencode(params)

    def order(self, user_id: str, order_id: str) -> dict[str, Any]:
        with self.db.connect() as connection:
            row = connection.execute(
                """SELECT id,provider,amount_fen,credits,status,provider_trade_no,paid_at,created_at
                   FROM orders WHERE id=? AND user_id=?""",
                (order_id, user_id),
            ).fetchone()
        if not row:
            raise ApiError(404, "ORDER_NOT_FOUND", "订单不存在")
        return dict(row)

    def orders(self, user_id: str) -> dict[str, Any]:
        with self.db.connect() as connection:
            rows = connection.execute(
                "SELECT id,provider,amount_fen,credits,status,paid_at,created_at FROM orders WHERE user_id=? ORDER BY created_at DESC LIMIT 100",
                (user_id,),
            ).fetchall()
        return {"items": [dict(row) for row in rows]}

    def refresh_order(self, user_id: str, order_id: str) -> dict[str, Any]:
        order = self.order(user_id, order_id)
        if order["status"] == "paid":
            return order
        params = self._alipay_api_params(
            "alipay.trade.query", {"out_trade_no": order_id}
        )
        request = urllib.request.Request(
            self.config.alipay_gateway + "?" + urllib.parse.urlencode(params),
            headers={"Accept": "application/json"},
        )
        try:
            with open_no_redirect(request, timeout=15) as response:
                raw_response = read_limited(response, self.config.max_provider_response_bytes).decode("utf-8")
                payload = json.loads(raw_response)
        except (urllib.error.URLError, TimeoutError, ValueError) as exc:
            raise ApiError(502, "ALIPAY_QUERY_FAILED", "暂时无法查询支付宝订单") from exc
        signed_content = self._extract_json_object(raw_response, "alipay_trade_query_response")
        if not isinstance(payload, dict) or not self._verify_alipay_response_signature(payload, signed_content):
            raise ApiError(502, "ALIPAY_QUERY_SIGNATURE_INVALID", "支付宝查询响应验签失败")
        result = payload.get("alipay_trade_query_response", {}) if isinstance(payload, dict) else {}
        if result.get("trade_status") in {"TRADE_SUCCESS", "TRADE_FINISHED"}:
            if result.get("code") != "10000" or not result.get("trade_no"):
                raise ApiError(502, "ALIPAY_QUERY_INVALID", "支付宝查询结果无效")
            try:
                amount = Decimal(str(result.get("total_amount"))) * 100
                if amount != amount.to_integral_value():
                    raise InvalidOperation
                amount_fen = int(amount)
            except (InvalidOperation, ValueError) as exc:
                raise ApiError(502, "ALIPAY_QUERY_INVALID", "支付宝订单金额无效") from exc
            if (
                amount_fen != order["amount_fen"]
                or result.get("out_trade_no") != order_id
                or (result.get("app_id") and result.get("app_id") != self.config.alipay_app_id)
                or (result.get("seller_id") and result.get("seller_id") != self.config.alipay_seller_id)
            ):
                raise ApiError(400, "PAYMENT_ORDER_MISMATCH", "支付宝查询结果与订单不匹配")
            self._settle_alipay_order(
                order_id,
                str(result.get("trade_no", "")),
                f"query:{result.get('trade_no', '')}",
                payload,
            )
        return self.order(user_id, order_id)

    def _alipay_api_params(self, method: str, biz_content: dict[str, Any]) -> dict[str, str]:
        if not self.config.alipay_configured():
            raise ApiError(503, "ALIPAY_NOT_CONFIGURED", "支付宝商户配置尚未完成")
        params = {
            "app_id": self.config.alipay_app_id,
            "method": method,
            "format": "JSON",
            "charset": "utf-8",
            "sign_type": "RSA2",
            "timestamp": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(self.now())),
            "version": "1.0",
            "biz_content": json.dumps(biz_content, ensure_ascii=False, separators=(",", ":")),
        }
        content = "&".join(f"{key}={params[key]}" for key in sorted(params))
        try:
            result = subprocess.run(
                ["openssl", "dgst", "-sha256", "-sign", self.config.alipay_merchant_private_key_path],
                input=content.encode("utf-8"), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                timeout=5, check=True,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            raise ApiError(503, "ALIPAY_SIGNING_UNAVAILABLE", "支付宝签名服务暂不可用") from exc
        params["sign"] = base64.b64encode(result.stdout).decode("ascii")
        return params

    def process_alipay_notification(self, fields: dict[str, str]) -> str:
        if not self.config.alipay_configured():
            raise ApiError(503, "ALIPAY_NOT_CONFIGURED", "支付宝商户配置尚未完成")
        signature = fields.get("sign", "")
        if fields.get("sign_type") != "RSA2":
            raise ApiError(400, "INVALID_PAYMENT_SIGN_TYPE", "支付通知签名类型无效")
        if not signature or not self._verify_alipay_signature(fields, signature):
            raise ApiError(400, "INVALID_PAYMENT_SIGNATURE", "支付通知验签失败")
        order_id = fields.get("out_trade_no", "")
        trade_no = fields.get("trade_no", "")
        status = fields.get("trade_status", "")
        if fields.get("app_id") != self.config.alipay_app_id:
            raise ApiError(400, "PAYMENT_APP_MISMATCH", "支付通知应用不匹配")
        if fields.get("seller_id") != self.config.alipay_seller_id:
            raise ApiError(400, "PAYMENT_SELLER_MISMATCH", "支付通知商户不匹配")
        if status not in {"TRADE_SUCCESS", "TRADE_FINISHED"}:
            return "success"
        try:
            amount_in_fen = Decimal(fields.get("total_amount", "")) * 100
            if amount_in_fen != amount_in_fen.to_integral_value():
                raise InvalidOperation
            amount_fen = int(amount_in_fen)
        except (InvalidOperation, ValueError):
            raise ApiError(400, "INVALID_PAYMENT_AMOUNT", "支付金额无效")
        with self.db.connect() as connection:
            order = connection.execute("SELECT * FROM orders WHERE id=?", (order_id,)).fetchone()
        if not order or order["provider"] != "alipay" or order["amount_fen"] != amount_fen:
            raise ApiError(400, "PAYMENT_ORDER_MISMATCH", "支付通知与订单不匹配")
        self._settle_alipay_order(order_id, trade_no, trade_no, fields)
        return "success"

    def _settle_alipay_order(
        self, order_id: str, trade_no: str, event_id: str, payload: dict[str, Any]
    ) -> None:
        if not trade_no:
            raise ApiError(400, "PAYMENT_TRADE_NUMBER_MISSING", "支付宝交易号缺失")
        now = self.now()
        with self.db.transaction(immediate=True) as connection:
            order = connection.execute("SELECT * FROM orders WHERE id=?", (order_id,)).fetchone()
            if not order:
                raise ApiError(400, "PAYMENT_ORDER_MISMATCH", "支付订单不存在")
            try:
                connection.execute(
                    """INSERT INTO payment_events
                       (id,provider,provider_event_id,order_id,verified,payload_sha256,created_at)
                       VALUES(?,'alipay',?,?,1,?,?)""",
                    (
                        new_id("payevt"),
                        event_id,
                        order_id,
                        hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest(),
                        now,
                    ),
                )
            except sqlite3.IntegrityError:
                return
            if order["status"] == "paid":
                return
            connection.execute(
                "UPDATE orders SET status='paid',provider_trade_no=?,paid_at=?,updated_at=? WHERE id=?",
                (trade_no, now, now, order_id),
            )
            connection.execute(
                "UPDATE wallets SET available_credits=available_credits+?,updated_at=? WHERE user_id=?",
                (order["credits"], now, order["user_id"]),
            )
            connection.execute(
                """INSERT INTO ledger_entries
                   (id,user_id,kind,delta_available,delta_held,reference_type,reference_id,idempotency_key,created_at)
                   VALUES(?,?,'purchase',?,0,'order',?,?,?)""",
                (
                    new_id("led"),
                    order["user_id"],
                    order["credits"],
                    order_id,
                    f"alipay:{trade_no}",
                    now,
                ),
            )

    def _verify_alipay_signature(self, fields: dict[str, str], signature: str) -> bool:
        signed = "&".join(
            f"{key}={fields[key]}" for key in sorted(fields) if key not in {"sign", "sign_type"} and fields[key]
        )
        return self._verify_alipay_signature_content(signed, signature)

    def _verify_alipay_response_signature(
        self, payload: dict[str, Any], signed_content: str | None = None
    ) -> bool:
        signature = payload.get("sign")
        response = payload.get("alipay_trade_query_response")
        if not isinstance(signature, str) or not signature or not isinstance(response, dict):
            return False
        sign_type = payload.get("sign_type")
        if sign_type is not None and sign_type != "RSA2":
            return False
        signed = signed_content or json.dumps(response, ensure_ascii=False, separators=(",", ":"))
        return self._verify_alipay_signature_content(signed, signature)

    @staticmethod
    def _extract_json_object(raw_json: str, key: str) -> str | None:
        match = re.search(rf'"{re.escape(key)}"\s*:\s*', raw_json)
        if not match:
            return None
        value_start = match.end()
        try:
            value, value_end = json.JSONDecoder().raw_decode(raw_json[value_start:])
        except (TypeError, ValueError):
            return None
        if not isinstance(value, dict):
            return None
        return raw_json[value_start : value_start + value_end]

    def _verify_alipay_signature_content(self, signed: str, signature: str) -> bool:
        try:
            signature_bytes = base64.b64decode(signature, validate=True)
        except ValueError:
            return False
        try:
            with tempfile.NamedTemporaryFile() as signature_file:
                signature_file.write(signature_bytes)
                signature_file.flush()
                result = subprocess.run(
                    [
                        "openssl",
                        "dgst",
                        "-sha256",
                        "-verify",
                        self.config.alipay_public_key_path,
                        "-signature",
                        signature_file.name,
                    ],
                    input=signed.encode("utf-8"),
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=5,
                    check=False,
                )
            return result.returncode == 0
        except (OSError, subprocess.SubprocessError):
            return False

    # Xiangxin generation gateway and wallet holds

    def generation_capabilities(self) -> dict[str, Any]:
        return public_catalog(self.config.image2_model)

    def generation_models(self) -> dict[str, Any]:
        # The upstream catalog is diagnostic only. Client parameters come from
        # our static whitelist and never from provider IDs alone.
        return public_catalog(self.config.image2_model)

    def quote_generation(
        self, user_id: str, body_or_kind: dict[str, Any] | str, legacy_body: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        if isinstance(body_or_kind, str):
            kind = body_or_kind
            body = dict(legacy_body or {})
        else:
            body = dict(body_or_kind)
            kind = str(body.get("kind", ""))
        request = dict(body)
        request.pop("kind", None)
        model, normalized = validate_request(kind, request, self.config.image2_model)
        self._validate_generation_assets(user_id, normalized.get("reference_asset_ids", []))
        key = billing_key(model, normalized)
        credits = self.config.model_prices.get(key)
        if credits is None and self.config.env in {"development", "test"}:
            credits = self.config.model_prices.get(f"{kind}:{model['id']}")
        if credits is None:
            raise ApiError(422, "MODEL_PRICE_NOT_CONFIGURED", "该模型或参数组合尚未配置积分价格")
        now = self.now()
        quote_id = new_id("quote")
        expires_at = now + 600
        with self.db.transaction() as connection:
            connection.execute(
                """INSERT INTO generation_quotes
                   (id,user_id,kind,model,canonical_json,billing_key,credits,expires_at,created_at)
                   VALUES(?,?,?,?,?,?,?,?,?)""",
                (
                    quote_id,
                    user_id,
                    kind,
                    model["id"],
                    json.dumps(normalized, ensure_ascii=False, sort_keys=True),
                    key,
                    credits,
                    expires_at,
                    now,
                ),
            )
        return {
            "quote_id": quote_id,
            "credits": credits,
            "currency": "credits",
            "expires_at": expires_at,
            "billing_key": key,
            "request": normalized,
        }

    def create_generation_task(
        self,
        user_id: str,
        kind: str,
        payload: dict[str, Any],
        idempotency_key: str,
    ) -> dict[str, Any]:
        if not idempotency_key or len(idempotency_key) > 128:
            raise ApiError(400, "INVALID_IDEMPOTENCY_KEY", "请提供有效的幂等键")
        quote_id = str(payload.get("quote_id", ""))
        if not quote_id:
            raise ApiError(422, "QUOTE_REQUIRED", "请先获取当前参数的积分报价")
        request = dict(payload)
        request.pop("quote_id", None)
        request.pop("kind", None)
        model, normalized = validate_request(kind, request, self.config.image2_model)
        self._validate_generation_assets(user_id, normalized.get("reference_asset_ids", []))
        canonical_json = json.dumps(normalized, ensure_ascii=False, sort_keys=True)
        operation = normalized["operation"]
        now = self.now()
        task_id = new_id("gen")
        with self.db.transaction(immediate=True) as connection:
            existing = connection.execute(
                "SELECT * FROM generation_tasks WHERE user_id=? AND idempotency_key=?",
                (user_id, idempotency_key),
            ).fetchone()
            if existing:
                if existing["kind"] != kind or existing["request_json"] != canonical_json:
                    raise ApiError(
                        409,
                        "IDEMPOTENCY_CONFLICT",
                        "该幂等键已用于不同的生成参数",
                    )
                return self._task_dict(connection, existing)
            quote = connection.execute(
                "SELECT * FROM generation_quotes WHERE id=? AND user_id=?",
                (quote_id, user_id),
            ).fetchone()
            if (
                not quote
                or quote["consumed_at"] is not None
                or quote["expires_at"] < now
                or quote["kind"] != kind
                or quote["model"] != model["id"]
                or quote["canonical_json"] != canonical_json
            ):
                raise ApiError(409, "QUOTE_MISMATCH", "报价已过期或与当前参数不一致")
            credits = quote["credits"]
            wallet = connection.execute(
                "SELECT * FROM wallets WHERE user_id=?", (user_id,)
            ).fetchone()
            if wallet["available_credits"] < credits:
                raise ApiError(409, "INSUFFICIENT_CREDITS", "积分不足，请先充值")
            connection.execute(
                """INSERT INTO generation_tasks
                   (id,user_id,kind,provider,operation,model,prompt,request_json,idempotency_key,
                    quoted_credits,status,created_at,updated_at,next_attempt_at,attempt_count)
                   VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,0)""",
                (
                    task_id,
                    user_id,
                    kind,
                    model["provider"],
                    operation,
                    model["id"],
                    normalized["prompt"],
                    canonical_json,
                    idempotency_key,
                    credits,
                    "queued",
                    now,
                    now,
                    now,
                ),
            )
            consumed = connection.execute(
                "UPDATE generation_quotes SET consumed_at=? WHERE id=? AND consumed_at IS NULL",
                (now, quote_id),
            ).rowcount
            if consumed != 1:
                raise ApiError(409, "QUOTE_MISMATCH", "报价已被使用")
            connection.execute(
                "UPDATE wallets SET available_credits=available_credits-?,held_credits=held_credits+?,updated_at=? WHERE user_id=?",
                (credits, credits, now, user_id),
            )
            hold_id = new_id("hold")
            connection.execute(
                "INSERT INTO wallet_holds(id,user_id,task_id,credits,state,created_at,updated_at) VALUES(?,?,?,?, 'held',?,?)",
                (hold_id, user_id, task_id, credits, now, now),
            )
            connection.execute(
                """INSERT INTO ledger_entries
                   (id,user_id,kind,delta_available,delta_held,reference_type,reference_id,idempotency_key,created_at)
                   VALUES(?,?,'hold',?,?,'generation_task',?,?,?)""",
                (new_id("led"), user_id, -credits, credits, task_id, f"hold:{task_id}", now),
            )

        return self.get_generation_task(user_id, task_id)

    def _validate_generation_assets(self, user_id: str, asset_ids: list[str]) -> None:
        if not asset_ids:
            return
        with self.db.connect() as connection:
            rows = connection.execute(
                "SELECT id,expires_at FROM generation_assets WHERE user_id=? AND id IN (%s)"
                % ",".join("?" for _ in asset_ids),
                (user_id, *asset_ids),
            ).fetchall()
        if len(rows) != len(set(asset_ids)) or any(row["expires_at"] < self.now() for row in rows):
            raise ApiError(422, "GENERATION_ASSET_UNAVAILABLE", "参考素材不存在或已过期")

    def claim_generation_tasks(self, limit: int = 1) -> list[dict[str, Any]]:
        limit = 1
        now = self.now()
        lease_owner = new_id("worker")
        lease_seconds = max(self.config.provider_timeout_seconds + 60, 180)
        with self.db.transaction(immediate=True) as connection:
            # A process can disappear after sending a paid POST but before
            # storing its response. That ambiguity must never trigger a retry.
            connection.execute(
                """UPDATE generation_tasks
                   SET status='pending_reconcile',error_code='SUBMISSION_RESULT_UNKNOWN',
                       error_message='提交结果待核实',lease_owner=NULL,lease_expires_at=NULL,
                       next_attempt_at=?,updated_at=?
                   WHERE status='submitting' AND lease_expires_at<?""",
                (now + 300, now, now),
            )
            rows = connection.execute(
                """SELECT * FROM generation_tasks
                   WHERE (status='queued'
                          OR (kind='video' AND status IN ('provider_accepted','processing','validating'))
                          OR (kind='video' AND status='pending_reconcile' AND upstream_task_id IS NOT NULL))
                     AND next_attempt_at<=?
                     AND (lease_expires_at IS NULL OR lease_expires_at<?)
                   ORDER BY created_at LIMIT ?""",
                (now, now, limit),
            ).fetchall()
            for row in rows:
                connection.execute(
                    """UPDATE generation_tasks SET lease_owner=?,lease_expires_at=?,attempt_count=attempt_count+1
                       WHERE id=?""",
                    (lease_owner, now + lease_seconds, row["id"]),
                )
        return [dict(row) | {"lease_owner": lease_owner} for row in rows]

    def process_generation_task(self, task: dict[str, Any]) -> None:
        task_id = task["id"]
        try:
            if task["kind"] == "image" and task["status"] == "queued":
                self._submit_image_task(task)
            elif task["kind"] == "video" and task["status"] == "queued":
                self._submit_video_task(task)
            elif task["kind"] == "video" and task["status"] in {"provider_accepted", "processing", "validating", "pending_reconcile"}:
                self._refresh_video_by_task(task)
        except (XiangxinError, Image2Error) as exc:
            if task["kind"] == "video" and task["status"] != "queued":
                self._mark_provider_retry(task_id, exc, retry_poll=True)
            elif getattr(exc, "retryable", False):
                self._mark_provider_retry(task_id, exc)
            else:
                self._fail_and_release(task_id, exc.code, exc.message)
        except ApiError as exc:
            if task["kind"] == "video" and task["status"] != "queued":
                self._mark_provider_retry(task_id, exc, retry_poll=True)
            else:
                self._fail_and_release(task_id, exc.code, exc.message)
        except Exception as exc:
            self._mark_provider_retry(task_id, type("ProviderError", (), {"code": "WORKER_ERROR", "message": str(exc)})())

    def _task_payload(self, task: dict[str, Any]) -> dict[str, Any]:
        payload = json.loads(task["request_json"])
        payload.pop("operation", None)
        reference_ids = payload.pop("reference_asset_ids", [])
        if reference_ids:
            with self.db.connect() as connection:
                rows = connection.execute(
                    "SELECT id,upload_id,provider_asset_id,expires_at FROM generation_assets WHERE user_id=? AND id IN (%s)"
                    % ",".join("?" for _ in reference_ids),
                    (task["user_id"], *reference_ids),
                ).fetchall()
            if len(rows) != len(reference_ids) or any(row["expires_at"] < self.now() for row in rows):
                raise ApiError(422, "GENERATION_ASSET_EXPIRED", "参考素材已过期，请重新上传")
            rows_by_id = {row["id"]: row for row in rows}
            rows = [rows_by_id[asset_id] for asset_id in reference_ids]
            provider_ids: list[str] = []
            for row in rows:
                provider_id = row["provider_asset_id"]
                if not provider_id:
                    path = f"/api/storage/staging/{row['upload_id']}"
                    expires_at = min(row["expires_at"], self.now() + 1800)
                    signature = sign_path(self.config.signing_secret, path, expires_at)
                    source_url = f"{self.config.public_base_url}{path}?expires={expires_at}&signature={signature}"
                    upstream = self.xiangxin.register_asset(source_url)
                    data = upstream.get("data") if isinstance(upstream.get("data"), dict) else {}
                    provider_id = (
                        upstream.get("assetId")
                        or upstream.get("asset_id")
                        or upstream.get("id")
                        or data.get("assetId")
                        or data.get("asset_id")
                        or data.get("id")
                    )
                    if not isinstance(provider_id, str) or not provider_id:
                        raise XiangxinError("XIANGXIN_INVALID_RESPONSE", "素材登记响应缺少素材编号", True)
                    with self.db.transaction() as connection:
                        connection.execute(
                            "UPDATE generation_assets SET provider_asset_id=? WHERE id=?",
                            (provider_id, row["id"]),
                        )
                provider_ids.append(provider_id)
            payload["reference_images"] = [f"assetId://{provider_id}" for provider_id in provider_ids]
        return payload

    def _submit_image_task(self, task: dict[str, Any]) -> None:
        self._set_task_status(task["id"], "submitting", keep_lease=True)
        raw_payload = json.loads(task["request_json"])
        reference_ids = raw_payload.pop("reference_asset_ids", [])
        raw_payload.pop("operation", None)
        if task["operation"] == "edit":
            references = self._generation_asset_files(task["user_id"], reference_ids)
            output_bytes = self.image2.edit(raw_payload, references)
        else:
            output_bytes = self.image2.create(raw_payload)
        self._store_binary_outputs(task["id"], output_bytes, ["image/png"] * len(output_bytes))

    def _generation_asset_files(self, user_id: str, asset_ids: list[str]) -> list[tuple[Path, str]]:
        if not asset_ids:
            return []
        with self.db.connect() as connection:
            rows = connection.execute(
                """SELECT generation_assets.id,uploads.object_key,uploads.content_type,uploads.expected_size
                   FROM generation_assets JOIN uploads ON uploads.id=generation_assets.upload_id
                   WHERE generation_assets.user_id=? AND generation_assets.id IN (%s)"""
                % ",".join("?" for _ in asset_ids),
                (user_id, *asset_ids),
            ).fetchall()
        if len(rows) != len(set(asset_ids)):
            raise ApiError(422, "GENERATION_ASSET_UNAVAILABLE", "参考素材不存在或已过期")
        rows_by_id = {row["id"]: row for row in rows}
        rows = [rows_by_id[asset_id] for asset_id in dict.fromkeys(asset_ids)]
        if any(
            not row["content_type"].startswith("image/")
            or row["expected_size"] > self.config.max_reference_image_bytes
            for row in rows
        ):
            raise ApiError(413, "REFERENCE_IMAGE_TOO_LARGE", "参考图片单文件不能超过 20MB")
        return [(self.config.storage_path / row["object_key"], row["content_type"]) for row in rows]

    def _submit_video_task(self, task: dict[str, Any]) -> None:
        self._set_task_status(task["id"], "submitting", keep_lease=True)
        payload = self._task_payload(task)
        upstream = self.xiangxin.create_video(payload)
        data = upstream.get("data") if isinstance(upstream.get("data"), dict) else {}
        upstream_task_id = str(
            upstream.get("id") or upstream.get("task_id") or data.get("id") or data.get("task_id") or ""
        )
        if not upstream_task_id:
            raise XiangxinError("XIANGXIN_INVALID_RESPONSE", "视频响应缺少任务编号", True)
        with self.db.transaction() as connection:
            connection.execute(
                """UPDATE generation_tasks SET status='provider_accepted',upstream_task_id=?,
                   upstream_response_json=?,next_attempt_at=?,lease_owner=NULL,lease_expires_at=NULL,updated_at=? WHERE id=?""",
                (upstream_task_id, json.dumps(upstream), self.now() + 10, self.now(), task["id"]),
            )

    def _refresh_video_by_task(self, task: dict[str, Any]) -> None:
        upstream = self.xiangxin.get_video(task["upstream_task_id"])
        data = upstream.get("data") if isinstance(upstream.get("data"), dict) else {}
        status = str(upstream.get("status") or data.get("status") or "").lower()
        if status in {"queued", "processing", "running", "pending"}:
            self._set_task_status(task["id"], "processing", upstream, delay=10)
            return
        if status in {"failed", "error", "cancelled", "canceled"}:
            self._fail_and_release(
                task["id"],
                "XIANGXIN_GENERATION_FAILED",
                str(upstream.get("error") or data.get("error") or "视频生成失败"),
                upstream,
            )
            return
        url = upstream.get("result_url") or upstream.get("video_url") or data.get("result_url") or data.get("video_url")
        if isinstance(url, str) and url.startswith("/"):
            url = self.config.xiangxin_base_url.rstrip("/") + url
        if status in {"completed", "succeeded", "success"} and isinstance(url, str):
            self._set_task_status(task["id"], "validating", upstream)
            content, content_type = self._download_provider_result(url, "video")
            self._store_binary_outputs(task["id"], [content], [content_type])
            return
        self._mark_provider_retry(task["id"], type("ProviderError", (), {"code": "UNKNOWN_PROVIDER_STATUS", "message": "上游状态待核实"})())

    def _download_provider_result(self, url: str, kind: str) -> tuple[bytes, str]:
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme != "https" or not parsed.hostname or not any(
            parsed.hostname == host or parsed.hostname.endswith("." + host) for host in self.config.provider_result_hosts
        ):
            raise ApiError(502, "PROVIDER_RESULT_HOST_DENIED", "上游结果地址不在允许范围")
        headers = {"Accept": "image/*,video/*"}
        provider_host = urllib.parse.urlsplit(self.config.xiangxin_base_url).hostname
        if provider_host and parsed.hostname == provider_host and self.config.xiangxin_api_key:
            headers["Authorization"] = f"Bearer {self.config.xiangxin_api_key}"
        request = urllib.request.Request(url, headers=headers)
        try:
            with open_no_redirect(request, timeout=self.config.provider_timeout_seconds) as response:
                content_type = response.headers.get_content_type()
                if kind == "image" and not content_type.startswith("image/"):
                    raise ApiError(502, "INVALID_PROVIDER_RESULT", "上游未返回图片")
                if kind == "video" and not content_type.startswith("video/"):
                    raise ApiError(502, "INVALID_PROVIDER_RESULT", "上游未返回视频")
                content = read_limited(response, self.config.max_upload_bytes)
        except ApiError:
            raise
        except (urllib.error.URLError, TimeoutError) as exc:
            raise XiangxinError("PROVIDER_RESULT_UNAVAILABLE", "上游结果暂时不可下载", True) from exc
        if not content:
            raise ApiError(502, "INVALID_PROVIDER_RESULT", "上游结果大小无效")
        return content, content_type

    def _store_binary_outputs(self, task_id: str, contents: list[bytes], content_types: list[str]) -> None:
        now = self.now()
        expires_at = now + self.config.generation_output_ttl_seconds
        for content, content_type in zip(contents, content_types):
            self._probe_media(content, content_type)
        with self.db.transaction(immediate=True) as connection:
            task = connection.execute("SELECT * FROM generation_tasks WHERE id=?", (task_id,)).fetchone()
            hold = connection.execute("SELECT * FROM wallet_holds WHERE task_id=?", (task_id,)).fetchone()
            if not task or task["status"] == "succeeded":
                return
            for content, content_type in zip(contents, content_types):
                output_id = new_id("out")
                object_key = f"outputs/{task_id}/{output_id}"
                target = self.config.storage_path / object_key
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(content)
                connection.execute(
                    """INSERT INTO generation_outputs
                       (id,task_id,object_key,content_type,size_bytes,sha256,expires_at,created_at)
                       VALUES(?,?,?,?,?,?,?,?)""",
                    (
                        output_id,
                        task_id,
                        object_key,
                        content_type,
                        len(content),
                        hashlib.sha256(content).hexdigest(),
                        expires_at,
                        now,
                    ),
                )
            connection.execute(
                """UPDATE generation_tasks SET status='succeeded',error_code=NULL,error_message=NULL,
                   lease_owner=NULL,lease_expires_at=NULL,updated_at=? WHERE id=?""",
                (now, task_id),
            )
            if hold and hold["state"] == "held":
                connection.execute("UPDATE wallet_holds SET state='captured',updated_at=? WHERE id=?", (now, hold["id"]))
                connection.execute("UPDATE wallets SET held_credits=held_credits-?,updated_at=? WHERE user_id=?", (hold["credits"], now, hold["user_id"]))
                connection.execute(
                    """INSERT INTO ledger_entries
                       (id,user_id,kind,delta_available,delta_held,reference_type,reference_id,idempotency_key,created_at)
                       VALUES(?,?,'capture',0,?,'generation_task',?,?,?)""",
                    (new_id("led"), hold["user_id"], -hold["credits"], task_id, f"capture:{task_id}", now),
                )

    def _probe_media(self, content: bytes, content_type: str) -> None:
        suffix = ".png" if content_type.startswith("image/") else ".mp4"
        try:
            with tempfile.NamedTemporaryFile(suffix=suffix) as media_file:
                media_file.write(content)
                media_file.flush()
                result = subprocess.run(
                    [
                        "ffprobe",
                        "-v",
                        "error",
                        "-show_streams",
                        "-show_format",
                        "-of",
                        "json",
                        media_file.name,
                    ],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    timeout=20,
                    check=True,
                )
            probe = json.loads(result.stdout.decode("utf-8"))
        except (OSError, subprocess.SubprocessError, ValueError) as exc:
            raise ApiError(502, "INVALID_PROVIDER_MEDIA", "生成结果无法解析") from exc
        streams = probe.get("streams", []) if isinstance(probe, dict) else []
        video_streams = [
            stream
            for stream in streams
            if isinstance(stream, dict)
            and stream.get("codec_type") == "video"
            and int(stream.get("width") or 0) > 0
            and int(stream.get("height") or 0) > 0
        ]
        if not video_streams:
            raise ApiError(502, "INVALID_PROVIDER_MEDIA", "生成结果缺少可读取画面")
        if content_type.startswith("image/"):
            if video_streams[0].get("codec_name") not in {"png", "mjpeg", "webp"}:
                raise ApiError(502, "INVALID_PROVIDER_MEDIA", "生成结果不是支持的图片")
            return
        try:
            duration = float(probe.get("format", {}).get("duration") or 0)
        except (TypeError, ValueError):
            duration = 0
        if duration <= 0:
            raise ApiError(502, "INVALID_PROVIDER_MEDIA", "生成视频没有有效时长")

    def cleanup_expired(self) -> dict[str, int]:
        now = self.now()
        removed_outputs = 0
        removed_uploads = 0
        with self.db.connect() as connection:
            outputs = connection.execute(
                "SELECT id,task_id,object_key FROM generation_outputs WHERE expires_at<=?",
                (now,),
            ).fetchall()
            active_rows = connection.execute(
                """SELECT request_json FROM generation_tasks
                   WHERE status IN ('queued','submitting','provider_accepted','processing','validating','pending_reconcile')"""
            ).fetchall()
            staging = connection.execute(
                """SELECT uploads.id,uploads.object_key,generation_assets.id generation_asset_id
                   FROM uploads LEFT JOIN generation_assets ON generation_assets.upload_id=uploads.id
                   WHERE uploads.purpose='generation_input' AND uploads.expires_at<=?""",
                (now,),
            ).fetchall()
        active_asset_ids: set[str] = set()
        for row in active_rows:
            try:
                request = json.loads(row["request_json"])
            except (TypeError, ValueError):
                continue
            active_asset_ids.update(request.get("reference_asset_ids", []))

        for output in outputs:
            (self.config.storage_path / output["object_key"]).unlink(missing_ok=True)
            with self.db.transaction(immediate=True) as connection:
                deleted = connection.execute(
                    "DELETE FROM generation_outputs WHERE id=? AND expires_at<=?",
                    (output["id"], now),
                ).rowcount
                if deleted:
                    remaining = connection.execute(
                        "SELECT 1 FROM generation_outputs WHERE task_id=? LIMIT 1",
                        (output["task_id"],),
                    ).fetchone()
                    if not remaining:
                        connection.execute(
                            """UPDATE generation_tasks SET status='expired',
                               error_code='OUTPUT_EXPIRED',error_message='生成结果已过期',updated_at=?
                               WHERE id=? AND status='succeeded'""",
                            (now, output["task_id"]),
                        )
                    removed_outputs += 1

        for upload in staging:
            if upload["generation_asset_id"] in active_asset_ids:
                continue
            (self.config.storage_path / upload["object_key"]).unlink(missing_ok=True)
            with self.db.transaction(immediate=True) as connection:
                connection.execute(
                    "DELETE FROM generation_assets WHERE upload_id=?", (upload["id"],)
                )
                removed_uploads += connection.execute(
                    """DELETE FROM uploads WHERE id=? AND purpose='generation_input'
                       AND expires_at<=?""",
                    (upload["id"], now),
                ).rowcount
        return {"outputs": removed_outputs, "uploads": removed_uploads}

    def _set_task_status(
        self,
        task_id: str,
        status: str,
        upstream: dict[str, Any] | None = None,
        delay: int = 0,
        keep_lease: bool = False,
    ) -> None:
        with self.db.transaction() as connection:
            if keep_lease:
                connection.execute(
                    """UPDATE generation_tasks SET status=?,
                       upstream_response_json=COALESCE(?,upstream_response_json),
                       next_attempt_at=?,updated_at=? WHERE id=?""",
                    (
                        status,
                        json.dumps(upstream) if upstream else None,
                        self.now() + delay,
                        self.now(),
                        task_id,
                    ),
                )
            else:
                connection.execute(
                    """UPDATE generation_tasks SET status=?,
                       upstream_response_json=COALESCE(?,upstream_response_json),
                       next_attempt_at=?,lease_owner=NULL,lease_expires_at=NULL,updated_at=? WHERE id=?""",
                    (
                        status,
                        json.dumps(upstream) if upstream else None,
                        self.now() + delay,
                        self.now(),
                        task_id,
                    ),
                )

    def _mark_provider_retry(self, task_id: str, error: Any, retry_poll: bool = False) -> None:
        with self.db.transaction() as connection:
            connection.execute(
                """UPDATE generation_tasks SET status='pending_reconcile',error_code=?,error_message=?,
                   upstream_task_id=COALESCE(?,upstream_task_id),next_attempt_at=?,lease_owner=NULL,
                   lease_expires_at=NULL,updated_at=? WHERE id=?""",
                (
                    error.code,
                    error.message,
                    getattr(error, "upstream_task_id", None),
                    self.now() + (30 if retry_poll else 300),
                    self.now(),
                    task_id,
                ),
            )

    def get_generation_task(self, user_id: str, task_id: str, refresh: bool = False) -> dict[str, Any]:
        with self.db.connect() as connection:
            row = connection.execute(
                "SELECT * FROM generation_tasks WHERE id=? AND user_id=?", (task_id, user_id)
            ).fetchone()
        if not row:
            raise ApiError(404, "GENERATION_TASK_NOT_FOUND", "生成任务不存在")
        with self.db.connect() as connection:
            return self._task_dict(connection, row)

    def list_generation_tasks(self, user_id: str) -> list[dict[str, Any]]:
        with self.db.connect() as connection:
            rows = connection.execute(
                "SELECT * FROM generation_tasks WHERE user_id=? ORDER BY created_at DESC LIMIT 100",
                (user_id,),
            ).fetchall()
            return [self._task_dict(connection, row) for row in rows]

    def generation_output(self, user_id: str, task_id: str, output_id: str) -> tuple[Path, str, str]:
        with self.db.connect() as connection:
            row = connection.execute(
                """SELECT generation_outputs.object_key,generation_outputs.content_type,
                          generation_tasks.user_id,generation_tasks.status
                   FROM generation_outputs JOIN generation_tasks ON generation_tasks.id=generation_outputs.task_id
                   WHERE generation_outputs.id=? AND generation_tasks.id=?""",
                (output_id, task_id),
            ).fetchone()
        if not row or row["user_id"] != user_id or row["status"] != "succeeded":
            raise ApiError(404, "GENERATION_OUTPUT_NOT_FOUND", "生成结果不存在")
        path = self.config.storage_path / row["object_key"]
        if not path.is_file():
            raise ApiError(404, "GENERATION_OUTPUT_MISSING", "生成结果文件不可用")
        return path, row["content_type"], path.name

    def _fail_and_release(
        self,
        task_id: str,
        code: str,
        message: str,
        upstream: dict[str, Any] | None = None,
    ) -> None:
        now = self.now()
        with self.db.transaction(immediate=True) as connection:
            task = connection.execute("SELECT * FROM generation_tasks WHERE id=?", (task_id,)).fetchone()
            hold = connection.execute("SELECT * FROM wallet_holds WHERE task_id=?", (task_id,)).fetchone()
            if not task or task["status"] in {"succeeded", "failed"}:
                return
            connection.execute(
                """UPDATE generation_tasks SET status='failed',error_code=?,error_message=?,
                   upstream_response_json=COALESCE(?,upstream_response_json),updated_at=? WHERE id=?""",
                (code, message, json.dumps(upstream) if upstream else None, now, task_id),
            )
            if hold and hold["state"] == "held":
                connection.execute(
                    "UPDATE wallet_holds SET state='released',updated_at=? WHERE id=?", (now, hold["id"])
                )
                connection.execute(
                    """UPDATE wallets SET available_credits=available_credits+?,
                       held_credits=held_credits-?,updated_at=? WHERE user_id=?""",
                    (hold["credits"], hold["credits"], now, hold["user_id"]),
                )
                connection.execute(
                    """INSERT INTO ledger_entries
                       (id,user_id,kind,delta_available,delta_held,reference_type,reference_id,idempotency_key,created_at)
                       VALUES(?,?,'release',?,?,'generation_task',?,?,?)""",
                    (
                        new_id("led"),
                        hold["user_id"],
                        hold["credits"],
                        -hold["credits"],
                        task_id,
                        f"release:{task_id}",
                        now,
                    ),
                )

    def _task_dict(self, connection: sqlite3.Connection, row: sqlite3.Row) -> dict[str, Any]:
        outputs = connection.execute(
            """SELECT id,content_type,size_bytes,sha256,expires_at,created_at
               FROM generation_outputs WHERE task_id=?""",
            (row["id"],),
        ).fetchall()
        output_dicts = []
        for output in outputs:
            item = dict(output)
            item["download_url"] = f"/api/generation/tasks/{row['id']}/outputs/{output['id']}/content"
            output_dicts.append(item)
        return {
            "id": row["id"],
            "kind": row["kind"],
            "model": row["model"],
            "operation": row["operation"],
            "upstream_task_id": row["upstream_task_id"],
            "prompt": row["prompt"],
            "quoted_credits": row["quoted_credits"],
            "status": row["status"],
            "error": (
                {"code": row["error_code"], "message": row["error_message"]}
                if row["error_code"]
                else None
            ),
            "outputs": output_dicts,
            "created_at": row["created_at"],
            "updated_at": row["updated_at"],
        }
