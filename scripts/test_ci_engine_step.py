"""Exercise the CI supervisor with small subprocesses, without compiling the app."""
import json
import importlib.util
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
import unittest


SCRIPT = Path(__file__).with_name("ci-engine-step.py")
SPEC = importlib.util.spec_from_file_location("ci_engine_step", SCRIPT)
SUPERVISOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUPERVISOR)


class EngineStepTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.output = Path(self.directory.name)

    def command(self, program, timeout=5):
        return [sys.executable, str(SCRIPT), "--phase", "fixture", "--timeout", str(timeout),
                "--kill-after", "0.2", "--heartbeat", "0.1", "--output", str(self.output),
                "--", sys.executable, "-u", "-c", program]

    def record(self):
        return json.loads((self.output / "fixture/result.json").read_text())

    def test_success_retains_both_streams_and_resource_result(self):
        result = subprocess.run(self.command("import sys; print('stdout'); print('stderr', file=sys.stderr)"),
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.record()["exitCode"], 0)
        self.assertGreater(self.record()["maxChildRssKiB"], 0)
        log = (self.output / "fixture/output.log").read_text()
        self.assertIn("stdout", log)
        self.assertIn("stderr", log)
        samples = [json.loads(line) for line in (self.output / "fixture/resources.jsonl").read_text().splitlines()]
        self.assertTrue(samples[-1]["final"])
        self.assertIn("cgroupMemory", samples[-1])

    def test_command_failure_is_not_hidden_by_log_capture(self):
        result = subprocess.run(self.command("print('failure detail'); raise SystemExit(7)"),
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(self.record()["exitCode"], 7)
        self.assertIn("failure detail", (self.output / "fixture/output.log").read_text())

    def test_timeout_kills_a_child_that_ignores_term(self):
        child = "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('ready'); time.sleep(30)"
        program = ("import signal,subprocess,sys,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                   f"child=subprocess.Popen([sys.executable,'-u','-c',{child!r}]); "
                   "print('child_pid='+str(child.pid)); time.sleep(30)")
        result = subprocess.run(self.command(program, timeout=2), capture_output=True,
                                text=True, timeout=10)
        self.assertEqual(result.returncode, 124, result.stderr)
        self.assertEqual(self.record()["terminationReason"], "phase_timeout")
        self.assertLess(self.record()["elapsedSeconds"], 6)
        log = (self.output / "fixture/output.log").read_text()
        self.assertIn("ready", log)
        pid = int(re.search(r"child_pid=(\d+)", log).group(1))
        status = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                                capture_output=True, text=True, timeout=5).stdout.strip()
        try:
            self.assertTrue(not status or status.startswith("Z"), status)
        finally:
            if status and not status.startswith("Z"):
                os.kill(pid, signal.SIGKILL)

    def test_runner_term_retains_cancellation_diagnostics(self):
        process = subprocess.Popen(self.command("import time; print('ready'); time.sleep(30)"),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 5
            while not (self.output / "fixture/output.log").exists():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.02)
            os.kill(process.pid, signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 143, stdout + stderr)
            self.assertEqual(self.record()["terminationReason"], "runner_signal")
            self.assertEqual(self.record()["receivedSignal"], signal.SIGTERM)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()


class CgroupMemoryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.cgroup = self.root / "self-cgroup"
        self.cgroup.write_text("0::/job/step\n")

    def metric(self, group, name, value):
        directory = self.root / "groups" / group
        directory.mkdir(parents=True, exist_ok=True)
        (directory / name).write_text(value)

    def sample(self):
        return SUPERVISOR.sample_cgroup_memory(self.cgroup, self.root / "groups")

    def test_ancestor_oom_events_and_unlimited_limits_are_preserved(self):
        self.metric("job/step", "memory.current", "42\n")
        self.metric("job", "memory.max", "max\n")
        self.metric("job", "memory.events", "low 0\nhigh 2\nmax 4\noom 1\noom_kill 1\n")
        result = self.sample()
        self.assertTrue(result["available"])
        self.assertEqual([row["path"] for row in result["groups"]], ["/job/step", "/job", "/"])
        self.assertEqual(result["groups"][1]["metrics"]["memory.events"]["oom_kill"], 1)
        self.assertEqual(result["groups"][1]["metrics"]["memory.max"], "max")

    def test_missing_and_invalid_metrics_do_not_become_zero(self):
        self.metric("job/step", "memory.current", "invalid")
        result = self.sample()
        self.assertFalse(result["available"])
        self.assertNotIn("memory.current", result["groups"][0]["metrics"])
        self.assertEqual(result["groups"][0]["errors"]["memory.current"], "ValueError")

    def test_v1_and_parent_traversal_are_explicitly_unavailable(self):
        for entry in ("4:memory:/job\n", "0::/job/../foreign\n"):
            with self.subTest(entry=entry):
                self.cgroup.write_text(entry)
                result = self.sample()
                self.assertFalse(result["available"])
                self.assertNotIn("groups", result)


if __name__ == "__main__":
    unittest.main()
