"""Opt-in, single-submission video smoke against an isolated loopback test service."""
import argparse
import json
from pathlib import Path
import secrets
import sqlite3
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    args = parser.parse_args()
    assert args.base.startswith("http://127.0.0.1:")
    assert not args.receipt.exists(), "A receipt already exists; inspect it instead of resubmitting"
    token = ""

    def call(method, path, data=None, idem=None):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        if idem:
            headers["Idempotency-Key"] = idem
        request = urllib.request.Request(args.base + path, method=method, headers=headers,
                                         data=None if data is None else json.dumps(data).encode())
        with urllib.request.urlopen(request, timeout=20) as response:
            return json.load(response)

    email = f"provider-smoke-{secrets.token_hex(6)}@example.test"
    password = secrets.token_urlsafe(24)
    registration = call("POST", "/api/auth/register", {"email": email, "password": password})
    call("POST", "/api/auth/verify-email", {"email": email, "token": registration["verification_token"]})
    account = call("POST", "/api/auth/login", {"email": email, "password": password})
    token = account["access_token"]
    with sqlite3.connect(args.database) as connection:
        connection.execute("UPDATE wallets SET available_credits=100 WHERE user_id=?", (account["user"]["id"],))
    body = {"kind": "video", "model": "sd_2.0_mini_special", "operation": "generate",
            "prompt": "A white ceramic cup on a gray studio table. A slow gentle camera push in. No people, no text.",
            "duration": 5, "resolution": "720p", "aspect_ratio": "16:9"}
    quote = call("POST", "/api/generation/quote", body)
    body.pop("kind")
    body["quote_id"] = quote["quote_id"]
    task = call("POST", "/api/generation/videos", body, secrets.token_hex(16))
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    args.receipt.write_text(json.dumps({"task_id": task["id"], "user_id": account["user"]["id"]}))
    print("Submitted exactly one video task:", task["id"], flush=True)
    previous = None
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        task = call("GET", f"/api/generation/tasks/{task['id']}")
        if task["status"] != previous:
            print("Status:", task["status"], task.get("error"), flush=True)
            previous = task["status"]
        if task["status"] in {"succeeded", "failed", "pending_reconcile"}:
            args.receipt.write_text(json.dumps({"task": task, "wallet": call("GET", "/api/wallet")}, ensure_ascii=False, indent=2))
            if task["status"] != "succeeded":
                raise SystemExit(2)
            output = task["outputs"][0]
            request = urllib.request.Request(args.base + output["download_url"], headers={"Authorization": f"Bearer {token}"})
            target = args.receipt.with_suffix(".mp4")
            with urllib.request.urlopen(request, timeout=60) as response, target.open("wb") as stream:
                import shutil
                shutil.copyfileobj(response, stream)
            print("Downloaded:", target, flush=True)
            return
        time.sleep(5)
    print("Timeout; retain the receipt and query this task without resubmitting", flush=True)
    raise SystemExit(2)


if __name__ == "__main__":
    main()
