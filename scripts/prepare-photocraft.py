#!/usr/bin/env python3
"""按固定提交和有序补丁准备完整 PhotoCraft；已有目录只读校验。"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from photocraft_source import ROOT, INTEGRATION, load_pin, run, verify


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path, default=ROOT / "external/photocraft")
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    destination = Path(os.path.abspath(args.destination.expanduser()))
    pin = load_pin()
    if destination.exists() or destination.is_symlink():
        print(json.dumps(verify(destination, pin), ensure_ascii=False, indent=2))
        return
    if args.verify_only:
        raise ValueError(f"缺少 PhotoCraft 源码：{destination}")
    # 临时检出完整仓库，成功后独占发布到尚不存在的目标；不下载或安装 Rust。
    work = ROOT / ".toonflow"
    work.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="prepare-photocraft-", dir=work) as temporary:
        checkout = Path(temporary) / "checkout"
        run(["git", "clone", "--filter=blob:none", "--no-checkout", "--", pin["repository"], str(checkout)])
        run(["git", "checkout", "--detach", pin["commit"]], checkout)
        for patch in pin["patches"]:
            path = INTEGRATION / patch["path"]
            run(["git", "apply", "--check", str(path)], checkout)
            run(["git", "apply", str(path)], checkout)
        identity = verify(checkout, pin)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.mkdir()  # 已有目标或竞争创建直接失败，不覆盖。
        shutil.copytree(checkout, destination, symlinks=True, dirs_exist_ok=True)
        verify(destination, pin)
    print(json.dumps(identity, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError, StopIteration) as error:
        raise SystemExit(f"PhotoCraft 准备失败：{error}") from error
