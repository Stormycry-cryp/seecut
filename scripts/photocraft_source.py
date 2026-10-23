"""PhotoCraft 固定来源、完整文件校验和受控补丁；不执行上游代码。"""
from pathlib import Path, PurePosixPath
import contextlib
import hashlib
import io
import json
import os
import re
import stat
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
INTEGRATION = ROOT / "integrations/photocraft"
REPOSITORY = "https://github.com/storytold/photocraft.git"
COMMIT = "ff4738810134d7f7e68b49f2e4f231b686282ff3"


def run(argv, cwd=None, **kwargs):
    environment = kwargs.pop("env", os.environ).copy()
    environment["GIT_NO_REPLACE_OBJECTS"] = "1"
    kwargs["env"] = environment
    return subprocess.check_output(argv, cwd=cwd, **kwargs)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def load_pin(integration=INTEGRATION):
    pin = json.loads((integration / "upstream.json").read_text())
    required = {"schemaVersion": 1, "repository": REPOSITORY, "commit": COMMIT,
                "version": "0.14.0", "minimumRust": "1.95", "toolchain": "1.95.0",
                "desktopPackage": "photocraft", "cliPackage": "photocraft-cli",
                "features": ["photocraft/heif", "photocraft-cli/heif"]}
    if any(pin.get(k) != v for k, v in required.items()):
        raise ValueError("PhotoCraft 固定来源或构建合同不匹配")
    patches = pin.get("patches")
    if not isinstance(patches, list):
        raise ValueError("patches 必须为按应用顺序排列的列表")
    seen = set()
    for patch in patches:
        if not isinstance(patch, dict) or set(patch) != {"path", "sha256"}:
            raise ValueError("补丁必须包含且仅包含 path 和 sha256")
        name = patch["path"]
        safe_relative(name)
        if not name.startswith("patches/") or name in seen:
            raise ValueError("补丁必须位于 patches/ 且不能重复")
        seen.add(name)
        if not isinstance(patch["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", patch["sha256"]):
            raise ValueError("补丁 SHA-256 无效")
        path = integration / name
        no_symlink(path, integration)
        if digest(path.read_bytes()) != patch["sha256"]:
            raise ValueError(f"补丁哈希不匹配：{name}")
        patch_paths(path)
    return pin


def safe_relative(name):
    if not isinstance(name, str) or not name or "\\" in name:
        raise ValueError(f"无效相对路径：{name}")
    path = PurePosixPath(name)
    if path.is_absolute() or any(p in ("..", ".git") for p in path.parts) or str(path) != name:
        raise ValueError(f"不安全相对路径：{name}")
    return path


def no_symlink(path, root):
    path.relative_to(root)
    for current in (path, *path.parents):
        if current.is_symlink():
            raise ValueError(f"路径不能经由软链接：{path}")
        if current == root:
            break


def patch_paths(path):
    data = run(["git", "apply", "--numstat", "-z", str(path)])
    records = iter(data.split(b"\0"))
    paths = set()
    for record in records:
        if not record:
            continue
        fields = record.split(b"\t", 2)
        if len(fields) != 3:
            raise ValueError("无法解析补丁文件列表")
        names = [fields[2]] if fields[2] else [next(records), next(records)]
        for name in names:
            text = name.decode()
            safe_relative(text)
            paths.add(text)
    if not paths:
        raise ValueError("补丁没有文件变更")
    return paths


def repository_identity(source, pin, check_worktree=True, integration=INTEGRATION):
    no_symlink(source, source)
    if not source.is_dir():
        raise ValueError(f"缺少完整 PhotoCraft 源码：{source}")
    top = Path(run(["git", "rev-parse", "--show-toplevel"], source, text=True).strip()).resolve()
    if top != source.resolve():
        raise ValueError("PhotoCraft 必须是独立的 Git 仓库或 worktree")
    origin = run(["git", "remote", "get-url", "origin"], source, text=True).strip()
    if origin != pin["repository"]:
        raise ValueError(f"PhotoCraft origin 不匹配：{origin}")
    head = run(["git", "rev-parse", "HEAD"], source, text=True).strip()
    if head != pin["commit"]:
        raise ValueError(f"PhotoCraft HEAD 不匹配：{head}")
    if not check_worktree:
        return
    # 不接受暂存内容，也不借助 assume-unchanged / skip-worktree 隐藏工作区差异。
    if run(["git", "diff", "--cached", "--name-only", "-z"], source):
        raise ValueError("PhotoCraft 有暂存修改，保留并停止")
    extras = run(["git", "ls-files", "--others", "--exclude-standard", "-z"], source)
    ignored = run(["git", "ls-files", "--others", "--ignored", "--exclude-standard", "-z"], source)
    allowed = set().union(*(patch_paths(integration / p["path"]) for p in pin["patches"]))
    unknown = {p.decode() for p in (extras + ignored).split(b"\0") if p} - allowed
    if unknown:
        raise ValueError(f"PhotoCraft 有未知未跟踪或忽略文件，保留并停止：{', '.join(sorted(unknown))[:1000]}")


def snapshot(directory):
    """文件内容和可执行位参与校验；完整源码不允许软链接或特殊文件。"""
    files = {}
    for parent, dirs, names in os.walk(directory, followlinks=False):
        dirs[:] = sorted(d for d in dirs if not (d == ".git" and Path(parent) == directory))
        for name in [*dirs, *names]:
            path = Path(parent) / name
            if path.name == ".git" and path.parent == directory:
                continue  # Git worktree 的 .git 是管理指针文件。
            mode = path.lstat().st_mode
            relative = path.relative_to(directory).as_posix()
            if stat.S_ISLNK(mode) or not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
                raise ValueError(f"源码包含软链接或特殊文件：{relative}")
            if stat.S_ISREG(mode):
                files[relative] = {"sha256": digest(path.read_bytes()), "executable": bool(mode & 0o111)}
    return files


@contextlib.contextmanager
def canonical(source, pin, integration=INTEGRATION):
    """从固定 Git 对象重放补丁，不修改已有源码或其 Git 索引。"""
    archive = run(["git", "archive", "--format=tar", pin["commit"]], source)
    with tempfile.TemporaryDirectory(prefix="photocraft-verify-") as temporary:
        directory = Path(temporary)
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            for member in tar.getmembers():
                safe_relative(member.name.rstrip("/"))
                if not (member.isfile() or member.isdir()):
                    raise ValueError(f"上游包含不支持的文件类型：{member.name}")
            tar.extractall(directory, filter="data")
        for patch in pin["patches"]:
            path = integration / patch["path"]
            run(["git", "apply", "--check", str(path)], directory)
            run(["git", "apply", str(path)], directory)
        yield directory


def verify(source, pin=None, integration=INTEGRATION):
    pin = pin or load_pin(integration)
    repository_identity(source, pin, integration=integration)
    with canonical(source, pin, integration) as expected:
        expected_files = snapshot(expected)
    actual_files = snapshot(source)
    mismatches = sorted(k for k in expected_files.keys() | actual_files.keys()
                        if expected_files.get(k) != actual_files.get(k))
    if mismatches:
        raise ValueError("源码存在未登记修改或缺失：" + ", ".join(mismatches[:20]))
    workspace = tomllib.loads((source / "Cargo.toml").read_text())["workspace"]["package"]
    if workspace["version"] != pin["version"] or workspace["rust-version"] != pin["minimumRust"]:
        raise ValueError("源码版本或最低 Rust 不匹配")
    identity = {"repository": pin["repository"], "commit": pin["commit"], "version": pin["version"],
                "patches": pin["patches"], "fileCount": len(expected_files),
                "sourceTreeSha256": digest(json.dumps(expected_files, sort_keys=True, separators=(",", ":")).encode()),
                "cargoLockSha256": digest((source / "Cargo.lock").read_bytes())}
    return identity
