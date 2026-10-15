#!/usr/bin/env python3
"""生成独立实验应用；不安装或替换 /Applications 下的应用。"""
from pathlib import Path
import argparse
import json
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--output", type=Path, help="新的 .app 产物路径；不覆盖已有产物")
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
work = root / ".toonflow"
app = Path(os.path.abspath((args.output or work / "SeeCut Toonflow.app").expanduser()))
if app.exists() or app.is_symlink():
    raise SystemExit(f"实验应用已存在，请先关闭并选择新的产物路径：{app}")
if app.suffix != ".app":
    raise SystemExit("--output 需要指定新的 .app 路径")
source = root / "external/toonflow"
subprocess.run([sys.executable, str(root / "scripts/prepare-toonflow.py"), "--verify-only"], check=True)
bun = Path(os.environ.get("SEECUT_BUN", work / "bin/bun")).expanduser().resolve()
if not (source / "build/server/seecutHost.js").is_file() or not bun.is_file():
    raise SystemExit("先运行 scripts/build-toonflow.py")
pin = json.loads((root / "integrations/toonflow/upstream.json").read_text())
if subprocess.check_output([str(bun), "--version"], text=True).strip() != pin["bun"]:
    raise SystemExit(f"本实验固定 Bun {pin['bun']}")
provenance = root / "integrations/toonflow/SOURCE.md"
if not provenance.is_file():
    raise SystemExit(f"缺少来源说明：{provenance}")
if not args.binary.is_file():
    raise SystemExit(f"缺少原生可执行文件：{args.binary}")
environment = os.environ.copy()
work.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="package-", dir=work) as temporary:
    environment.update(SEECUT_BIN=str(args.binary.resolve()), SEECUT_INSTALL="0", SEECUT_OUT=str(Path(temporary) / "native-package"))
    subprocess.run([str(root / "scripts/make-app.sh")], cwd=root, env=environment, check=True)
    app.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(Path(temporary) / "native-package/Seecut.app", app, symlinks=True)
resources = app / "Contents/Resources/toonflow"
shutil.copytree(source / "build", resources / "build", symlinks=False)
(resources / "bin").mkdir()
shutil.copy2(bun, resources / "bin/bun")
shutil.copy2(source / "LICENSE", resources / "LICENSE")
shutil.copy2(provenance, resources / "SOURCE.md")
(app / "Contents/MacOS/portable").mkdir()
plist_path = app / "Contents/Info.plist"
with plist_path.open("rb") as handle:
    info = plistlib.load(handle)
info.update(CFBundleName="SeeCut Flow", CFBundleDisplayName="SeeCut Flow", CFBundleIdentifier="cloud.stormycry.seecut.flow-experiment")
with plist_path.open("wb") as handle:
    plistlib.dump(info, handle)
subprocess.run(["codesign", "--force", "--sign", "-", str(resources / "bin/bun")], check=True)
subprocess.run(["codesign", "--force", "--sign", "-", str(app)], check=True)
subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
print(json.dumps({"app": str(app), "installed": False, "portable": True}, ensure_ascii=False))
