"""Exercise a running loopback development server; no provider calls by default."""
import argparse
import json
import secrets
import sqlite3
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="http://127.0.0.1:8787")
    parser.add_argument("--database")
    args = parser.parse_args()
    assert args.base.startswith("http://127.0.0.1:")
    token = ""

    def call(method, path, data=None):
        raw = None if data is None else json.dumps(data).encode()
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        request = urllib.request.Request(args.base + path, data=raw, method=method, headers=headers)
        with urllib.request.urlopen(request, timeout=10) as response:
            body = response.read()
            return json.loads(body) if body else None

    suffix = secrets.token_hex(4)
    email = f"seecut-test-{suffix}@example.test"
    password = secrets.token_urlsafe(24)
    account = call("POST", "/api/auth/register", {"email": email, "password": password})
    assert "verification_token" in account, "Use an isolated test server with test token exposure"
    call("POST", "/api/auth/verify-email", {"token": account["verification_token"]})
    auth = call("POST", "/api/auth/login", {"email": email, "password": password})
    token = auth["access_token"]
    assert call("GET", "/api/auth/me")["email"] == email
    team = call("POST", "/api/teams", {"name": "本地联调团队"})
    invite = call("POST", f"/api/teams/{team['id']}/invites", {})
    assert invite["invite_url"].startswith("seecut://team-invite/")
    assert call("GET", "/api/wallet")["available_credits"] == 0
    assert len(call("GET", "/api/generation/capabilities")["models"]) == 2
    if args.database:
        with sqlite3.connect(args.database) as connection:
            connection.execute("UPDATE wallets SET available_credits=100 WHERE user_id=?", (auth["user"]["id"],))
    call("POST", "/api/auth/logout", {})
    try:
        call("GET", "/api/auth/me")
        raise AssertionError("Session was not revoked")
    except urllib.error.HTTPError as error:
        assert error.code == 401
    print("HTTP smoke passed: register, verify, login, team, invite, wallet, catalog, logout")


if __name__ == "__main__":
    main()
