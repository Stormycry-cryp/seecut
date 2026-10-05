#!/usr/bin/env python3
"""Run one normal Linux App and an independent, trusted black-box QA script.

Legacy QA receives --app-pid, --client-binary, --work-dir, --output, --expected-sha,
--deadline-monotonic, and --next-stage. An absent optional client is passed as a nonexistent
absolute work path. QA records /proc binary identity and the unique visible X11
window ID itself. It must finish before the monotonic deadline, leave regular
files in output or its known observation/navigation directories, and never
include secrets in artifacts. Optional --identity-approval and --ui-approval are
copied verbatim and forwarded; fixed reviewed stages verify their raw identities.
An optional candidate manifest binds the copied executable to its actual source
HEAD, build run/job and content hash before launch. Explicit source HEAD and
artifact ID allow a reviewed test-only workflow revision to use the original App.
The explicit workflow-observation stage instead uses its reviewed fixed script
and helper, window ID, approvals and isolated-display flags without legacy args.
The fresh-workbench-observation stage uses its fixed script and metadata helper,
exact candidate, window ID, approvals and private accessibility bus, with no client
or fixture inputs. Settings entry permanently ends pixel capture.
The two A2 observation stages each run one fixed, independently reviewed
controller scope on a fresh App, with finite scope artifacts and a 120s controller.
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
WORKFLOW_HEAD = "11ebf203e1a78d3b6a21677c4b17e96223c74b0e"
WORKFLOW_APP_SHA = "8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30"
WORKFLOW_BUILD_RUN = 37328783317
WORKFLOW_ARTIFACT = "11355501142"
WORKFLOW_UI = ("reviewed-ui-11ebf20-workflow.json", "a3cc14171f1c4a0f62806d1333a2fc19e47707f05ef4a42814b5dc01870dddaa")
WORKFLOW_IDENTITY_SHA = "2b94fc606a46d7d0df43ea2d07a29e5a3b5075253521a201e65d264ee5933574"
WORKFLOW_SCRIPTS = {
    "qa": ("workflow_ui_11ebf20.py", "2b208fe3de01e5bc1b1a65506fe2acf2855cfc764f752dc4b09cff837df5f9d5"),
    "public_ui_probe": ("public_ui_probe.py", "d6e74b62eaaca096ec75f55fd0326942dbece24dfb0e32397062c1248a3c23e2"),
    "workflow_checks": ("workflow_checks.py", "93b6246ec2f0bb450753812bc0eacc0493480ab1bee9acccf88cd19210bfe853"),
    "native_ui_action": ("native_ui_action_11ebf20.py", "5284f9da2cb5cb73d0a4cb042b443d60dc8f558b65460fdfbbc9e6250e484b42"),
}
WORKFLOW_PNG_NAMES = {
    name + ".png" for name in (
        "01-current-initial", "02-after-quick", "03-gallery", "04-create-dialog", "05-editor", "06-open-menu",
        "12-left-saved-editor", "13-new-blank-dialog", "14-new-blank-editor", "15-reopen-menu", "19-export-destination",
        "12-save-gallery", "13-gallery-other-blank", "14-gallery-after-other-document",
        "17-secondary-project-control-hover", "20-export-App-destination-requires-review",
    )
} | {
    phase + suffix + ".png"
    for phase in ("07-import", "10-save", "16-reopen", "20-export")
    for suffix in ("-ambiguous-native", "-native-outside-display", "-native-visible", "-no-visible-native-after-wait")
} | {
    phase + suffix + ".png"
    for phase in ("08-import", "11-save", "17-reopen", "21-export")
    for suffix in ("-returned-App", "-menu-closed", "-native-not-gone", "-native-chrome", "-location-visible")
} | {
    phase + suffix + ".png"
    for phase in ("09-edit", "16-gallery-reopened-edit", "18-reopened-edit") for suffix in ("-dragged", "-one-Undo")
} | {
    phase + suffix + ".png"
    for phase in ("08-import", "15-gallery-reopen", "17-reopen")
    for suffix in ("-fixture-visible-stable", "-fixture-not-confirmed-after-wait")
}
WORKFLOW_JSON_NAMES = {"workflow.json", "public-accessibility.json", "12-save-gallery-public.json"} | {
    phase + suffix + ".json"
    for phase in ("08-import", "11-save", "17-reopen", "21-export")
    for suffix in ("-public-before", "-public-location", "-public-before-accept")
} | {"20-export-public.json"}
WORKFLOW_FILE_LIMIT = 2 * 1024 * 1024
WORKFLOW_PNG_LIMIT = 14 * 1024 * 1024
WORKFLOW_METADATA_LIMIT = 128 * 1024
NEXT_UI_SCRIPT = ("independent-next-ui.py", "0f6bea99d3ff0a8d9c884a697e416409b1dc89692ccf78d3dc118af9ca7af476")
NEXT_UI_PROBE = ("public_probe_cecc8fd_ui2.py", "8db69027991a7fe49beb8f4515926b7fd932436c0034ccffd03d6a543f36954b")
NEXT_UI_HEAD = "cecc8fdf9578675051dae58bda25f0ff805ce235"
NEXT_UI_APP_SHA = "8859b10e7b8a58785d6a60454995f33d56efea554011825287917d06064aff55"
NEXT_UI_BUILD_RUN = 37312315509
NEXT_UI_ARTIFACT = "11345919609"
NEXT_UI_DIRECTORY = "independent-qa-cecc8fd-ui2"
NEXT_UI_PNG_NAMES = {
    "01-before-quick.png", "02-after-quick-1280x900.png",
    "03-workbench-Tab.png", "04-workbench-ShiftTab.png",
    "05-after-canvas-navigation.png", "06-after-clip-navigation.png", "07-after-assets-navigation.png",
}
NEXT_UI_JSON_NAMES = {
    "navigation.json", "03-workbench-tab-public.json", "04-workbench-shifttab-public.json",
    "08-settings-public.json", "09-settings-tab-public.json", "10-settings-shifttab-public.json",
    "11-settings-after-escape-public.json",
}
A2_SCOPES = {
    "settings-controls-observation": "settings-controls",
    "canvas-entry-observation": "canvas-entry",
}
# Final independently delivered byte identities; controller/helpers are not adapters.
A2_HEAD = "11ebf203e1a78d3b6a21677c4b17e96223c74b0e"
A2_APP_SHA = "8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30"
A2_BUILD_RUN = 37328783317
A2_ARTIFACT = "11355501142"
A2_SCRIPTS = {
    "qa": ("next_ui_11ebf20_a3.py", "6a9b01710b0fda386a133cf57c93a650d183378ee366621327fcc1013c464f4f"),
    "a2_probe": ("public_probe_11ebf20_ui4.py", "ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd"),
    "a2_action": ("public_action_11ebf20_ui4.py", "8aafd63bc2653c1099003edbf5808c1c4550471d31f619cf1851a0e7ea063518"),
}
A2_UI = ("reviewed-ui-11ebf20-a3.json", "3770f759f4479904651f3e3404e0ff23b6bea08c3a007320bf2d915ff7c93852")
A2_IDENTITY_SHA = "2b94fc606a46d7d0df43ea2d07a29e5a3b5075253521a201e65d264ee5933574"
A2_CONTROLLER_SECONDS = 120
A2_DIRECTORIES = {stage: "main-qa-11ebf20-a3-" + scope for stage, scope in A2_SCOPES.items()}
A2_PNG_NAMES = {
    "settings-controls": {
        "01-before-quick.png", "02-after-quick-1280x900.png", "07-after-close-workbench-1280x900.png",
        "08-after-close-workbench-1024x900.png", "08-after-close-workbench-1440x900.png",
    },
    "canvas-entry": {
        "01-before-quick.png", "02-after-quick-1280x900.png", "03-canvas-gallery-1280x900.png",
        "04-canvas-gallery-1024x900.png", "04-canvas-gallery-1440x900.png", "05-canvas-gallery-return-1280x900.png",
        "06-current-new-canvas-dialog.png", "07-current-create-result.png",
        "08-current-blank-editor-1024x900.png", "08-current-blank-editor-1440x900.png",
    },
}
A2_PUBLIC_JSON_NAMES = {
    "settings-controls": {"03-settings-expanded-public.json", "04-professional-result-public.json",
                          "05-dark-result-public.json", "06-after-close-public.json"},
    "canvas-entry": {"06-current-new-canvas-dialog-public.json", "07-create-result-public.json"},
}
A2_ACTION_JSON_NAMES = {
    "settings-controls": {"04-professional-action.json", "05-dark-action.json", "06-close-settings-action.json"},
    "canvas-entry": set(),
}
A2_ACTION_LIMIT = 8 * 1024
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
    fresh = next_stage == "fresh-workbench-observation"
    a2_scope = A2_SCOPES.get(next_stage)
    directories = ({A2_DIRECTORIES[next_stage]} if a2_scope else
                   ({NEXT_UI_DIRECTORY} if fresh else ({WORKFLOW_DIRECTORY} if workflow else QA_DIRECTORIES)))
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
                if a2_scope:
                    if directory == output:
                        if entry.name not in RESERVED:
                            raise ValueError("A2 artifacts require this scope's known QA directory")
                    elif entry.name in A2_PNG_NAMES[a2_scope]:
                        png_count += 1
                        png_bytes += info.st_size
                        if png_count > 10 or info.st_size > WORKFLOW_FILE_LIMIT or png_bytes > WORKFLOW_PNG_LIMIT:
                            raise ValueError("A2 PNG count or byte budget exceeded")
                    elif entry.name in A2_ACTION_JSON_NAMES[a2_scope]:
                        if info.st_size > A2_ACTION_LIMIT:
                            raise ValueError("A2 action record exceeds 8 KiB")
                    elif entry.name == "a3.json" or entry.name in A2_PUBLIC_JSON_NAMES[a2_scope]:
                        if info.st_size > WORKFLOW_METADATA_LIMIT:
                            raise ValueError("A2 metadata exceeds 128 KiB")
                    else:
                        raise ValueError("unknown A2 scope artifact file")
                elif workflow:
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
                elif fresh:
                    if directory == output:
                        if entry.name not in RESERVED:
                            raise ValueError("fresh workbench artifacts require the known QA directory")
                    elif entry.name in NEXT_UI_PNG_NAMES:
                        png_count += 1
                        png_bytes += info.st_size
                        if png_count > 7 or info.st_size > WORKFLOW_FILE_LIMIT or png_bytes > WORKFLOW_PNG_LIMIT:
                            raise ValueError("fresh workbench PNG count or byte budget exceeded")
                    elif entry.name in NEXT_UI_JSON_NAMES:
                        if info.st_size > WORKFLOW_METADATA_LIMIT:
                            raise ValueError("fresh workbench metadata exceeds 128 KiB")
                    else:
                        raise ValueError("unknown fresh workbench artifact file")
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


def enable_private_accessibility(env, deadline, result):
    """Prepare only the attested fresh-stage bus before any App process exists."""
    state = {"status": "unavailable", "before": None, "after": None,
             "screen_reader_enabled": None}
    result["accessibility_preparation"] = state
    left = deadline - time.monotonic()
    if left <= 0:
        state["status"] = "deadline"
        raise ValueError("private accessibility preparation deadline")
    program = '''import json
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib
state = {"status": "unavailable", "before": None, "after": None, "screen_reader_enabled": None}
try:
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    def get(name):
        reply = bus.call_sync("org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Get",
            GLib.Variant("(ss)", ("org.a11y.Status", name)), GLib.VariantType.new("(v)"),
            Gio.DBusCallFlags.NONE, 1000, None)
        value = reply.unpack()[0]
        if type(value) is not bool:
            raise ValueError("non_boolean")
        return value
    state["before"] = get("IsEnabled")
    state["screen_reader_enabled"] = get("ScreenReaderEnabled")
    bus.call_sync("org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Set",
        GLib.Variant("(ssv)", ("org.a11y.Status", "IsEnabled", GLib.Variant("b", True))),
        GLib.VariantType.new("()"), Gio.DBusCallFlags.NONE, 1000, None)
    state["after"] = get("IsEnabled")
    state["status"] = "verified" if state["after"] is True else "not_enabled"
except Exception:
    pass
print(json.dumps(state))
'''
    try:
        completed = subprocess.run(["/usr/bin/python3", "-B", "-c", program], env=env,
                                   capture_output=True, timeout=min(5, left), check=False)
    except subprocess.TimeoutExpired:
        state["status"] = "timeout"
        raise ValueError("private accessibility preparation timed out") from None
    except OSError:
        raise ValueError("private accessibility preparation unavailable") from None
    try:
        data = json.loads(completed.stdout) if len(completed.stdout) <= 1024 else None
        if (completed.returncode != 0 or not isinstance(data, dict) or set(data) != set(state)
                or data["status"] not in {"verified", "unavailable", "not_enabled"}
                or any(data[key] is not None and type(data[key]) is not bool
                       for key in ("before", "after", "screen_reader_enabled"))):
            raise ValueError("invalid preparation result")
        state.update(data)
    except (ValueError, TypeError):
        raise ValueError("private accessibility preparation unavailable") from None
    if time.monotonic() >= deadline:
        state["status"] = "deadline"
        raise ValueError("private accessibility preparation deadline")
    if (state["status"] != "verified" or state["after"] is not True
            or type(state["before"]) is not bool or type(state["screen_reader_enabled"]) is not bool):
        raise ValueError("private accessibility preparation not verified")


def qa_command(args, copied, app_pid, window_id, work, output, source_head, qa_deadline):
    command = [sys.executable, str(copied["qa"]), "--app-pid", str(app_pid),
               "--work-dir", str(work), "--output", str(output),
               "--deadline-monotonic", str(qa_deadline)]
    if args.next_stage == "workflow-observation":
        command.extend(("--window-id", str(window_id), "--input-dir", str(copied["input_dir"])))
        if args.private_accessibility_bus:
            command.extend(("--private-accessibility-bus", "--probe-python", "/usr/bin/python3"))
    elif args.next_stage == "fresh-workbench-observation":
        command.extend(("--window-id", str(window_id), "--expected-sha", source_head,
                        "--next-stage", "navigation", "--private-accessibility-bus",
                        "--probe-python", "/usr/bin/python3"))
    elif args.next_stage in A2_SCOPES:
        command[command.index("--deadline-monotonic") + 1] = str(
            min(qa_deadline, time.monotonic() + A2_CONTROLLER_SECONDS + 15))
        command.extend(("--window-id", str(window_id), "--expected-sha", source_head,
                        "--next-stage", A2_SCOPES[args.next_stage], "--private-accessibility-bus",
                        "--probe-python", "/usr/bin/python3"))
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
    parser.add_argument("--next-stage", choices=("observe-only", "navigation", "project-entry", "canvas-create-observation", "canvas-create-entry", "editor-entry-observation", "image-picker-observation", "workflow-observation", "fresh-workbench-observation", *A2_SCOPES), default="navigation")
    parser.add_argument("--isolated-display-capture", action="store_true",
                        help="Explicit isolated-display declaration for reviewed observation stages")
    parser.add_argument("--private-accessibility-bus", action="store_true",
                        help="workflow/fresh/A2 observation only; requires a direct dedicated dbus-run-session parent")
    parser.add_argument("--seconds", type=int, choices=(300, 420), default=300)
    args = parser.parse_args()
    capture_stages = {"image-picker-observation", "workflow-observation", "fresh-workbench-observation", *A2_SCOPES}
    if args.next_stage in capture_stages and not args.isolated_display_capture:
        parser.error(args.next_stage + " requires --isolated-display-capture")
    if args.isolated_display_capture and args.next_stage not in capture_stages:
        parser.error("--isolated-display-capture requires an authorized observation stage")
    if args.private_accessibility_bus and args.next_stage not in {"workflow-observation", "fresh-workbench-observation", *A2_SCOPES}:
        parser.error("--private-accessibility-bus is only valid for workflow/fresh observation")
    if args.input_dir is not None and args.next_stage != "workflow-observation":
        parser.error("--input-dir is only valid for workflow-observation")
    if args.next_stage == "workflow-observation":
        if not args.private_accessibility_bus:
            parser.error("workflow-observation requires --private-accessibility-bus")
        if args.seconds != 300 or args.client_binary is not None:
            parser.error("workflow-observation requires 300 seconds and no MCP client")
        if not all((args.identity_approval, args.ui_approval, args.source_head,
                    args.candidate_manifest, args.candidate_artifact_id, args.input_dir)):
            parser.error("workflow-observation requires exact candidate provenance, declarations and owned fixtures")
        if args.source_head != WORKFLOW_HEAD or args.candidate_artifact_id != WORKFLOW_ARTIFACT:
            parser.error("workflow-observation requires the exact reviewed candidate source and artifact")
    if args.next_stage == "fresh-workbench-observation":
        if not args.private_accessibility_bus:
            parser.error("fresh-workbench-observation requires --private-accessibility-bus")
        if args.seconds != 300 or args.client_binary is not None:
            parser.error("fresh-workbench-observation requires 300 seconds and no MCP client")
        if not all((args.identity_approval, args.ui_approval, args.candidate_manifest)):
            parser.error("fresh-workbench-observation requires candidate provenance and both declarations")
        if args.source_head != NEXT_UI_HEAD or args.candidate_artifact_id != NEXT_UI_ARTIFACT:
            parser.error("fresh-workbench-observation requires the exact reviewed candidate source and artifact")
    if args.next_stage in A2_SCOPES:
        if not args.private_accessibility_bus:
            parser.error("A2 observation requires --private-accessibility-bus")
        if args.seconds != 300 or args.client_binary is not None or args.input_dir is not None:
            parser.error("A2 observation requires 300 seconds, no MCP client and no fixture inputs")
        if not all((args.identity_approval, args.ui_approval, args.source_head, args.candidate_manifest)):
            parser.error("A2 observation requires candidate provenance and both declarations")
        if args.source_head != A2_HEAD or args.candidate_artifact_id != A2_ARTIFACT:
            parser.error("A2 observation requires the exact reviewed candidate source and artifact")
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
        if args.next_stage == "fresh-workbench-observation":
            if (candidate is None or source_head != NEXT_UI_HEAD
                    or args.candidate_artifact_id != NEXT_UI_ARTIFACT
                    or candidate["app_sha256"] != NEXT_UI_APP_SHA
                    or candidate["build_run_id"] != NEXT_UI_BUILD_RUN):
                raise ValueError("fresh workbench candidate differs from its reviewed immutable provenance")
        if args.next_stage == "workflow-observation" or args.next_stage in A2_SCOPES:
            workflow = args.next_stage == "workflow-observation"
            reviewed_head, app_sha, build_run, artifact = (
                (WORKFLOW_HEAD, WORKFLOW_APP_SHA, WORKFLOW_BUILD_RUN, WORKFLOW_ARTIFACT) if workflow else
                (A2_HEAD, A2_APP_SHA, A2_BUILD_RUN, A2_ARTIFACT))
            if (candidate is None or source_head != reviewed_head or args.candidate_artifact_id != artifact
                    or candidate["app_sha256"] != app_sha or candidate["build_run_id"] != build_run):
                raise ValueError("reviewed candidate differs from its immutable provenance")
        sources = {"app": regular_input(args.binary), "qa": regular_input(args.qa_script)}
        if args.next_stage == "workflow-observation":
            if sources["qa"] != root / "scripts" / WORKFLOW_SCRIPTS["qa"][0]:
                raise ValueError("workflow-observation requires the fixed reviewed QA script")
            for name, (filename, _digest) in WORKFLOW_SCRIPTS.items():
                if name != "qa":
                    sources[name] = regular_input(root / "scripts" / filename)
        elif args.next_stage == "fresh-workbench-observation":
            if sources["qa"] != root / "scripts" / NEXT_UI_SCRIPT[0]:
                raise ValueError("fresh-workbench-observation requires the fixed reviewed QA script")
            sources["next_ui_probe"] = regular_input(root / "scripts" / NEXT_UI_PROBE[0])
        elif args.next_stage in A2_SCOPES:
            if sources["qa"] != root / "scripts" / A2_SCRIPTS["qa"][0]:
                raise ValueError("A2 observation requires the fixed reviewed QA script")
            for name, (filename, _digest) in A2_SCRIPTS.items():
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
        elif args.next_stage == "fresh-workbench-observation":
            copied["next_ui_probe"] = work / NEXT_UI_PROBE[0]
        elif args.next_stage in A2_SCOPES:
            for name, (filename, _digest) in A2_SCRIPTS.items():
                if name != "qa":
                    copied[name] = work / filename
        hashes = {name: copy_and_hash(source, copied[name], executable=name in {"app", "client"},
                                      limit=(APP_LIMIT if name == "app" else
                                             (64 * 1024 if args.next_stage == "workflow-observation" or args.next_stage in A2_SCOPES else None)))
                  for name, source in sources.items()}
        if args.next_stage == "workflow-observation":
            for name, (_filename, digest) in WORKFLOW_SCRIPTS.items():
                if hashes[name] != digest:
                    raise ValueError("workflow script differs from its reviewed SHA256")
            copied["input_dir"], fixture_hashes = copy_workflow_fixtures(Path(args.input_dir), work)
            result["input_fixture_sha256"] = fixture_hashes
        elif args.next_stage == "fresh-workbench-observation":
            if hashes["qa"] != NEXT_UI_SCRIPT[1]:
                raise ValueError("fresh workbench script differs from its reviewed SHA256")
            if hashes["next_ui_probe"] != NEXT_UI_PROBE[1]:
                raise ValueError("fresh workbench helper differs from its reviewed SHA256")
        elif args.next_stage in A2_SCOPES:
            for name, (_filename, digest) in A2_SCRIPTS.items():
                if hashes[name] != digest:
                    raise ValueError("A2 script or helper differs from its reviewed SHA256")
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
        if args.next_stage == "workflow-observation" or args.next_stage in A2_SCOPES:
            workflow = args.next_stage == "workflow-observation"
            identity_sha, ui_sha, reviewed_head, app_sha = (
                (WORKFLOW_IDENTITY_SHA, WORKFLOW_UI[1], WORKFLOW_HEAD, WORKFLOW_APP_SHA) if workflow else
                (A2_IDENTITY_SHA, A2_UI[1], A2_HEAD, A2_APP_SHA))
            if hashes.get("identity_approval") != identity_sha:
                raise ValueError("runtime identity differs from its reviewed raw bytes")
            if hashes.get("ui_approval") != ui_sha:
                raise ValueError("UI declaration differs from its reviewed raw bytes")
            identity = json.loads(copied["identity_approval"].read_bytes())
            if (not isinstance(identity, dict) or identity.get("schema") != 2
                    or identity.get("reviewed_by") != "main-reviewer" or identity.get("runtime_head") != reviewed_head
                    or identity.get("runtime_app_sha256") != app_sha or identity.get("change_scope") != "product-candidate"):
                raise ValueError("main runtime identity differs from the reviewed App")
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
        if args.next_stage in A2_SCOPES:
            result["limits"]["controller_seconds"] = A2_CONTROLLER_SECONDS
        if args.next_stage in {"workflow-observation", "fresh-workbench-observation", *A2_SCOPES}:
            phase = "accessibility_preparation"
            enable_private_accessibility(env, qa_deadline, result)
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
