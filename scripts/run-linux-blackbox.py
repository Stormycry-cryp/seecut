#!/usr/bin/env python3
"""Run one normal Linux App and an independent, trusted black-box QA script.

Legacy QA receives --app-pid, --client-binary, --work-dir, --output, --expected-sha,
--deadline-monotonic, and --next-stage. An absent optional client is passed as a nonexistent
absolute work path. QA records /proc binary identity and the unique visible X11
window ID itself. It must finish before the monotonic deadline, leave regular
files in output or its known observation/navigation directories, and never
include secrets in artifacts. Optional --identity-approval and --ui-approval are
copied verbatim and forwarded; this harness never creates or interprets a declaration.
An optional candidate manifest binds the copied executable to its actual source
HEAD, build run/job and content hash before launch. Explicit source HEAD and
artifact ID allow a reviewed test-only workflow revision to use the original App.
The explicit workflow-observation stage instead uses its reviewed fixed script
and helper, window ID, approvals and isolated-display flags without legacy args.
QA receives the actual App source HEAD. This contract supports one App lifetime:
QA must not launch another App,
detach descendants, or move them into new process groups. This harness supplies
no UI navigation, authorization, product assertions, or product test results.
It is resource isolation for trusted QA, not a security sandbox: HOME and X11
remain available. A zero harness exit establishes only QA exit and cleanup.
"""

import argparse
from collections import deque
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time


LOG_LIMIT = 256 * 1024
ARTIFACT_LIMIT = 15 * 1024 * 1024
LINE_LIMIT = 16 * 1024
SENSITIVE = re.compile(rb"token|authorization|credential|password|secret|bearer", re.I)
RESERVED = {"harness.json", "app.log", "qa.log"}
QA_DIRECTORIES = {"independent-qa-observation", "independent-qa-navigation"}
WORKFLOW_DIRECTORY = "independent-qa-workflow"
WORKFLOW_SCRIPTS = {
    "qa": ("independent-workflow-ui.py", "e4c16c49450019164c70f6cf4f96bafaaec95224e3341ac263d4f42fbb4c7aca"),
    "public_ui_probe": ("public_ui_probe.py", "347f78240b81af991947d0f343648ae1aeffa132589fd20da1a4dfe8c99d1f79"),
    "workflow_checks": ("workflow_checks.py", "de496988f07b0846c5055c3884948233f1a885c40dc88b7aa721825359fcf65f"),
}
WORKFLOW_PNG_NAMES = {
    name + ".png" for name in (
        "01-current-initial", "02-after-quick", "03-gallery", "04-create-dialog", "05-editor", "06-open-menu",
        "12-left-saved-editor", "13-new-blank-dialog", "14-new-blank-editor", "15-reopen-menu", "19-export-destination",
    )
} | {
    phase + suffix + ".png"
    for phase in ("07-import", "10-save", "16-reopen", "20-export")
    for suffix in ("-ambiguous-native", "-native-outside-display", "-native-visible", "-no-visible-native-after-wait")
} | {
    phase + suffix + ".png"
    for phase in ("08-import", "11-save", "17-reopen", "21-export")
    for suffix in ("-returned-App", "-menu-closed", "-native-not-gone")
} | {
    phase + suffix + ".png"
    for phase in ("09-edit", "18-reopened-edit") for suffix in ("-dragged", "-one-Undo")
}
WORKFLOW_JSON_NAMES = {"workflow.json", "public-accessibility.json"} | {
    phase + suffix + ".json"
    for phase in ("08-import", "11-save", "17-reopen", "21-export")
    for suffix in ("-public-before", "-public-location", "-public-before-accept")
} | {"19-export-public-" + str(index) + ".json" for index in range(1, 4)}
WORKFLOW_FILE_LIMIT = 2 * 1024 * 1024
WORKFLOW_PNG_LIMIT = 14 * 1024 * 1024
WORKFLOW_METADATA_LIMIT = 128 * 1024
WORKFLOW_FIXTURE_MANIFEST = "8926b97d3370005fa008bcac6cf21fc287d3717448968591b1d705df76a290a7"
WORKFLOW_FIXTURES = {
    "opaque-quadrants.png": (800, "0928c47fa44250879270def6198e04fd939dd8250864179760203f0d334a6d63"),
    "transparent-markers.png": (849, "8201fa2b0c7c94de97433462d40ae97aa43b77e395921769d967c7f168875b75"),
    "fully-transparent.png": (83, "ba04f531df0c7a12124750d521add77c55b16a6432d653c5559c16680dbd9f50"),
}
APPROVAL_LIMIT = 16 * 1024
APP_LIMIT = 512 * 1024 * 1024
HARNESS_RESERVE = 2 * LOG_LIMIT + 32 * 1024


class StopRun(Exception):
    pass


class BoundedLog:
    """Drain continuously; retain only complete redacted lines in bounded RAM."""

    def __init__(self, stream):
        self.stream = stream
        self.lines = deque()
        self.size = 0
        self.truncated = False
        self.lock = threading.Lock()
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()

    def _append(self, line):
        if SENSITIVE.search(line):
            line = b"[redacted sensitive line]\n"
        with self.lock:
            self.lines.append(line)
            self.size += len(line)
            while self.size > LOG_LIMIT:
                self.size -= len(self.lines.popleft())
                self.truncated = True

    def _read(self):
        pending = bytearray()
        oversized = False
        try:
            while True:
                chunk = self.stream.read(4096)
                if not chunk:
                    break
                for part in chunk.splitlines(keepends=True):
                    ends_line = part.endswith((b"\n", b"\r"))
                    if not oversized:
                        pending.extend(part)
                        if len(pending) > LINE_LIMIT:
                            pending.clear()
                            oversized = True
                    if ends_line:
                        self._append(b"[oversized line omitted]\n" if oversized else bytes(pending))
                        pending.clear()
                        oversized = False
            if oversized or pending:
                self._append(b"[oversized line omitted]\n" if oversized else bytes(pending) + b"\n")
        finally:
            self.stream.close()

    def snapshot(self):
        with self.lock:
            return b"".join(self.lines)


def regular_input(value):
    path = Path(value).absolute()
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode):
        raise ValueError("inputs must be regular files, not symlinks")
    return path


def fresh_output(value):
    path = Path(value)
    if not path.is_absolute():
        raise ValueError("--output must be absolute")
    if ".." in path.parts:
        raise ValueError("--output must not contain parent traversal")
    for parent in reversed(path.parents):
        if parent.is_symlink() or not parent.is_dir():
            raise ValueError("--output ancestors must be existing real directories")
    path.mkdir(mode=0o700)  # Existing paths, including dangling symlinks, fail.
    return path


def copy_and_hash(source, destination, executable=False, limit=None):
    digest = hashlib.sha256()
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as incoming, destination.open("xb") as outgoing:
        if not stat.S_ISREG(os.fstat(incoming.fileno()).st_mode):
            raise ValueError("input changed to a non-regular file")
        copied_bytes = 0
        while True:
            chunk = incoming.read(1024 * 1024)
            if not chunk:
                break
            copied_bytes += len(chunk)
            if limit is not None and copied_bytes > limit:
                raise ValueError("input exceeds its copy budget")
            digest.update(chunk)
            outgoing.write(chunk)
    destination.chmod(0o700 if executable else 0o600)
    return digest.hexdigest()


def copy_workflow_fixtures(source, work):
    """Copy only the reviewed owned fixture inputs, never an arbitrary tree."""
    if not source.is_absolute() or ".." in source.parts:
        raise ValueError("--input-dir must be an absolute owned fixture directory")
    for path in (source, *source.parents):
        if not stat.S_ISDIR(path.lstat().st_mode):
            raise ValueError("fixture directory ancestors must be real directories")
    if source.stat().st_uid != os.getuid():
        raise ValueError("fixture directory must be owned by this runner")
    if {item.name for item in source.iterdir()} != {"manifest.json", *WORKFLOW_FIXTURES}:
        raise ValueError("fixture directory must contain exactly the reviewed four inputs")
    destination = work / "qa-input"
    destination.mkdir(mode=0o700)
    hashes = {}
    for name in ("manifest.json", *WORKFLOW_FIXTURES):
        incoming = regular_input(source / name)
        info = incoming.lstat()
        if info.st_uid != os.getuid() or info.st_nlink != 1:
            raise ValueError("fixture inputs must be owned regular files with one link")
        maximum = APPROVAL_LIMIT if name == "manifest.json" else WORKFLOW_FIXTURES[name][0]
        digest = copy_and_hash(incoming, destination / name, limit=maximum)
        expected = WORKFLOW_FIXTURE_MANIFEST if name == "manifest.json" else WORKFLOW_FIXTURES[name][1]
        if digest != expected or (name != "manifest.json" and (destination / name).stat().st_size != maximum):
            raise ValueError("copied fixture differs from its reviewed identity")
        hashes[name] = digest
    return destination, hashes


def spawn(command, work, env):
    return subprocess.Popen(command, cwd=work, env=env, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            start_new_session=True)


def kill_group(process, signum):
    try:
        os.killpg(process.pid, signum)
    except ProcessLookupError:
        pass


def group_exists(process):
    try:
        os.killpg(process.pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        # EPERM also establishes existence (e.g. a transient macOS zombie in
        # helper self-checks). Keep waiting/reaping rather than skipping cleanup.
        return True


def stop_processes(processes, deadline):
    # Signal even exited leaders: their descendants may still own pipes.
    for process in processes:
        kill_group(process, signal.SIGTERM)
    grace = min(deadline, time.monotonic() + 5)
    while time.monotonic() < grace:
        for process in processes:
            process.poll()  # Reap a leader without forgetting its group.
        if not any(group_exists(process) for process in processes):
            break
        time.sleep(min(0.05, max(0, grace - time.monotonic())))
    for process in processes:
        kill_group(process, signal.SIGKILL)
    for process in processes:
        process.wait(timeout=max(0.01, deadline - time.monotonic()))


def inspect_artifacts(output, limit=ARTIFACT_LIMIT, next_stage=None):
    """Traverse only the QA contract's known directory, never links."""
    if not stat.S_ISDIR(output.lstat().st_mode):
        raise ValueError("output root must remain a real directory")
    total = 0
    workflow = next_stage == "workflow-observation"
    directories = {WORKFLOW_DIRECTORY} if workflow else QA_DIRECTORIES
    png_count = png_bytes = project_count = 0
    pending = [output]
    while pending:
        directory = pending.pop()
        with os.scandir(directory) as entries:
            for entry in entries:
                info = entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode) and directory == output and entry.name in directories:
                    pending.append(Path(entry.path))
                    continue
                if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                    raise ValueError("output contains an unknown directory, link, or non-regular file")
                if workflow:
                    if directory == output:
                        if entry.name not in RESERVED:
                            raise ValueError("workflow artifacts require the known QA directory")
                    elif entry.name in WORKFLOW_PNG_NAMES or entry.name == "exported-qa.png":
                        png_count += entry.name != "exported-qa.png"
                        png_bytes += info.st_size
                        if png_count > 40 or info.st_size > WORKFLOW_FILE_LIMIT or png_bytes > WORKFLOW_PNG_LIMIT:
                            raise ValueError("workflow PNG count or byte budget exceeded")
                    elif entry.name in WORKFLOW_JSON_NAMES:
                        if info.st_size > WORKFLOW_METADATA_LIMIT:
                            raise ValueError("workflow metadata exceeds 128 KiB")
                    elif re.fullmatch(r"saved-project(?:\.[A-Za-z0-9_-]{1,16})?", entry.name):
                        project_count += 1
                        if project_count > 1 or info.st_size > WORKFLOW_FILE_LIMIT:
                            raise ValueError("workflow requires at most one bounded saved project")
                    else:
                        raise ValueError("unknown workflow artifact file")
                total += info.st_size
                if total > limit:
                    raise ValueError("output exceeds artifact budget")
    return total


def wait_for_window(app, work, env, deadline):
    if shutil.which("xdotool") is None:
        raise ValueError("xdotool is required for visible-window readiness")
    until = min(deadline, time.monotonic() + 60)
    while time.monotonic() < until:
        if app.poll() is not None:
            raise ValueError("App exited before one visible window became ready")
        probe = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(app.pid)],
                               cwd=work, env=env, capture_output=True, text=True,
                               timeout=min(5, max(0.01, until - time.monotonic())))
        if probe.returncode not in (0, 1):
            raise ValueError("visible-window search failed")
        windows = sorted(set(int(value) for value in probe.stdout.split()))
        if len(windows) > 1:
            raise ValueError("multiple visible App windows prevent unique identification")
        if windows:
            return windows[0]
        time.sleep(min(0.25, max(0, until - time.monotonic())))
    raise ValueError("no unique visible App window within readiness deadline")


def write_owned(output, name, payload):
    # Never follow or overwrite anything the QA has placed at a reserved path.
    fd = os.open(output / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as handle:
        handle.write(payload)


def verify_private_accessibility_bus(env):
    """Only the workflow's direct dbus-run-session child can attest this bus."""
    if not env.get("DBUS_SESSION_BUS_ADDRESS"):
        raise ValueError("private accessibility bus requires DBUS_SESSION_BUS_ADDRESS")
    parent = Path("/proc") / str(os.getppid())
    if (parent.stat().st_uid != os.getuid()
            or (parent / "exe").resolve(strict=True).name != "dbus-run-session"):
        raise ValueError("private accessibility bus requires a direct dbus-run-session parent")


def qa_command(args, copied, app_pid, window_id, work, output, source_head, qa_deadline):
    command = [sys.executable, str(copied["qa"]), "--app-pid", str(app_pid),
               "--work-dir", str(work), "--output", str(output),
               "--deadline-monotonic", str(qa_deadline)]
    if args.next_stage == "workflow-observation":
        command.extend(("--window-id", str(window_id), "--input-dir", str(copied["input_dir"])))
        if args.private_accessibility_bus:
            command.extend(("--private-accessibility-bus", "--probe-python", "/usr/bin/python3"))
    else:
        command.extend(("--client-binary", str(copied["client"]),
                        "--expected-sha", source_head, "--next-stage", args.next_stage))
    if args.isolated_display_capture:
        command.append("--isolated-display-capture")
    for name in ("identity_approval", "ui_approval"):
        if name in copied:
            command.extend(("--" + name.replace("_", "-"), str(copied[name])))
    return command


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "qa-script", "output", "expected-sha"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--client-binary")
    parser.add_argument("--candidate-manifest")
    parser.add_argument("--candidate-artifact-id")
    parser.add_argument("--source-head", help="Exact immutable App source HEAD; defaults to workflow HEAD")
    parser.add_argument("--identity-approval")
    parser.add_argument("--ui-approval")
    parser.add_argument("--input-dir", help="workflow-observation only; exact owned fixture inputs")
    parser.add_argument("--next-stage", choices=("observe-only", "navigation", "project-entry", "canvas-create-observation", "canvas-create-entry", "editor-entry-observation", "image-picker-observation", "workflow-observation"), default="navigation")
    parser.add_argument("--isolated-display-capture", action="store_true",
                        help="Explicit isolated-display declaration for image-picker/workflow observation")
    parser.add_argument("--private-accessibility-bus", action="store_true",
                        help="workflow-observation only; requires a direct dedicated dbus-run-session parent")
    parser.add_argument("--seconds", type=int, choices=(300, 420), default=300)
    args = parser.parse_args()
    capture_stages = {"image-picker-observation", "workflow-observation"}
    if args.next_stage in capture_stages and not args.isolated_display_capture:
        parser.error(args.next_stage + " requires --isolated-display-capture")
    if args.isolated_display_capture and args.next_stage not in capture_stages:
        parser.error("--isolated-display-capture requires an authorized observation stage")
    if args.private_accessibility_bus and args.next_stage != "workflow-observation":
        parser.error("--private-accessibility-bus is only valid for workflow-observation")
    if args.input_dir is not None and args.next_stage != "workflow-observation":
        parser.error("--input-dir is only valid for workflow-observation")
    if args.next_stage == "workflow-observation":
        if args.seconds != 300 or args.client_binary is not None:
            parser.error("workflow-observation requires 300 seconds and no MCP client")
        if not all((args.identity_approval, args.ui_approval, args.source_head,
                    args.candidate_manifest, args.candidate_artifact_id, args.input_dir)):
            parser.error("workflow-observation requires exact candidate provenance, declarations and owned fixtures")
    return args


def main():
    args = arguments()
    started = time.monotonic()
    deadline = started + args.seconds
    qa_deadline = deadline - 15
    processes, logs = [], []
    output = work = None
    phase = "setup"
    result = {"schema": 1, "status": "incomplete", "seconds": args.seconds,
              "next_stage": args.next_stage,
              "isolated_display_capture": args.isolated_display_capture,
              "private_accessibility_bus": args.private_accessibility_bus,
              "isolated_display_capture_source": ("explicit CLI option --isolated-display-capture"
                                                  if args.isolated_display_capture else None),
              "qa_contract": "one App lifetime; no detached children; known QA directory; secret-free artifacts",
              "product_acceptance": "not established by this harness"}
    exit_code = 1

    def interrupted(signum, _frame):
        if phase == "cleanup":
            return
        signal.setitimer(signal.ITIMER_REAL, 0)
        raise StopRun("signal " + str(signum))

    def hard_stop(_signum, _frame):
        for process in processes:
            kill_group(process, signal.SIGKILL)
        os._exit(124)  # Final wall limit; partial output/work may survive.

    previous = {sig: signal.signal(sig, interrupted)
                for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGALRM)}
    signal.setitimer(signal.ITIMER_REAL, max(0.001, qa_deadline - time.monotonic()))
    try:
        if platform.system() != "Linux":
            raise ValueError("normal App black-box runs require Linux")
        if not re.fullmatch(r"[0-9a-f]{40}", args.expected_sha):
            raise ValueError("--expected-sha must be 40 lowercase hex characters")
        if not os.environ.get("DISPLAY"):
            raise ValueError("DISPLAY is required")
        if args.private_accessibility_bus:
            verify_private_accessibility_bus(os.environ)
        if "SEECUT_UI_PREVIEW_DIR" in os.environ:
            raise ValueError("SEECUT_UI_PREVIEW_DIR must be absent")
        root = Path(__file__).resolve().parents[1]
        head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"],
                                       timeout=min(10, max(0.01, qa_deadline - time.monotonic())),
                                       stderr=subprocess.DEVNULL, text=True).strip()
        if head != args.expected_sha:
            raise ValueError("checkout HEAD does not match --expected-sha")
        source_head = args.source_head if args.source_head is not None else head
        if not re.fullmatch(r"[0-9a-f]{40}", source_head):
            raise ValueError("--source-head must be 40 lowercase hex characters")
        if args.source_head is not None and args.candidate_manifest is None:
            raise ValueError("explicit App source HEAD requires an immutable candidate manifest")
        if source_head != head and args.candidate_artifact_id is None:
            raise ValueError("cross-HEAD App source requires an exact candidate artifact ID")
        candidate = None
        if args.candidate_manifest is not None:
            if not Path(args.candidate_manifest).is_absolute():
                raise ValueError("--candidate-manifest must be absolute")
            source = regular_input(args.candidate_manifest)
            fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, "rb") as stream:
                if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                    raise ValueError("candidate manifest changed to a non-regular file")
                raw = stream.read(APPROVAL_LIMIT + 1)
            if len(raw) > APPROVAL_LIMIT:
                raise ValueError("candidate manifest exceeds 16 KiB")
            candidate = json.loads(raw)
            fields = {"schema", "repository", "source_head", "platform", "app_sha256", "app_bytes",
                      "build_run_id", "build_job_id", "build_job", "build_job_name", "build_step_name"}
            if not isinstance(candidate, dict) or set(candidate) != fields:
                raise ValueError("invalid candidate manifest fields")
            if (type(candidate["schema"]) is not int or candidate["schema"] != 1
                    or candidate["repository"] != "Stormycry-cryp/seecut"
                    or candidate["source_head"] != source_head
                    or candidate["platform"] != "Linux x86_64" or platform.machine() != "x86_64"
                    or candidate["build_job"] != "engine"
                    or candidate["build_job_name"] != "Linux independent black-box QA"
                    or candidate["build_step_name"] != "Build the normal window candidate"
                    or not isinstance(candidate["app_sha256"], str)
                    or not re.fullmatch(r"[0-9a-f]{64}", candidate["app_sha256"])):
                raise ValueError("candidate provenance does not match the requested App source and platform")
            for key in ("app_bytes", "build_run_id", "build_job_id"):
                if type(candidate[key]) is not int or candidate[key] <= 0:
                    raise ValueError("candidate manifest requires positive integer sizes and IDs")
            if candidate["app_bytes"] > APP_LIMIT:
                raise ValueError("candidate App exceeds 512 MiB")
            result["candidate"] = dict(candidate,
                manifest_sha256=hashlib.sha256(raw).hexdigest(), source_artifact_id=None)
        if args.candidate_artifact_id is not None:
            if candidate is None or not re.fullmatch(r"[1-9][0-9]*", args.candidate_artifact_id):
                raise ValueError("candidate artifact ID requires a valid manifest")
            result["candidate"]["source_artifact_id"] = int(args.candidate_artifact_id)
        sources = {"app": regular_input(args.binary), "qa": regular_input(args.qa_script)}
        if args.next_stage == "workflow-observation":
            if sources["qa"] != root / "scripts" / WORKFLOW_SCRIPTS["qa"][0]:
                raise ValueError("workflow-observation requires the fixed reviewed QA script")
            for name, (filename, _digest) in WORKFLOW_SCRIPTS.items():
                if name != "qa":
                    sources[name] = regular_input(root / "scripts" / filename)
        if args.client_binary is not None:
            if not Path(args.client_binary).is_absolute():
                raise ValueError("--client-binary must be absolute")
            sources["client"] = regular_input(args.client_binary)
        approval = None
        if args.identity_approval is not None:
            if not Path(args.identity_approval).is_absolute():
                raise ValueError("--identity-approval must be absolute")
            approval = regular_input(args.identity_approval)
            if approval.lstat().st_size > APPROVAL_LIMIT:
                raise ValueError("--identity-approval exceeds 16 KiB")
        ui_approval = None
        if args.ui_approval is not None:
            if not Path(args.ui_approval).is_absolute():
                raise ValueError("--ui-approval must be absolute")
            ui_approval = regular_input(args.ui_approval)
            if ui_approval.lstat().st_size > APPROVAL_LIMIT:
                raise ValueError("--ui-approval exceeds 16 KiB")
        output = fresh_output(args.output)
        work = Path(tempfile.mkdtemp(prefix="seecut-blackbox-"))
        copied = {"app": work / "concat", "client": work / "concat-editor-mcp",
                  "qa": work / "independent-qa.py"}
        if args.next_stage == "workflow-observation":
            for name, (filename, _digest) in WORKFLOW_SCRIPTS.items():
                if name != "qa":
                    copied[name] = work / filename
        hashes = {name: copy_and_hash(source, copied[name], executable=name in {"app", "client"},
                                      limit=APP_LIMIT if name == "app" else None)
                  for name, source in sources.items()}
        if args.next_stage == "workflow-observation":
            for name, (_filename, digest) in WORKFLOW_SCRIPTS.items():
                if hashes[name] != digest:
                    raise ValueError("workflow script differs from its reviewed SHA256")
            copied["input_dir"], fixture_hashes = copy_workflow_fixtures(Path(args.input_dir), work)
            result["input_fixture_sha256"] = fixture_hashes
        if candidate is not None:
            if (hashes["app"] != candidate["app_sha256"]
                    or copied["app"].stat().st_size != candidate["app_bytes"]):
                raise ValueError("copied App does not match the immutable candidate manifest")
        if approval is not None:
            copied["identity_approval"] = work / "identity-approval.json"
            hashes["identity_approval"] = copy_and_hash(
                approval, copied["identity_approval"], limit=APPROVAL_LIMIT)
        if ui_approval is not None:
            copied["ui_approval"] = work / "ui-approval.json"
            hashes["ui_approval"] = copy_and_hash(
                ui_approval, copied["ui_approval"], limit=APPROVAL_LIMIT)
        portable = work / "portable"
        portable.mkdir(mode=0o700)
        prefs = {"locale": "en", "dark": False, "server": {"enabled": False}}
        (portable / "settings.json").write_text(json.dumps(prefs), encoding="utf-8")
        env = os.environ.copy()
        if args.private_accessibility_bus:
            for key in ("AT_SPI_BUS_ADDRESS", "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE"):
                env.pop(key, None)
        env["SLINT_SCALE_FACTOR"] = "1"
        for key in ("TMPDIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
            isolated = work / key.lower()
            isolated.mkdir(mode=0o700)
            env[key] = str(isolated)
        for key in ("SEECUT_MCP_CLIENT_TOKEN", "SEECUT_MCP_WRITE_TOKEN"):
            env.pop(key, None)
        result.update(head=head, workflow_head=head, source_head=source_head,
                      expected_head=args.expected_sha, sha256=hashes,
                      app_binary=str(copied["app"]),
                      platform=platform.platform(), display=env["DISPLAY"],
                      slint_scale_factor=env["SLINT_SCALE_FACTOR"],
                      cpu={"logical_count": os.cpu_count(), "machine": platform.machine(),
                           "slint_wgpu_cpu": env.get("SLINT_WGPU_CPU") == "1"},
                      vulkan_icd=("[redacted]" if SENSITIVE.search(env.get("VK_ICD_FILENAMES", "").encode())
                                  else env.get("VK_ICD_FILENAMES", "")),
                      startup_preferences=prefs,
                      preference_source="explicit isolated configuration; not UI observation",
                      isolation="copied executable beside fresh portable; preserved HOME; private TMPDIR/XDG",
                      limits={"artifact_total_bytes": ARTIFACT_LIMIT,
                              "wall_seconds": args.seconds, "cleanup_reserve_seconds": 15,
                              "log_bytes_per_process": LOG_LIMIT})
        phase = "launch"
        app = spawn([str(copied["app"])], work, env)
        processes.append(app)
        logs.append(BoundedLog(app.stdout))
        result["app_pid"] = app.pid
        phase = "visible_window_readiness"
        result["app_window_id"] = wait_for_window(app, work, env, qa_deadline)
        result["client_binary_available"] = args.client_binary is not None
        command = qa_command(args, copied, app.pid, result["app_window_id"],
                             work, output, source_head, qa_deadline)
        qa = spawn(command, work, env)
        processes.append(qa)
        logs.append(BoundedLog(qa.stdout))
        result["qa_pid"] = qa.pid
        phase = "qa"
        while qa.poll() is None:
            inspect_artifacts(output, ARTIFACT_LIMIT - HARNESS_RESERVE, args.next_stage)
            if time.monotonic() >= qa_deadline:
                interrupted(signal.SIGALRM, None)
            time.sleep(min(0.25, max(0, qa_deadline - time.monotonic())))
        result["qa_exit_code"] = qa.returncode
        result["status"] = {0: "qa_exited", 3: "qa_review_required"}.get(qa.returncode, "qa_failed")
        exit_code = qa.returncode if qa.returncode in (0, 3) else 1
    except StopRun:
        result["status"] = "interrupted_or_timed_out"
        exit_code = 124
    except (OSError, ValueError, subprocess.SubprocessError):
        result["status"] = "harness_error"
        result["error_phase"] = phase  # Never dump arbitrary stderr/exception secrets.
    finally:
        phase = "cleanup"
        signal.setitimer(signal.ITIMER_REAL, 0)
        # Ignore repeated cancellation during bounded cleanup; ALRM still kills.
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(sig, signal.SIG_IGN)
        signal.signal(signal.SIGALRM, hard_stop)
        signal.setitimer(signal.ITIMER_REAL, max(0.001, deadline - time.monotonic()))
        try:
            stop_processes(processes, deadline)
            for log in logs:
                log.thread.join(timeout=min(0.5, max(0, deadline - time.monotonic())))
            result["process_exit_codes"] = [process.returncode for process in processes]
            result["log_truncated"] = [log.truncated for log in logs]
            for process in processes:
                if process.stdout is not None and not logs:
                    process.stdout.close()
            if work is not None:
                shutil.rmtree(work)  # Only this invocation's mkdtemp directory.
            result["owned_work_removed"] = work is None or not work.exists()
            if output is not None:
                qa_bytes = inspect_artifacts(output, next_stage=args.next_stage)
                if any((output / name).exists() or (output / name).is_symlink() for name in RESERVED):
                    raise ValueError("QA used a reserved harness output name")
                payloads = {name: log.snapshot() for name, log in zip(("app.log", "qa.log"), logs)}
                result["elapsed_seconds"] = round(time.monotonic() - started, 3)
                payloads["harness.json"] = (json.dumps(result, indent=2) + "\n").encode()
                if qa_bytes + sum(map(len, payloads.values())) > ARTIFACT_LIMIT:
                    raise ValueError("combined output exceeds 15 MiB")
                for name, payload in payloads.items():
                    write_owned(output, name, payload)
                inspect_artifacts(output, next_stage=args.next_stage)
        except (OSError, ValueError, subprocess.SubprocessError):
            result["status"] = "cleanup_or_artifact_error"
            exit_code = 1
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            for sig, handler in previous.items():
                signal.signal(sig, handler)
    print(json.dumps({"status": result["status"], "product_acceptance": result["product_acceptance"]}))
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
