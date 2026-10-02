#!/usr/bin/env python3
"""Visible UI steps chosen independently from the reviewed first-run screenshots.

Select quick mode, observe the actual workbench, hover/click canvas navigation,
and hover settings. No project creation, token grant, file picker, or paid action.
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


BASELINE_HEAD = "48d4f771a2327e6982f6b4d2189c72d8f4cfbe75"
BASELINE_APP_SHA256 = "88a8981f684de6b9a326823bfbb4437cbd7ed80fad215d9cfb513c8a9d9d65f5"


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
                   help="Main-reviewer declaration for an exact newer runtime HEAD with test/runtime-only changes")
    args = p.parse_args()
    start = time.monotonic()
    deadline = min(args.deadline_monotonic, start + 45)
    evidence = None
    report = {
        "phase": "independent-first-run-and-canvas-navigation",
        "candidate_head_from_launcher": args.expected_sha,
        "status": "blocked", "product_verdict": "pending_independent_image_review",
        "pid": args.app_pid, "actions": [], "captures": [],
        "coordinate_basis": "Independent review of initial-1280x900.png for HEAD 48d4f771a2327e6982f6b4d2189c72d8f4cfbe75; no implementation coordinates",
        "coordinate_baseline_head": BASELINE_HEAD,
        "coordinate_baseline_app_sha256": BASELINE_APP_SHA256,
        "runtime_head_verification": "exact HEAD supplied by launcher; separate from observed coordinate baseline",
        "limitations": "Click targets are intended targets until resulting screenshots are independently reviewed.",
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
        path = evidence / name
        command(["import", "-window", str(window), "-strip", str(path)], timeout=8)
        os.chmod(path, 0o600)
        size = path.stat().st_size
        if size > 2 * 1024 * 1024:
            raise Blocked("Capture exceeds 2 MiB; stop before artifact publication.")
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
        # A QA-script integration changes HEAD without establishing a new UI baseline.
        # Require a review declaration naming the actual new HEAD; never substitute the old HEAD.
        if args.expected_sha != BASELINE_HEAD:
            approval_path = args.identity_approval
            if approval_path is None:
                raise Blocked("New runtime HEAD requires an exact main-reviewer test/runtime-only declaration.")
            if (not approval_path.is_absolute() or approval_path.is_symlink()
                    or not approval_path.is_file() or approval_path.stat().st_size > 16384):
                raise Blocked("Identity approval must be an explicit small regular JSON file.")
            try:
                with approval_path.open("rb") as stream:
                    approval_bytes = stream.read(16385)
                if len(approval_bytes) > 16384:
                    raise Blocked("Identity approval exceeds 16 KiB.")
                approval = json.loads(approval_bytes)
            except (ValueError, UnicodeError):
                raise Blocked("Invalid identity approval JSON.") from None
            if (not isinstance(approval, dict) or approval.get("schema") != 1
                    or approval.get("coordinate_baseline_head") != BASELINE_HEAD
                    or approval.get("approved_runtime_head") != args.expected_sha
                    or approval.get("reviewed_change_scope") != "tests-and-runtime-only"
                    or approval.get("reviewed_by") != "main-reviewer"):
                raise Blocked("Identity approval does not cover this exact runtime HEAD and coordinate baseline.")
            report["identity_approval"] = {
                "approved_runtime_head": approval["approved_runtime_head"],
                "coordinate_baseline_head": approval["coordinate_baseline_head"],
                "reviewed_change_scope": approval["reviewed_change_scope"],
                "reviewed_by": approval["reviewed_by"],
                "declaration_sha256": hashlib.sha256(approval_bytes).hexdigest(),
                "provenance": "main-reviewer declaration; no product acceptance asserted",
            }
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
        binary_matches = report["app_binary_sha256"] == BASELINE_APP_SHA256
        report["app_binary_matches_coordinate_baseline"] = binary_matches
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
        if not binary_matches:
            # Do not use old coordinates to click any control on a changed binary.
            pause(0.5)
            capture("00-new-binary-initial-1280x900.png", "New binary initial UI; coordinate reuse requires independent image review")
            report["status"] = "new_binary_observed_review_required"
            report["product_verdict"] = "not_tested"
            raise ReviewRequired("Binary SHA256 changed; initial image captured, no pointer or click actions performed.")
        pointer(800, 650, "neutral area outside first-run dialog")
        pause(0.5)
        capture("01-before-mode-choice.png", "Verify fresh first-run modal before interpreting any later click")
        pointer(558, 500, "quick-mode button from independently reviewed first-run screenshot", click=True)
        pointer(800, 650, "neutral result area")
        pause(0.6)
        capture("02-after-quick-choice.png", "Actual UI after one quick-mode click, selection not inferred")
        pointer(40, 170, "canvas navigation icon independently observed at left")
        pause(0.7)
        capture("03-canvas-hover.png", "Actual tooltip/hover feedback for canvas icon")
        pointer(40, 170, "canvas navigation icon", click=True)
        pointer(800, 650, "neutral content area")
        pause(0.8)
        capture("04-after-canvas-navigation.png", "Actual canvas/project entry after click; no precreated project")
        pointer(40, 805, "bottom settings icon independently observed at left")
        pause(0.7)
        capture("05-settings-hover.png", "Only hover: settings and permission UI not opened")
        report["status"] = "bounded_actions_completed_review_pending"
    except ReviewRequired as exc:
        report["review_reason"] = str(exc)
    except (Blocked, OSError, ValueError) as exc:
        report["blocking_reason"] = str(exc)
    finally:
        report["elapsed_seconds"] = round(time.monotonic() - start, 3)
        report["permissions_granted"] = False
        report["project_created"] = False
        report["paid_action_requested"] = False
        report["cleanup_owner"] = "launcher; no App process is killed by this script"
        if evidence is not None:
            path = evidence / "navigation.json"
            path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
            os.chmod(path, 0o600)
        print(json.dumps(report, ensure_ascii=False))
    if report["status"] == "bounded_actions_completed_review_pending":
        return 0
    return 3 if report["status"] == "new_binary_observed_review_required" else 2


if __name__ == "__main__":
    raise SystemExit(main())
