#!/usr/bin/env python3
"""Run the real assistant Slint component with a local synthetic host.

Requires the pinned Slint Python wheel in requirements-assistant-component.txt.
No server, model, MCP client, user preferences or user files are accessed.
This fixture does not verify the Rust host or the complete editor window.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import subprocess
from datetime import datetime, timedelta, timezone
import slint


def message(role: str, text: str, progress: str = "", state: str = "") -> dict:
    return {"role": role, "text": text, "progress": progress, "state": state}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--compile-only", action="store_true")
    args = parser.parse_args()
    if importlib.metadata.version("slint") != "1.17.1b2":
        parser.error("Use the pinned Slint 1.17.1b2 wheel (product core 1.17.1).")
    root = Path(__file__).resolve().parents[2]
    args.output.mkdir(parents=True, exist_ok=True)
    components = slint.load_file(Path(__file__).with_suffix(".slint"), style="fluent")
    ui_sources = root / "src/crates/concat/ui"
    manifest = {
        "kind": "native-component-fixture",
        "host": "synthetic-python-no-network",
        "slint": importlib.metadata.version("slint"),
        "head": subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip(),
        "sources": {
            str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(ui_sources.rglob("*.slint"))
        },
        "fixture_sources": {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest() for path in [Path(__file__), Path(__file__).with_suffix(".slint"), Path(__file__).with_name("requirements-assistant-component.txt")]},
        "limitations": ["not-full-app", "not-rust-host", "not-mcp", "not-image-reference-match"],
    }
    (args.output / "source-manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    if args.compile_only:
        print("Production assistant component compiled with Slint 1.17.1.")
        return

    app = components.AssistantComponentQA()
    rows = slint.ListModel([])
    app.messages = rows
    counter = {"sent": 0, "stopped": 0, "closed": 0, "connections": 0, "permissions": 0, "ticks": 0}
    stream_timer = slint.Timer()

    def record(event: str, **fields: object) -> None:
        entry = {"at": datetime.now(timezone.utc).isoformat(), "event": event, **fields}
        with (args.output / "events.jsonl").open("a") as log:
            log.write(json.dumps(entry, ensure_ascii=False) + "\n")
        app.observation = "发送 {sent} 次 / 停止 {stopped} 次 / 关闭 {closed} 次 / 流式更新 {ticks} 次".format(**counter)

    def reset_rows(values: list[dict]) -> None:
        while rows.row_count():
            del rows[rows.row_count() - 1]
        for value in values:
            rows.append(value)

    def tick() -> None:
        counter["ticks"] += 1
        index = rows.row_count() - 1
        value = dict(rows.row_data(index))
        value["text"] += f"\n流式合成段落 {counter['ticks']}：检查继续阅读时的位置与最新内容。"
        rows.set_row_data(index, value)
        if counter["ticks"] % 5 == 0:
            record("stream.progress", tick=counter["ticks"])
        if counter["ticks"] >= 80:
            stream_timer.stop()
            app.busy = False
            value["state"] = "已完成"
            value["progress"] = ""
            rows.set_row_data(index, value)
            record("stream.completed")

    def fixture(action: str) -> None:
        stream_timer.stop()
        app.busy = False
        app.unknown = False
        app.can_send = True
        app.connected = True
        app.status = "可以发送问题"
        if action == "short":
            reset_rows([message("你", "请检查这份合成素材。"), message("助手", "这是本地合成回复。可以检查字号、换行和输入操作。", "已读取合成工程", "已完成")])
        elif action == "long":
            reset_rows([message("你" if index % 2 == 0 else "助手", f"第 {index + 1} 条合成消息。" + "检查长列表中的换行和阅读位置。" * 8) for index in range(24)])
            app.scroll_token += 1
        elif action == "stream":
            if not rows.row_count():
                reset_rows([message("助手", "开始合成流式输出。")])
            rows.append(message("助手", "正在追加本地合成内容。", "正在处理合成输入", ""))
            counter["ticks"] = 0
            app.busy = True
            stream_timer.start(slint.TimerMode.Repeated, timedelta(milliseconds=500), tick)
        elif action == "unknown":
            app.unknown = True
            app.can_send = False
            app.status = "操作结果待核实"
        elif action == "failed":
            app.status = "处理失败"
            rows.append(message("助手", "本地合成失败：请保留输入并检查后重试。", "", "处理失败"))
        elif action == "disconnected":
            app.can_send = False
            app.connected = False
            app.status = "尚未连接"
        record("fixture", action=action)

    def sent(text: str) -> None:
        counter["sent"] += 1
        record("send", characters=len(text), text_sha256=hashlib.sha256(text.encode()).hexdigest())
        rows.append(message("你", text))
        app.draft = ""
        app.scroll_token += 1
        fixture("stream")

    def stopped() -> None:
        counter["stopped"] += 1
        stream_timer.stop()
        app.busy = False
        app.status = "已停止"
        record("stop")

    def closed() -> None:
        counter["closed"] += 1
        stream_timer.stop()
        app.busy = False
        record("close")

    def connection_requested() -> None:
        counter["connections"] += 1
        record("connection.requested", network="never-called")

    def permission_requested(action: str) -> None:
        counter["permissions"] += 1
        record("permission.requested", action=action, granted=False)

    def observed_key(text: str, control: bool, meta: bool) -> None:
        # Record only modifier/Return events, never typed content or credentials.
        if text in ("\n", "\r", "\x11", "\x16", "\x17", "\x18"):
            record("key", codes=[ord(char) for char in text], control=control, meta=meta)

    app.fixture = fixture
    app.sent = sent
    app.stopped = stopped
    app.closed = closed
    app.connection_requested = connection_requested
    app.permission_requested = permission_requested
    app.observed_key = observed_key
    fixture("short")
    app.run()
    stream_timer.stop()
    record("window.exited")


if __name__ == "__main__":
    main()
