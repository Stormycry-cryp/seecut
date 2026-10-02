#!/usr/bin/env python3
"""Bounded visible-UI observation; never starts an App or grants permission."""
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


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--app-pid", type=int, required=True)
    p.add_argument("--client-binary", type=Path, required=True)
    p.add_argument("--work-dir", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--expected-sha", required=True, help="40-character candidate Git HEAD supplied by launcher")
    p.add_argument("--deadline-monotonic", type=float, required=True)
    p.add_argument("--window-id", type=int, help="Optional explicit window; must belong to app-pid")
    args = p.parse_args()
    started = time.monotonic()
    deadline = min(args.deadline_monotonic, started + 45)
    result = {
        "phase": "independent-visible-ui-observation",
        "status": "blocked",
        "product_cases_executed": [],
        "product_verdict": "not_tested",
        "candidate_head_from_launcher": args.expected_sha,
        "head_provenance": "launcher attestation, not inferred from package version",
        "pid": args.app_pid,
        "captures": [],
        "started_monotonic": started,
        "deadline_monotonic": deadline,
        "sensitivity": "private review required before publishing any raw screenshot",
    }
    run_dir = None

    def run(argv, *, max_seconds=5):
        left = deadline - time.monotonic()
        if left < 0.2:
            raise Blocked("Observation deadline reached; no further UI actions.")
        try:
            return subprocess.run(argv, capture_output=True, text=True, check=True,
                                  timeout=min(max_seconds, left))
        except subprocess.TimeoutExpired:
            raise Blocked("An observation command timed out; no automatic retry.") from None
        except subprocess.CalledProcessError as exc:
            # Do not include arbitrary stderr: neither credentials nor App logs belong here.
            raise Blocked(f"Observation command {Path(argv[0]).name} failed with exit {exc.returncode}.") from None

    try:
        if not re.fullmatch(r"[0-9a-f]{40}", args.expected_sha):
            raise Blocked("expected-sha must be an exact lowercase 40-character Git HEAD.")
        if not (deadline > started and args.app_pid > 1):
            raise Blocked("Invalid PID or expired monotonic deadline.")
        if not os.environ.get("DISPLAY"):
            raise Blocked("DISPLAY is absent; a visible X11 QA display is required.")
        for directory in (args.work_dir, args.output):
            if not directory.is_absolute() or directory.is_symlink() or not directory.is_dir():
                raise Blocked("work-dir/output must be explicit existing directories, not symlinks.")
            if directory.stat().st_uid != os.getuid():
                raise Blocked("work-dir/output must belong to the current QA OS user.")
        new_run_dir = args.output / "independent-qa-observation"
        new_run_dir.mkdir(mode=0o700)  # Refuse to overwrite evidence from an earlier run.
        run_dir = new_run_dir
        for executable in ("xdotool", "import"):
            if shutil.which(executable) is None:
                raise Blocked(f"Required observation executable is unavailable: {executable}.")
        proc = Path("/proc") / str(args.app_pid)
        if proc.stat().st_uid != os.getuid():
            raise Blocked("App must be owned by the same OS user as this script.")
        # Process identity only; do not read App environment, portable data, or project state.
        binary = (proc / "exe").resolve(strict=True)
        result["app_binary"] = str(binary)
        digest = hashlib.sha256()
        with binary.open("rb") as f:
            while True:
                if time.monotonic() >= deadline:
                    raise Blocked("Deadline reached while measuring binary identity.")
                block = f.read(1024 * 1024)
                if not block:
                    break
                digest.update(block)
        result["app_binary_sha256"] = digest.hexdigest()
        result["client_binary_available"] = args.client_binary.is_file() and os.access(args.client_binary, os.X_OK)
        result["client_started"] = False
        result["display"] = os.environ["DISPLAY"]
        result["tmpdir"] = os.environ.get("TMPDIR", "")
        if args.window_id is None:
            found = run(["xdotool", "search", "--onlyvisible", "--pid", str(args.app_pid)]).stdout.split()
            ids = sorted(set(int(x) for x in found))
            if len(ids) != 1:
                raise Blocked("Cannot uniquely identify one visible App window; provide an explicit verified window-id.")
            window = ids[0]
        else:
            window = args.window_id
        if int(run(["xdotool", "getwindowpid", str(window)]).stdout.strip()) != args.app_pid:
            raise Blocked("Selected window PID does not match this QA App.")
        result["window_id"] = window
        result["geometry_before"] = run(["xdotool", "getwindowgeometry", "--shell", str(window)]).stdout
        for width in (1280, 1024, 1440):
            run(["xdotool", "windowsize", "--sync", str(window), str(width), "900"])
            # Settling is capture preparation, not evidence that loading has completed.
            if deadline - time.monotonic() < 1:
                raise Blocked("Insufficient observation time for another capture.")
            time.sleep(0.35)
            name = f"initial-{width}x900.png"
            path = run_dir / name
            run(["import", "-window", str(window), "-strip", str(path)], max_seconds=8)
            os.chmod(path, 0o600)
            size = path.stat().st_size
            if size > 4 * 1024 * 1024:
                raise Blocked("Screenshot exceeded 4 MiB; stop before publishing artifacts.")
            result["captures"].append({
                "file": name, "bytes": size,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "geometry": run(["xdotool", "getwindowgeometry", "--shell", str(window)]).stdout,
                "captured_monotonic": time.monotonic(),
                "meaning": "raw current UI only; no layout/navigation/permission verdict",
            })
        result["status"] = "observation_captured_review_pending"
    except (Blocked, OSError, ValueError) as exc:
        result["blocking_reason"] = str(exc)
    finally:
        result["elapsed_seconds"] = round(time.monotonic() - started, 3)
        result["cleanup"] = {
            "app_termination": "launcher owns App cleanup; this script never kills it",
            "permission_revocation": "not needed: this phase never grants permissions",
            "project_changes": "none: window resizing and screenshots only",
        }
        if run_dir is not None and run_dir.is_dir():
            report = run_dir / "observation.json"
            report.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
            os.chmod(report, 0o600)
        # Metadata only. Raw screenshots require private review, never an automatic upload.
        print(json.dumps(result, ensure_ascii=False))
    return 0 if result["status"] == "observation_captured_review_pending" else 2


if __name__ == "__main__":
    raise SystemExit(main())
