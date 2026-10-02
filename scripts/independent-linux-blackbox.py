#!/usr/bin/env python3
"""Visible UI steps chosen independently from the reviewed first-run screenshots.

Select quick mode, observe the actual workbench, hover/click canvas navigation,
and hover settings. Optional project-entry observes the new-project card and Escape.
No guessed form confirmation, token grant, file picker, or paid action.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time


class Blocked(Exception):
    pass


class ReviewRequired(Exception):
    pass


def declaration(path, label):
    if (path is None or not path.is_absolute() or path.is_symlink()
            or not path.is_file() or path.stat().st_size > 16384):
        raise Blocked(label + " must be an explicit regular JSON file <=16 KiB.")
    with path.open("rb") as stream:
        raw = stream.read(16385)
    if len(raw) > 16384:
        raise Blocked(label + " exceeds 16 KiB.")
    try:
        value = json.loads(raw)
    except (ValueError, UnicodeError):
        raise Blocked(label + " has invalid JSON.") from None
    if not isinstance(value, dict):
        raise Blocked(label + " must contain an object.")
    return value, hashlib.sha256(raw).hexdigest()


def exact_hex(value, length):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{" + str(length) + r"}", value) is not None


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--app-pid", type=int, required=True)
    p.add_argument("--client-binary", type=Path, required=True)
    p.add_argument("--work-dir", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--expected-sha", required=True)
    p.add_argument("--deadline-monotonic", type=float, required=True)
    p.add_argument("--window-id", type=int)
    p.add_argument("--identity-approval", type=Path,
                   required=True, help="Schema 2 main-reviewed exact runtime HEAD and App SHA256")
    p.add_argument("--ui-approval", type=Path,
                   help="Independent-QA reviewed UI actions and coordinates bound to observed App SHA256")
    p.add_argument("--next-stage", choices=("observe-only", "navigation", "project-entry", "canvas-create-observation"), default="navigation",
                   help="project-entry additionally clicks the independently observed new-project card and observes Escape")
    args = p.parse_args()
    start = time.monotonic()
    deadline = min(args.deadline_monotonic, start + 45)
    evidence = None
    report = {
        "phase": "independent-first-run-and-canvas-navigation",
        "candidate_head_from_launcher": args.expected_sha,
        "status": "blocked", "product_verdict": "pending_independent_image_review",
        "pid": args.app_pid, "actions": [], "captures": [],
        "coordinate_basis": "Only a separately supplied independent-QA UI declaration can authorize coordinates",
        "runtime_head_verification": "exact HEAD supplied by launcher; separate from observed coordinate baseline",
        "limitations": "Click targets are intended targets until resulting screenshots are independently reviewed.",
        "requested_stage": args.next_stage,
    }

    def command(argv, timeout=6):
        left = deadline - time.monotonic()
        if left < 0.2:
            raise Blocked("Navigation deadline reached; no further input.")
        try:
            return subprocess.run(argv, check=True, text=True, capture_output=True,
                                  timeout=min(timeout, left)).stdout
        except subprocess.TimeoutExpired:
            raise Blocked("UI command timeout; no automatic retry.") from None
        except subprocess.CalledProcessError as exc:
            raise Blocked(f"UI command {Path(argv[0]).name} exit={exc.returncode}; raw stderr withheld.") from None

    def pause(seconds):
        if deadline - time.monotonic() < seconds + 0.2:
            raise Blocked("Insufficient time for next capture; stop input.")
        time.sleep(seconds)

    def capture(name, meaning):
        # Replay already reviewed navigation without duplicating its five snapshots.
        # Keep the actual gallery plus five new observations within six PNGs total.
        if args.next_stage == "canvas-create-observation" and name in {
            "01-before-mode-choice.png", "02-after-quick-choice.png", "03-canvas-hover.png", "05-settings-hover.png"
        }:
            return
        path = evidence / name
        command(["import", "-window", str(window), "-strip", str(path)], timeout=8)
        os.chmod(path, 0o600)
        size = path.stat().st_size
        if size > 2 * 1024 * 1024:
            raise Blocked("Capture exceeds 2 MiB; stop before artifact publication.")
        if sum(item["bytes"] for item in report["captures"]) + size > 14 * 1024 * 1024:
            raise Blocked("Aggregate screenshots exceeded 14 MiB; stop before artifact publication.")
        report["captures"].append({"file": name, "bytes": size,
                                   "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                                   "meaning": meaning})

    def pointer(x, y, intent, click=False):
        command(["xdotool", "mousemove", "--window", str(window), str(x), str(y)])
        if click:
            command(["xdotool", "click", "1"])
        report["actions"].append({"kind": "click" if click else "hover", "x": x, "y": y,
                                   "intent": intent, "monotonic": time.monotonic()})

    try:
        if not re.fullmatch(r"[0-9a-f]{40}", args.expected_sha):
            raise Blocked("Invalid candidate HEAD attestation.")
        # Runtime identity establishes which candidate may be observed, never which UI may be clicked.
        runtime, runtime_digest = declaration(args.identity_approval, "Runtime declaration")
        if (runtime.get("schema") != 2 or runtime.get("runtime_head") != args.expected_sha
                or "runtime_app_sha256" not in runtime
                or not (runtime["runtime_app_sha256"] is None or exact_hex(runtime["runtime_app_sha256"], 64))
                or runtime.get("reviewed_by") != "main-reviewer"
                or runtime.get("change_scope") not in ("tests-and-runtime-only", "product-candidate")):
            raise Blocked("Schema 2 runtime declaration must match this exact HEAD and reviewed binary identity.")
        report["identity_approval"] = dict(runtime, declaration_sha256=runtime_digest)
        ui = None
        if args.ui_approval is not None:
            ui, ui_digest = declaration(args.ui_approval, "Independent UI declaration")
            if (ui.get("schema") != 1 or ui.get("reviewed_by") != "independent-qa"
                    or not exact_hex(ui.get("observed_head"), 40)
                    or not exact_hex(ui.get("observed_app_sha256"), 64)
                    or not isinstance(ui.get("evidence_id"), str)
                    or ui.get("window") != {"width": 1280, "height": 900}
                    or not isinstance(ui.get("approved_actions"), list)
                    or any(not isinstance(action, str) for action in ui["approved_actions"])
                    or not isinstance(ui.get("coordinates"), dict)):
                raise Blocked("Independent UI declaration is invalid; no UI permission inferred.")
            required = {"quick-mode", "canvas-navigation", "settings-hover"}
            if args.next_stage in ("project-entry", "canvas-create-observation"):
                required.add("project-entry-escape")
            if args.next_stage == "canvas-create-observation":
                required.add("canvas-create-observation")
            report["ui_approval"] = dict(ui, declaration_sha256=ui_digest)
            report["coordinate_baseline_head"] = ui["observed_head"]
            report["coordinate_baseline_app_sha256"] = ui["observed_app_sha256"]
            report["coordinate_basis"] = ui["evidence_id"]
            coordinate_keys = ["quick_mode", "canvas_navigation", "settings_hover", "new_project"]
            if args.next_stage == "canvas-create-observation":
                coordinate_keys.extend(["new_dialog_header", "new_cancel", "new_create"])
            for key in coordinate_keys:
                xy = ui["coordinates"].get(key)
                if (not isinstance(xy, list) or len(xy) != 2
                        or any(type(v) is not int for v in xy)
                        or not (0 <= xy[0] < 1280 and 32 <= xy[1] < 900)):
                    raise Blocked("UI declaration contains an invalid visible-window coordinate.")
            actions_covered = required.issubset(set(ui["approved_actions"]))
        else:
            actions_covered = False
        if args.app_pid < 2 or not deadline > start or not os.environ.get("DISPLAY"):
            raise Blocked("Missing live QA PID/display or expired deadline.")
        for directory in (args.work_dir, args.output):
            if not directory.is_absolute() or directory.is_symlink() or not directory.is_dir():
                raise Blocked("Only explicit existing isolated directories are accepted.")
            if directory.stat().st_uid != os.getuid():
                raise Blocked("QA directories must belong to the current OS user.")
        candidate_dir = args.output / "independent-qa-navigation"
        candidate_dir.mkdir(mode=0o700)
        evidence = candidate_dir
        for name in ("xdotool", "import"):
            if not shutil.which(name):
                raise Blocked("Missing visible UI dependency: " + name)
        proc = Path("/proc") / str(args.app_pid)
        if proc.stat().st_uid != os.getuid():
            raise Blocked("App belongs to another OS user.")
        binary = (proc / "exe").resolve(strict=True)
        digest = hashlib.sha256()
        with binary.open("rb") as stream:
            while True:
                if time.monotonic() >= deadline:
                    raise Blocked("Deadline reached during binary verification.")
                data = stream.read(1024 * 1024)
                if not data:
                    break
                digest.update(data)
        report["app_binary_sha256"] = digest.hexdigest()
        runtime_binary_attested = runtime["runtime_app_sha256"] is not None
        report["runtime_binary_mapping_attested"] = runtime_binary_attested
        if runtime_binary_attested and report["app_binary_sha256"] != runtime["runtime_app_sha256"]:
            raise Blocked("Actual App SHA256 does not match the main-reviewed runtime candidate; no UI input.")
        binary_matches = ui is not None and report["app_binary_sha256"] == ui["observed_app_sha256"]
        report["app_binary_matches_coordinate_baseline"] = binary_matches
        product_head_reviewed = (runtime["change_scope"] == "tests-and-runtime-only"
                                 or (ui is not None and ui["observed_head"] == args.expected_sha))
        may_navigate = runtime_binary_attested and binary_matches and actions_covered and product_head_reviewed and args.next_stage != "observe-only"
        report["navigation_authorized_by_independent_review"] = may_navigate
        report["client_binary_available"] = args.client_binary.is_file() and os.access(args.client_binary, os.X_OK)
        report["client_started"] = False
        if args.window_id is None:
            found = command(["xdotool", "search", "--onlyvisible", "--pid", str(args.app_pid)]).split()
            ids = sorted(set(int(x) for x in found))
            if len(ids) != 1:
                raise Blocked("One visible QA App window is required.")
            window = ids[0]
        else:
            window = args.window_id
        if int(command(["xdotool", "getwindowpid", str(window)]).strip()) != args.app_pid:
            raise Blocked("Window PID does not match the supplied QA App.")
        report["window_id"] = window
        command(["xdotool", "windowsize", "--sync", str(window), "1280", "900"])
        command(["xdotool", "windowfocus", "--sync", str(window)])
        geometry = command(["xdotool", "getwindowgeometry", "--shell", str(window)])
        report["geometry"] = geometry
        fields = dict(line.split("=", 1) for line in geometry.splitlines() if "=" in line)
        if fields.get("WIDTH") != "1280" or fields.get("HEIGHT") != "900":
            raise Blocked("Actual window geometry is not the independently reviewed 1280x900.")
        if not may_navigate:
            # No pointer, click or key actions without matching independent UI review.
            for width in (1280, 1024, 1440):
                command(["xdotool", "windowsize", "--sync", str(window), str(width), "900"])
                pause(0.4)
                capture(f"00-observe-only-{width}x900.png", "Exact runtime candidate raw initial UI; no navigation attempted")
            report["status"] = "candidate_observed_review_required"
            report["product_verdict"] = "not_tested"
            raise ReviewRequired("Candidate observed only; missing/mismatched independent UI review or explicit observe-only stage. No automatic navigation retry.")
        coords = ui["coordinates"]
        pointer(800, 650, "neutral area outside first-run dialog")
        pause(0.5)
        capture("01-before-mode-choice.png", "Verify fresh first-run modal before interpreting any later click")
        pointer(*coords["quick_mode"], "quick-mode button from independently reviewed first-run screenshot", click=True)
        pointer(800, 650, "neutral result area")
        pause(0.6)
        capture("02-after-quick-choice.png", "Actual UI after one quick-mode click, selection not inferred")
        pointer(*coords["canvas_navigation"], "canvas navigation icon independently observed at left")
        pause(0.7)
        capture("03-canvas-hover.png", "Actual tooltip/hover feedback for canvas icon")
        pointer(*coords["canvas_navigation"], "canvas navigation icon", click=True)
        pointer(800, 650, "neutral content area")
        pause(0.8)
        capture("04-after-canvas-navigation.png", "Actual canvas/project entry after click; no precreated project")
        pointer(*coords["settings_hover"], "bottom settings icon independently observed at left")
        pause(0.7)
        capture("05-settings-hover.png", "Only hover: settings and permission UI not opened")
        if args.next_stage in ("project-entry", "canvas-create-observation"):
            report["project_creation_requested"] = True
            pointer(*coords["new_project"], "new-project card plus independently observed in the empty canvas gallery", click=True)
            pointer(800, 650, "neutral content area")
            pause(0.8)
            capture("06-after-new-project-click.png", "Actual new-project dialog or editor; no form values inferred or submitted")
            if args.next_stage == "canvas-create-observation":
                pointer(*coords["new_dialog_header"], "visible new-canvas dialog header to establish App input focus", click=True)
                focused = int(command(["xdotool", "getwindowfocus"]).strip())
                focused_pid = int(command(["xdotool", "getwindowpid", str(focused)]).strip())
                report["keyboard_target_before_escape"] = {"window_id": focused, "pid": focused_pid}
                if focused_pid != args.app_pid:
                    raise Blocked("Keyboard focus is not owned by this App; no Escape or create sequence.")
            command(["xdotool", "key", "--clearmodifiers", "Escape"])
            report["actions"].append({"kind": "key", "key": "Escape", "intent": "Observe top-level temporary UI cancellation without guessing a form", "monotonic": time.monotonic()})
            pause(0.6)
            capture("07-after-project-escape.png", "Actual Escape result; project creation/cancellation determined only from screenshots")
            if args.next_stage == "canvas-create-observation":
                pointer(*coords["new_cancel"], "visible Cancel on observed new-canvas dialog; if Escape closed it this is a neutral content click", click=True)
                pointer(800, 650, "neutral content area")
                pause(0.6)
                capture("08-after-visible-cancel.png", "Actual cancel result; no project creation inferred")
                pointer(*coords["new_project"], "observed new-project card to reopen the dialog", click=True)
                pointer(800, 650, "neutral content area")
                pause(0.6)
                capture("09-before-visible-create.png", "Actual reopened dialog and its displayed values before one Create click")
                report["visible_create_requested"] = True
                pointer(*coords["new_create"], "visible Create button on the independently observed new-canvas dialog, using displayed defaults", click=True)
                pointer(800, 650, "neutral area; no painting gesture")
                pause(0.8)
                capture("10-after-visible-create.png", "Actual editor/result after one Create; persistence, content and success require independent review")
        report["status"] = "bounded_actions_completed_review_pending"
    except ReviewRequired as exc:
        report["review_reason"] = str(exc)
    except (Blocked, OSError, ValueError) as exc:
        report["blocking_reason"] = str(exc)
    finally:
        report["elapsed_seconds"] = round(time.monotonic() - start, 3)
        report["permissions_granted"] = False
        report["project_created"] = "pending_independent_image_review" if report.get("project_creation_requested") else False
        report["paid_action_requested"] = False
        report["cleanup_owner"] = "launcher; no App process is killed by this script"
        if evidence is not None:
            path = evidence / "navigation.json"
            path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
            os.chmod(path, 0o600)
        print(json.dumps(report, ensure_ascii=False))
    if report["status"] == "bounded_actions_completed_review_pending":
        return 0
    return 3 if report["status"] == "candidate_observed_review_required" else 2


if __name__ == "__main__":
    raise SystemExit(main())
