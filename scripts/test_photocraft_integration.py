"""配置/来源防护回归；mock Git，不下载、不编译、不执行上游代码。"""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import photocraft_source as source


def load_script(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), Path(__file__).parent / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


build = load_script("build-photocraft")
baseline = load_script("photocraft-baseline")


class PhotoCraftIntegrationTests(unittest.TestCase):
    def test_default_build_jobs_is_one(self):
        with patch.object(sys, "argv", ["build-photocraft.py", "--toolchain-bin", "/unused"]):
            self.assertEqual(build.parse_args().jobs, 1)

    def test_preflight_failure_creates_failed_attempt(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(build, "ROOT", Path(temp)):
            directory = Path(temp) / ".toonflow/attempt"
            with self.assertRaisesRegex(ValueError, "preflight"), build.evidence_attempt(directory):
                self.assertEqual(json.loads((directory / "provenance.json").read_text())["status"], "building")
                raise ValueError("synthetic preflight failure")
            provenance = json.loads((directory / "provenance.json").read_text())
            self.assertEqual(provenance["status"], "failed")
            self.assertEqual(provenance["error"]["type"], "ValueError")
            self.assertIn("attemptId", provenance)

    def test_existing_evidence_is_rejected_without_reusing_old_built(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(build, "ROOT", Path(temp)):
            directory = Path(temp) / ".toonflow/attempt"
            directory.mkdir(parents=True)
            old = '{"status":"built","attemptId":"old"}'
            (directory / "provenance.json").write_text(old)
            with self.assertRaises(FileExistsError), build.evidence_attempt(directory):
                self.fail("must not enter an old attempt")
            self.assertEqual((directory / "provenance.json").read_text(), old)

    def test_windows_refuses_before_launch(self):
        with patch.object(build.os, "name", "nt"), patch.object(build.subprocess, "Popen") as launch:
            with self.assertRaisesRegex(ValueError, "Windows"):
                build.run_owned_build(["unused"], cwd=None, env=None, stdout=None, stderr=None, timeout=1)
            launch.assert_not_called()

    @unittest.skipUnless(os.name == "posix", "POSIX session regression")
    def test_real_timeout_terminates_child_and_grandchild(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            script = root / "tree.py"
            script.write_text('''import os, signal, subprocess, sys, time
from pathlib import Path
depth = int(sys.argv[1])
child = None
def stop(*_):
    if child is not None:
        child.wait(timeout=2)
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
if depth < 2:
    child = subprocess.Popen([sys.executable, __file__, str(depth + 1)])
Path(__file__).with_name(f"pid-{depth}").write_text(str(os.getpid()))
time.sleep(30)
''')
            with self.assertRaises(subprocess.TimeoutExpired):
                build.run_owned_build([sys.executable, str(script), "0"], cwd=root, env=os.environ.copy(),
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=0.8)
            pids = [int((root / f"pid-{depth}").read_text()) for depth in range(3)]
            for pid in pids:
                with self.subTest(pid=pid), self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)

    @unittest.skipUnless(os.name == "posix", "POSIX session regression")
    def test_nonzero_build_error_is_preserved(self):
        with self.assertRaises(subprocess.CalledProcessError) as raised:
            build.run_owned_build([sys.executable, "-c", "raise SystemExit(23)"], cwd=None, env=os.environ.copy(),
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2)
        self.assertEqual(raised.exception.returncode, 23)

    @unittest.skipUnless(os.name == "posix", "POSIX session regression")
    def test_success_with_orphan_child_is_failed_and_cleaned(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            command = [sys.executable, "-c", "import os,subprocess,sys; from pathlib import Path; "
                       "Path('group').write_text(str(os.getpid())); "
                       "subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])"]
            with self.assertRaisesRegex(subprocess.SubprocessError, "子进程"):
                build.run_owned_build(command, cwd=root, env=os.environ.copy(), stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL, timeout=2)
            self.assertFalse(build.group_running(int((root / "group").read_text())))

    @unittest.skipUnless(os.name == "posix", "POSIX session regression")
    def test_normal_success_waits_and_returns(self):
        build.run_owned_build([sys.executable, "-c", "pass"], cwd=None, env=os.environ.copy(),
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2)

    @unittest.skipUnless(os.name == "posix", "POSIX session regression")
    def test_cleanup_failure_does_not_replace_original_timeout(self):
        original = subprocess.TimeoutExpired(["synthetic"], 0.1)
        with patch.object(build.subprocess, "Popen") as launch, patch.object(build, "terminate_group", side_effect=OSError("synthetic cleanup failure")):
            launch.return_value.wait.side_effect = original
            with self.assertRaises(subprocess.TimeoutExpired) as raised:
                build.run_owned_build(["synthetic"], cwd=None, env=None, stdout=None, stderr=None, timeout=0.1)
            self.assertIs(raised.exception, original)
            self.assertIn("synthetic cleanup failure", " ".join(original.__notes__))

    def test_fixed_pin_and_full_official_packages(self):
        pin = source.load_pin()
        self.assertEqual(pin["commit"], source.COMMIT)
        command = build.build_command(Path("/rust/cargo"), Path("/source"), Path("/cache"), 2, "dev", None, False, pin)
        self.assertIn("--frozen", command)
        self.assertIn("photocraft/heif,photocraft-cli/heif", command)
        self.assertEqual(command.count("-p"), 2)
        self.assertIn("--bins", command)
        self.assertNotIn("run", command)

    def test_network_requires_explicit_opt_in(self):
        command = build.build_command(Path("/rust/cargo"), Path("/source"), Path("/cache"), 3, "release", "triple", True, source.load_pin())
        self.assertIn("--locked", command)
        self.assertIn("--release", command)
        self.assertEqual(command[-2:], ["--target", "triple"])

    def test_unknown_pin_and_bad_patch_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            pin = source.load_pin()
            for key, value in (("commit", "a" * 40), ("minimumRust", "1.93"), ("repository", "https://other.test/repo.git")):
                altered = {**pin, key: value}
                (root / "upstream.json").write_text(json.dumps(altered))
                with self.assertRaises(ValueError):
                    source.load_pin(root)
            pin["patches"] = [{"path": "patches/theme.patch", "sha256": "0" * 64}]
            (root / "patches").mkdir()
            (root / "patches/theme.patch").write_text("modified")
            (root / "upstream.json").write_text(json.dumps(pin))
            with self.assertRaisesRegex(ValueError, "哈希"):
                source.load_pin(root)

    def test_patch_path_traversal_and_git_rejected(self):
        for value in ("../out", "/absolute", "patches/../out", ".git/config", "patches//a", "patches\\a"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                source.safe_relative(value)

    def test_patch_numstat_paths_rejected_before_apply(self):
        with patch.object(source, "run", return_value=b"1\t0\t../outside\0"):
            with self.assertRaises(ValueError):
                source.patch_paths(Path("/patch"))

    def test_snapshot_detects_nonpatch_edit_delete_and_mode(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            file = root / "plain.rs"
            file.write_text("upstream")
            expected = source.snapshot(root)
            file.write_text("unexpected")
            self.assertNotEqual(expected, source.snapshot(root))
            file.write_text("upstream")
            file.chmod(0o755)
            self.assertNotEqual(expected, source.snapshot(root))
            file.unlink()
            self.assertNotEqual(expected, source.snapshot(root))

    def test_snapshot_rejects_symlink_and_special_file(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "link").symlink_to("/tmp")
            with self.assertRaises(ValueError):
                source.snapshot(root)
            (root / "link").unlink()
            os.mkfifo(root / "fifo")
            with self.assertRaises(ValueError):
                source.snapshot(root)

    def test_wrong_head_origin_unknown_untracked_and_ignored_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            pin = source.load_pin()
            pin["patches"] = []  # 单独验证来源/工作区防护，不读取真实主题补丁。
            valid = [str(root) + "\n", pin["repository"] + "\n", pin["commit"] + "\n", b"", b"", b""]
            for index, value in ((1, "wrong\n"), (2, "b" * 40 + "\n"), (3, b"staged\0"), (4, b"unknown\0"), (5, b"target/\0")):
                replies = valid.copy()
                replies[index] = value
                with self.subTest(index=index), patch.object(source, "run", side_effect=replies), self.assertRaises(ValueError):
                    source.repository_identity(root, pin)

    def test_source_root_symlink_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "linked").symlink_to(root, target_is_directory=True)
            with self.assertRaises(ValueError):
                source.repository_identity(root / "linked", source.load_pin())

    def test_outputs_must_stay_in_managed_cache(self):
        for value in (source.ROOT / "target", source.ROOT / "external/photocraft/target", source.ROOT / ".toonflow", source.ROOT / ".toonflow/../target"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                build.managed_output(value)
        self.assertEqual(build.managed_output(source.ROOT / ".toonflow/photocraft-target"), source.ROOT / ".toonflow/photocraft-target")

    def test_runtime_dump_is_separate_and_validated(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "commands.json"
            path.write_text('[{"id":"file.new"},{"id":"file.new"}]')
            with self.assertRaises(ValueError):
                baseline.compare_dump(path, {"engineLiteralCandidates": []})
            path.write_text('[{"id":"file.new"}]')
            result = baseline.compare_dump(path, {"engineLiteralCandidates": [{"id": "file.save"}]})
            self.assertEqual(result["missingStaticCandidates"], ["file.save"])
            self.assertEqual(result["runtimeFeatureAcceptance"], "not-proven")


if __name__ == "__main__":
    unittest.main()
