#!/usr/bin/env python3
"""构建完整桌面编辑器和普通 CLI，记录可核对的构建来源；不启动应用。"""
import argparse
import contextlib
import datetime
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time
import uuid

from photocraft_source import ROOT, digest, load_pin, run, verify


def managed_output(path):
    if ".." in path.parts:
        raise ValueError(f"构建输出不能包含父目录跳转：{path}")
    path = path.expanduser().absolute()
    work = ROOT / ".toonflow"
    # 禁止借助软链接写入其他工程、根 target 或 external/photocraft/target。
    if not path.is_relative_to(work) or path == work:
        raise ValueError(f"构建输出和缓存必须在 {work} 的子目录内：{path}")
    for current in (path, *path.parents):
        if current.is_symlink():
            raise ValueError(f"构建输出不能经由软链接：{path}")
        if current == work:
            break
    return path


def build_command(cargo, source, target_dir, jobs, profile, target, allow_network, pin):
    command = [str(cargo), "build", "--manifest-path", str(source / "Cargo.toml"),
               "--locked" if allow_network else "--frozen", "--bins",
               "-p", pin["desktopPackage"], "-p", pin["cliPackage"],
               "--features", ",".join(pin["features"]), "--target-dir", str(target_dir),
               "--jobs", str(jobs), "--message-format=json-render-diagnostics"]
    if profile == "release":
        command.append("--release")
    if target:
        command.extend(["--target", target])
    return command


def group_running(group):
    """只判断本次 session 的活动成员；已终止、等待系统回收的僵尸不再执行。"""
    try:
        os.killpg(group, 0)
    except ProcessLookupError:
        return False
    states = subprocess.check_output(["ps", "-eo", "pgid=,stat="], text=True)
    return any(len(row) == 2 and row[0] == str(group) and not row[1].startswith("Z")
               for line in states.splitlines() if (row := line.split()))


def terminate_group(process):
    """仅终止由 start_new_session 创建的组；回收直接子进程并核对后代已停止。"""
    for sig, grace in ((signal.SIGTERM, 2.0), (signal.SIGKILL, 3.0)):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + grace
        while time.monotonic() < deadline:
            process.poll()
            if not group_running(process.pid):
                process.wait(timeout=1)
                return
            time.sleep(0.04)
    process.wait(timeout=1)
    raise subprocess.SubprocessError("构建进程组终止后仍有活动成员，不能确认清理完成")


def run_owned_build(command, *, cwd, env, stdout, stderr, timeout):
    if os.name != "posix":
        raise ValueError("当前受控构建脚本仅支持 macOS/Linux；Windows 进程树清理尚未实现，未启动构建")
    process = subprocess.Popen(command, cwd=cwd, env=env, stdout=stdout, stderr=stderr,
                               stdin=subprocess.DEVNULL, start_new_session=True)
    try:
        code = process.wait(timeout=timeout)
        if code:
            raise subprocess.CalledProcessError(code, command)
        if group_running(process.pid):
            # 父进程成功但仍遗留编译器/链接器，不能当作本次完整构建成功。
            raise subprocess.SubprocessError("构建父进程已退出但子进程尚未结束，正在终止本次进程组")
    except BaseException as original:
        try:
            terminate_group(process)
        except BaseException as cleanup:
            original.add_note(f"构建进程组清理也失败：{cleanup}")
        raise


@contextlib.contextmanager
def evidence_attempt(directory):
    evidence = managed_output(directory)
    # 每个尝试独占新目录；拒绝已有目录，避免旧 built 凭证与新失败混在一起。
    evidence.mkdir(parents=True, exist_ok=False)
    provenance = {"schemaVersion": 1, "attemptId": str(uuid.uuid4()), "status": "building",
                  "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  "runtimeAcceptance": "not-run", "guiLaunched": False}
    path = evidence / "provenance.json"
    path.write_text(json.dumps(provenance, indent=2) + "\n")
    try:
        yield evidence, provenance
    except BaseException as error:
        provenance.update(status="failed", error={"type": type(error).__name__, "message": str(error),
                                                  "notes": getattr(error, "__notes__", [])})
        raise
    finally:
        provenance["finishedAt"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        path.write_text(json.dumps(provenance, indent=2) + "\n")


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT / "external/photocraft")
    parser.add_argument("--toolchain-bin", type=Path, required=True, help="已安装 Rust 1.95.0 的 bin 目录")
    parser.add_argument("--rustup-home", type=Path, default=ROOT / ".toonflow/rustup")
    parser.add_argument("--cargo-home", type=Path, default=ROOT / ".toonflow/photocraft-cargo")
    parser.add_argument("--target-dir", type=Path, default=ROOT / ".toonflow/photocraft-target")
    parser.add_argument("--evidence-dir", type=Path, default=ROOT / ".toonflow/photocraft-build")
    parser.add_argument("--profile", choices=("dev", "release"), default="dev")
    parser.add_argument("--target", help="显式目标 triple；省略则使用已安装工具链宿主")
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--timeout", type=int, default=3600, help="构建超时秒数")
    parser.add_argument("--allow-network", action="store_true", help="显式允许 Cargo 下载锁文件中的缺失依赖")
    parser.add_argument("--craft-fonts", type=Path, help="可选、独立核验的 craft-fonts Git 源码目录")
    parser.add_argument("--craft-fonts-commit", help="使用 craft-fonts 时必须显式指定的40位提交")
    return parser.parse_args()


def build_with_evidence(args, evidence, provenance):
    if os.name != "posix":
        raise ValueError("当前受控构建脚本仅支持 macOS/Linux；Windows 进程树清理尚未实现，未启动构建")
    if not 1 <= args.jobs <= 16 or not 1 <= args.timeout <= 86400:
        raise ValueError("jobs 必须为 1..16，timeout 必须为 1..86400")
    pin = load_pin()
    source = args.source.expanduser().absolute()
    identity = verify(source, pin)
    outputs = {name: managed_output(getattr(args, name)) for name in ("cargo_home", "target_dir", "evidence_dir", "rustup_home")}
    tool_bin = args.toolchain_bin.expanduser().resolve(strict=True)
    cargo, rustc, rustdoc = (tool_bin / name for name in ("cargo", "rustc", "rustdoc"))
    for binary in (cargo, rustc, rustdoc):
        if not binary.is_file():
            raise ValueError(f"缺少工具链组件：{binary}")
    environment = os.environ.copy()
    # 构建只使用显式工具链，不改全局默认值；禁用 rustup 隐式安装。
    environment.update({"PATH": str(tool_bin) + os.pathsep + environment.get("PATH", ""),
                        "RUSTC": str(rustc), "RUSTDOC": str(rustdoc), "RUSTUP_AUTO_INSTALL": "0",
                        "RUSTUP_HOME": str(outputs["rustup_home"]), "CARGO_HOME": str(outputs["cargo_home"]),
                        "CARGO_TARGET_DIR": str(outputs["target_dir"]), "RUSTUP_TOOLCHAIN": pin["toolchain"]})
    for name in ("CRAFT_FONTS_DIR", "CRAFT_FONTS_REQUIRED"):
        environment.pop(name, None)
    rust_version = run([str(rustc), "--version", "--verbose"], env=environment, text=True)
    if f"release: {pin['toolchain']}\n" not in rust_version:
        raise ValueError(f"必须使用已安装 Rust {pin['toolchain']} 工具链")
    fonts = {"mode": "system-fallback", "officialReleaseFontParity": False}
    if args.craft_fonts:
        if not args.craft_fonts_commit or not re.fullmatch(r"[0-9a-f]{40}", args.craft_fonts_commit):
            raise ValueError("使用 craft-fonts 必须以 --craft-fonts-commit 指定完整提交")
        font_root = args.craft_fonts.expanduser().resolve(strict=True)
        if run(["git", "status", "--porcelain", "--untracked-files=all"], font_root):
            raise ValueError("craft-fonts 必须为独立、干净的 Git 源码")
        if Path(run(["git", "rev-parse", "--show-toplevel"], font_root, text=True).strip()).resolve() != font_root:
            raise ValueError("craft-fonts 必须为独立 Git 源码目录")
        if run(["git", "remote", "get-url", "origin"], font_root, text=True).strip() != "https://github.com/storytold/craft-fonts.git":
            raise ValueError("craft-fonts 来源不匹配")
        for name in ("fonts/manifest.txt", "ATTRIBUTION.md"):
            if not (font_root / name).is_file():
                raise ValueError(f"craft-fonts 缺少 {name}")
        font_commit = run(["git", "rev-parse", "HEAD"], font_root, text=True).strip()
        if font_commit != args.craft_fonts_commit:
            raise ValueError("craft-fonts HEAD 与显式提交不匹配")
        environment.update({"CRAFT_FONTS_DIR": str(font_root), "CRAFT_FONTS_REQUIRED": "1"})
        fonts = {"mode": "embedded-craft-fonts", "commit": font_commit,
                 "manifestSha256": digest((font_root / "fonts/manifest.txt").read_bytes()), "officialReleaseFontParity": "not-verified"}
    elif args.craft_fonts_commit:
        raise ValueError("--craft-fonts-commit 需要同时指定 --craft-fonts")
    for path in outputs.values():
        path.mkdir(parents=True, exist_ok=True)
    if evidence != outputs["evidence_dir"]:
        raise ValueError("构建证据目录身份变化")
    for name in ("provenance.json", "cargo-messages.jsonl", "cargo-stderr.log"):
        managed_output(evidence / name)
    # 避免上一次成功凭证被误认为本次成功。
    provenance_path = evidence / "provenance.json"
    provenance.update({"source": identity, "profile": args.profile,
                  "target": args.target, "features": pin["features"], "fonts": fonts, "jobs": args.jobs,
                  "rustc": rust_version, "cargo": run([str(cargo), "--version"], env=environment, text=True).strip(),
                  "processOwnership": "isolated-posix-session"})
    command = build_command(cargo, source, outputs["target_dir"], args.jobs, args.profile, args.target, args.allow_network, pin)
    provenance["command"] = command
    provenance_path.write_text(json.dumps(provenance, indent=2) + "\n")
    try:
        with (evidence / "cargo-messages.jsonl").open("w") as stdout, (evidence / "cargo-stderr.log").open("w") as stderr:
            run_owned_build(command, cwd=source, env=environment, stdout=stdout, stderr=stderr, timeout=args.timeout)
        # 构建期间任何来源变动都会取消成功凭证。
        if verify(source, pin) != identity:
            raise ValueError("构建期间源码身份发生变化")
        binaries = {}
        for line in (evidence / "cargo-messages.jsonl").read_text().splitlines():
            message = json.loads(line)
            if message.get("reason") == "compiler-artifact" and message.get("executable"):
                name = message["target"]["name"]
                if name in ("photocraft", "photocraft-cli"):
                    binary = Path(message["executable"])
                    binaries[name] = {"path": str(binary), "sha256": digest(binary.read_bytes()), "bytes": binary.stat().st_size}
        if set(binaries) != {"photocraft", "photocraft-cli"}:
            raise ValueError("构建结果缺少完整桌面编辑器或 CLI 二进制")
        provenance.update(status="built", binaries=binaries)
    except (OSError, ValueError, subprocess.SubprocessError):
        provenance["status"] = "failed"
        raise
    finally:
        provenance["finishedAt"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        provenance_path.write_text(json.dumps(provenance, indent=2) + "\n")
    print(f"PhotoCraft 完整桌面与 CLI 构建成功；运行时验收尚未执行：{provenance_path}")


def main():
    args = parse_args()
    with evidence_attempt(args.evidence_dir) as (evidence, provenance):
        build_with_evidence(args, evidence, provenance)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit(f"PhotoCraft 构建失败：{error}") from error
