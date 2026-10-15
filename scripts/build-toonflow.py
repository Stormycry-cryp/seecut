#!/usr/bin/env python3
"""构建固定版本的完整 Toonflow Web/Server 和 SeeCut 本地启动入口。"""
from pathlib import Path
import argparse
import json
import os
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.parse_args()
root = Path(__file__).resolve().parents[1]
source = root / "external/toonflow"
subprocess.run([sys.executable, str(root / "scripts/prepare-toonflow.py"), "--verify-only"], check=True)
pin = json.loads((root / "integrations/toonflow/upstream.json").read_text())
bun = Path(os.environ.get("SEECUT_BUN", root / ".toonflow/bin/bun")).expanduser().resolve()
if not bun.is_file():
    raise SystemExit(f"缺少 Bun {pin['bun']}，请通过 SEECUT_BUN 指定本地可执行文件")
version = subprocess.check_output([str(bun), "--version"], text=True).strip()
if version != pin["bun"]:
    raise SystemExit(f"本实验固定 Bun {pin['bun']}，当前 {version}")
if not (source / "node_modules").is_dir():
    raise SystemExit(f"缺少依赖。请显式进入 {source}，用 Bun {pin['bun']} 执行 bun install --frozen-lockfile，再运行构建。")
environment = os.environ.copy()
environment["PATH"] = str(bun.parent) + os.pathsep + environment.get("PATH", "")
environment["NODE_ENV"] = "production"
environment["appVersion"] = f"{pin['version']}-seecut"
subprocess.run([str(bun), "run", "--bun", "build:server"], cwd=source, env=environment, check=True)
subprocess.run([
    str(bun), "build", "src/seecutHost.ts", "--target=bun",
    "--external", "@silvia-odwyer/photon-node", "--external", "@firecrawl/anydoc",
    "--external", "@firecrawl/anydoc-wasm", "--outfile", "../../build/server/seecutHost.js",
], cwd=source / "apps/server", env=environment, check=True)
print("完整 Toonflow 与 SeeCut 启动入口构建完成")
