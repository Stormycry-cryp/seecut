from __future__ import annotations

import base64
import hashlib
import hmac
import secrets
import time


PASSWORD_MAX_LENGTH = 128


def new_id(prefix: str) -> str:
    return f"{prefix}_{secrets.token_urlsafe(16)}"


def new_token() -> str:
    return secrets.token_urlsafe(32)


def token_digest(token: str) -> str:
    return hashlib.sha256(token.encode("utf-8")).hexdigest()


def hash_password(password: str) -> str:
    if len(password) < 10:
        raise ValueError("password must contain at least 10 characters")
    if len(password) > PASSWORD_MAX_LENGTH:
        raise ValueError("password exceeds maximum length")
    salt = secrets.token_bytes(16)
    digest = hashlib.scrypt(password.encode("utf-8"), salt=salt, n=2**14, r=8, p=1)
    return "scrypt$16384$8$1$%s$%s" % (
        base64.urlsafe_b64encode(salt).decode("ascii"),
        base64.urlsafe_b64encode(digest).decode("ascii"),
    )


def verify_password(password: str, encoded: str) -> bool:
    if len(password) > PASSWORD_MAX_LENGTH:
        return False
    try:
        algorithm, n, r, p, salt_value, digest_value = encoded.split("$", 5)
        if algorithm != "scrypt":
            return False
        salt = base64.urlsafe_b64decode(salt_value.encode("ascii"))
        expected = base64.urlsafe_b64decode(digest_value.encode("ascii"))
        actual = hashlib.scrypt(
            password.encode("utf-8"), salt=salt, n=int(n), r=int(r), p=int(p)
        )
        return hmac.compare_digest(actual, expected)
    except (ValueError, TypeError):
        return False


def sign_path(secret: str, path: str, expires_at: int) -> str:
    payload = f"{path}\n{expires_at}".encode("utf-8")
    return hmac.new(secret.encode("utf-8"), payload, hashlib.sha256).hexdigest()


def verify_path_signature(secret: str, path: str, expires_at: int, signature: str) -> bool:
    if expires_at < int(time.time()):
        return False
    return hmac.compare_digest(sign_path(secret, path, expires_at), signature)
