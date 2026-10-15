#!/usr/bin/env python3
"""按固定提交及受管补丁准备 Toonflow；已有目录只校验，不覆盖。"""
from pathlib import Path, PurePosixPath
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
INTEGRATION = ROOT / "integrations/toonflow"
MARKER = ".seecut-upstream.json"


def sha256(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def patch_paths(patch):
    output = subprocess.check_output(["git", "apply", "--numstat", "-z", str(patch)])
    records = iter(output.split(b"\0"))
    paths = set()
    for record in records:
        if not record:
            continue
        fields = record.split(b"\t", 2)
        if len(fields) != 3:
            raise ValueError("无法读取补丁文件清单")
        names = [fields[2]] if fields[2] else [next(records), next(records)]
        for name in names:
            text = name.decode("utf-8")
            path = PurePosixPath(text)
            if path.is_absolute() or ".." in path.parts or ".git" in path.parts or not path.parts or text == MARKER:
                raise ValueError(f"补丁路径无效：{text}")
            paths.add(text)
    if not paths:
        raise ValueError("受管补丁没有文件变更")
    return paths


def file_hash(directory, name):
    path = directory / name
    for part in [path, *path.parents]:
        if part == directory:
            break
        if part.is_symlink():
            raise ValueError(f"受管补丁文件不能经由软链接：{name}")
    if not path.exists():
        return None
    if not path.is_file():
        raise ValueError(f"受管补丁路径不是普通文件：{name}")
    return sha256(path)


def verify(directory, identity, paths):
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError(f"源码目录无效或为软链接：{directory}")
    marker_path = directory / MARKER
    if marker_path.is_symlink() or not marker_path.is_file():
        raise ValueError(f"已有源码缺少受管标记，保留原目录并停止：{marker_path}")
    marker = json.loads(marker_path.read_text())
    if not isinstance(marker, dict) or any(marker.get(key) != value for key, value in identity.items()):
        raise ValueError(f"已有源码的提交或补丁身份不匹配，保留原目录并停止：{directory}")
    files = marker.get("files")
    if not isinstance(files, dict) or set(files) != paths:
        raise ValueError("源码标记中的补丁文件清单不匹配")
    for name in sorted(paths):
        expected = files[name]
        if expected is not None and (not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected)):
            raise ValueError(f"源码标记中的文件哈希无效：{name}")
        if file_hash(directory, name) != expected:
            raise ValueError(f"源码存在本地修改，保留原文件并停止：{name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path, default=ROOT / "external/toonflow", help="独立源码目标目录；已存在时只校验")
    parser.add_argument("--verify-only", action="store_true", help="仅校验，缺少目录时也不下载")
    args = parser.parse_args()
    destination = Path(os.path.abspath(args.destination.expanduser()))
    if destination.is_symlink():
        raise ValueError(f"源码目标目录不能是软链接：{destination}")
    destination = destination.parent.resolve() / destination.name
    pin = json.loads((INTEGRATION / "upstream.json").read_text())
    for key in ("repository", "commit", "version", "bun"):
        if not isinstance(pin.get(key), str) or not pin[key]:
            raise ValueError(f"上游清单缺少 {key}")
    if not re.fullmatch(r"[0-9a-f]{40}", pin["commit"]):
        raise ValueError("上游提交必须是完整的40位 SHA")
    patch = INTEGRATION / "integration.patch"
    paths = patch_paths(patch)
    identity = {"schemaVersion": 1, **{key: pin[key] for key in ("repository", "commit", "version", "bun")}, "patchSha256": sha256(patch)}
    if destination.exists():
        verify(destination, identity, paths)
        print(f"Toonflow 源码校验通过：{destination}")
        return
    if args.verify_only:
        raise ValueError(f"缺少受管源码，请先运行 scripts/prepare-toonflow.py：{destination}")
    work = ROOT / ".toonflow"
    work.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="prepare-", dir=work) as temporary:
        checkout = Path(temporary) / "checkout"
        subprocess.run(["git", "clone", "--filter=blob:none", "--no-checkout", "--", pin["repository"], str(checkout)], check=True)
        subprocess.run(["git", "checkout", "--detach", pin["commit"]], cwd=checkout, check=True)
        actual = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=checkout, text=True).strip()
        if actual != pin["commit"]:
            raise ValueError("检出的上游提交与清单不一致")
        subprocess.run(["git", "apply", "--check", str(patch)], cwd=checkout, check=True)
        subprocess.run(["git", "apply", str(patch)], cwd=checkout, check=True)
        marker = {**identity, "files": {name: file_hash(checkout, name) for name in sorted(paths)}}
        (checkout / MARKER).write_text(json.dumps(marker, ensure_ascii=False, indent=2) + "\n")
        destination.parent.mkdir(parents=True, exist_ok=True)
        # 独占创建目标，避免并发或路径变化时覆盖任何已有目录。
        destination.mkdir()
        shutil.copytree(checkout, destination, symlinks=True, ignore=shutil.ignore_patterns(".git"), dirs_exist_ok=True)
        verify(destination, identity, paths)
    print(f"Toonflow 源码准备完成，尚未安装依赖：{destination}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError, StopIteration) as error:
        raise SystemExit(f"Toonflow 准备失败：{error}") from error
