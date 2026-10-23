#!/usr/bin/env python3
"""Bound one CI command, stream its output, and retain resource diagnostics."""
import argparse
import datetime
import json
import os
from pathlib import Path, PurePosixPath
import re
import resource
import selectors
import shutil
import signal
import subprocess
import sys
import time


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def signal_group(pid, sig):
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        pass


def sample_cgroup_memory(cgroup_file=Path("/proc/self/cgroup"), root=Path("/sys/fs/cgroup")):
    """Read v2 memory limits and event counters for this job and its ancestors."""
    try:
        paths = [line.split(":", 2)[2] for line in cgroup_file.read_text().splitlines()
                 if line.startswith("0::")]
        if len(paths) != 1 or not paths[0].startswith("/") or ".." in PurePosixPath(paths[0]).parts:
            return {"available": False, "reason": "no_safe_cgroup_v2_path"}
        path = PurePosixPath(paths[0])
        groups = []
        for _ in range(128):
            group = {"path": str(path), "metrics": {}}
            for metric in ("memory.current", "memory.peak", "memory.max", "memory.high",
                           "memory.events", "memory.events.local", "memory.swap.current", "memory.swap.max"):
                try:
                    raw = (root / str(path).lstrip("/") / metric).read_text().strip()
                    if metric.startswith("memory.events"):
                        fields = [line.split() for line in raw.splitlines()]
                        if not fields or any(len(row) != 2 or not row[1].isdigit() for row in fields):
                            raise ValueError("invalid memory event counters")
                        value = {key: int(number) for key, number in fields}
                    elif raw == "max":
                        value = "max"
                    elif raw.isdigit():
                        value = int(raw)
                    else:
                        raise ValueError("invalid memory metric")
                    group["metrics"][metric] = value
                except FileNotFoundError:
                    continue
                except (OSError, ValueError) as error:
                    group.setdefault("errors", {})[metric] = type(error).__name__
            groups.append(group)
            if path == PurePosixPath("/"):
                return {"available": any(row["metrics"] for row in groups), "groups": groups}
            path = path.parent
        return {"available": False, "reason": "cgroup_depth_limit"}
    except (OSError, ValueError) as error:
        return {"available": False, "reason": type(error).__name__}


def sample_resources(pid, elapsed):
    # Only this command's process group; never collect argv or environment values.
    rows = []
    try:
        result = subprocess.run(
            ["ps", "-eo", "pid=,ppid=,pgid=,pcpu=,rss=,stat=,comm="],
            capture_output=True, text=True, timeout=5, check=True,
        )
        for line in result.stdout.splitlines():
            fields = line.split(None, 6)
            if len(fields) == 7 and fields[2] == str(pid):
                rows.append(dict(pid=int(fields[0]), ppid=int(fields[1]),
                                 cpuPercent=float(fields[3]), rssKiB=int(fields[4]),
                                 state=fields[5], executable=Path(fields[6]).name))
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        rows = [{"samplingError": type(error).__name__}]
    memory = {}
    if Path("/proc/meminfo").is_file():
        for line in Path("/proc/meminfo").read_text().splitlines():
            key, value = line.split(":", 1)
            if key in ("MemTotal", "MemAvailable", "SwapTotal", "SwapFree"):
                memory[key + "KiB"] = int(value.split()[0])
    return dict(atUTC=utc_now(), elapsedSeconds=round(elapsed, 3), processes=rows,
                systemMemory=memory, cgroupMemory=sample_cgroup_memory(), loadAverage=os.getloadavg(),
                diskFreeBytes=shutil.disk_usage(Path.cwd()).free)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--phase", required=True)
    parser.add_argument("--timeout", type=float, required=True)
    parser.add_argument("--kill-after", type=float, default=15)
    parser.add_argument("--heartbeat", type=float, default=60)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if (not re.fullmatch(r"[a-z0-9-]+", args.phase) or not command
            or min(args.timeout, args.kill_after, args.heartbeat) <= 0):
        parser.error("require a safe phase name, command, and positive durations")
    output = args.output / args.phase
    output.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    record = dict(phase=args.phase, startedAtUTC=utc_now(), command=command,
                  timeoutSeconds=args.timeout, killAfterSeconds=args.kill_after,
                  cargoEnvironment={key: os.environ.get(key) for key in (
                      "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG", "CARGO_BUILD_JOBS")})
    (output / "started.json").write_text(json.dumps(record, indent=2) + "\n")
    process = None
    selector = selectors.DefaultSelector()
    received_signal = None
    override = None
    kill_at = None
    exited_at = None
    next_sample = started
    exit_code = 1

    def receive_signal(sig, _frame):
        nonlocal received_signal
        received_signal = sig

    old_handlers = {sig: signal.signal(sig, receive_signal)
                    for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        with (output / "output.log").open("wb") as log, (output / "resources.jsonl").open("w") as samples:
            process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                       start_new_session=True)
            selector.register(process.stdout, selectors.EVENT_READ)
            while process.poll() is None or selector.get_map():
                now = time.monotonic()
                reason = None
                if received_signal is not None:
                    reason, code = "runner_signal", 128 + received_signal
                elif now - started >= args.timeout:
                    reason, code = "phase_timeout", 124
                elif exited_at is not None and now - exited_at >= 5:
                    reason, code = "descendant_kept_output_open", 1
                if reason and override is None:
                    override = code
                    record["terminationReason"] = reason
                    record["receivedSignal"] = received_signal
                    signal_group(process.pid, signal.SIGTERM)
                    kill_at = now + args.kill_after
                if kill_at is not None and now >= kill_at:
                    signal_group(process.pid, signal.SIGKILL)
                    kill_at = None
                if now >= next_sample:
                    sample = sample_resources(process.pid, now - started)
                    samples.write(json.dumps(sample) + "\n")
                    samples.flush()
                    print(f"[ci:{args.phase}] {json.dumps(sample)}", flush=True)
                    next_sample = now + args.heartbeat
                for key, _ in selector.select(timeout=0.2):
                    data = os.read(key.fileobj.fileno(), 65536)
                    if data:
                        log.write(data)
                        log.flush()
                        sys.stdout.buffer.write(data)
                        sys.stdout.buffer.flush()
                    else:
                        selector.unregister(key.fileobj)
                if process.poll() is not None and exited_at is None:
                    exited_at = time.monotonic()
            result = process.wait()
            sample = sample_resources(process.pid, time.monotonic() - started)
            sample["final"] = True
            samples.write(json.dumps(sample) + "\n")
            samples.flush()
            print(f"[ci:{args.phase}] {json.dumps(sample)}", flush=True)
            exit_code = override if override is not None else (result if result >= 0 else 128 - result)
    finally:
        if process is not None:
            signal_group(process.pid, signal.SIGKILL)
            process.wait()
            process.stdout.close()
        selector.close()
        for sig, handler in old_handlers.items():
            signal.signal(sig, handler)
        usage = resource.getrusage(resource.RUSAGE_CHILDREN)
        record.update(finishedAtUTC=utc_now(), elapsedSeconds=round(time.monotonic() - started, 3),
                      exitCode=exit_code, userSeconds=usage.ru_utime, systemSeconds=usage.ru_stime,
                      maxChildRssKiB=usage.ru_maxrss / (1024 if sys.platform == "darwin" else 1))
        (output / "result.json").write_text(json.dumps(record, indent=2) + "\n")
        print(f"[ci:{args.phase}] result: {json.dumps(record)}", flush=True)
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
