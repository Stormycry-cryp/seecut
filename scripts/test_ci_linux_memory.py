"""Exercise resource guards using fixtures; never run sudo or allocate swap."""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("ci-linux-memory.sh")


class LinuxMemoryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.environment = dict(os.environ, RUNNER_TEMP=str(self.root),
                                GITHUB_ACTIONS="true", RUNNER_OS="Linux")

    def shell(self, body):
        # The default mock is an additional fail-safe against privileged calls.
        prefix = f"source {shlex.quote(str(SCRIPT))}\ninit\nsudo() {{ echo UNEXPECTED_SUDO >&2; return 99; }}\n"
        return subprocess.run(["bash", "-euo", "pipefail", "-c", prefix + body],
                              env=self.environment, capture_output=True, text=True, timeout=10)

    def cgroup(self, path, limit="max", current="0"):
        directory = self.root / "cgroup" / path
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "memory.swap.max").write_text(limit)
        (directory / "memory.swap.current").write_text(current)
        return directory

    def test_ancestor_swap_limit_blocks_build(self):
        self.cgroup("parent/child")
        self.cgroup("parent", str(4 * 1024**3))
        result = self.shell('inspect_cgroup "$RUNNER_TEMP/cgroup" /parent/child true')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Insufficient cgroup swap headroom at /parent", result.stderr)

    def test_existing_usage_counts_against_limit(self):
        self.cgroup("child", str(8 * 1024**3), "4096")
        result = self.shell('inspect_cgroup "$RUNNER_TEMP/cgroup" /child true')
        self.assertNotEqual(result.returncode, 0)

    def test_unlimited_ancestors_allow_build(self):
        self.cgroup("parent/child", str(10 * 1024**3), str(2 * 1024**3))
        self.cgroup("parent")
        result = self.shell('inspect_cgroup "$RUNNER_TEMP/cgroup" /parent/child true')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unknown_limit_blocks_build(self):
        (self.root / "cgroup/child").mkdir(parents=True)
        result = self.shell('inspect_cgroup "$RUNNER_TEMP/cgroup" /child true')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Cannot verify cgroup swap limit", result.stderr)

    def test_local_or_non_linux_host_is_refused_before_privileged_calls(self):
        for changes in ({"GITHUB_ACTIONS": "false"}, {"RUNNER_OS": "macOS"}):
            with self.subTest(changes=changes):
                self.environment.update(GITHUB_ACTIONS="true", RUNNER_OS="Linux")
                self.environment.update(changes)
                result = self.shell("prepare")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("requires a GitHub Actions Linux runner", result.stdout + result.stderr)
                self.assertNotIn("UNEXPECTED_SUDO", result.stdout + result.stderr)

    def test_cleanup_refuses_foreign_path_and_symlink(self):
        directory = self.root / "seecut-engine-diagnostics/memory"
        directory.mkdir(parents=True)
        foreign = self.root / "foreign"
        foreign.write_text("preserve")
        link = self.root / "seecut-linux-swap.link"
        link.symlink_to(foreign)
        for path in (foreign, link):
            with self.subTest(path=path):
                (directory / "swap.path").write_text(str(path))
                result = self.shell("cleanup")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("unexpected swap path", result.stdout + result.stderr)
                self.assertEqual(foreign.read_text(), "preserve")
                self.assertTrue(link.is_symlink())

    def prepare_mocks(self, available=40 * 1024**2, activate=True):
        return f'''
inspect_cgroup() {{ :; }}
cgroup_path() {{ echo /fixture; }}
df() {{ printf 'Filesystem 1024-blocks Used Available Capacity Mounted\\nfixture 999999999 0 {available} 0%% /\\n'; }}
free() {{ :; }}
dd() {{ echo mocked-dd >> "$RUNNER_TEMP/calls"; }}
swapon() {{
  if [[ -f "$RUNNER_TEMP/active" ]]; then
    if [[ "$*" == *SIZE* ]]; then
      printf '%s %s\\n' "$(cat "$RUNNER_TEMP/active")" "$((swap_bytes - 4096))"
    else
      cat "$RUNNER_TEMP/active"
    fi
  fi
  echo /existing-swap
}}
sudo() {{
  printf '%s\\n' "$*" >> "$RUNNER_TEMP/calls"
  case $1 in
    mkswap) : ;;
    swapon) {('printf "%s\\n" "$2" > "$RUNNER_TEMP/active"' if activate else 'return 1')} ;;
    swapoff) rm "$RUNNER_TEMP/active" ;;
    *) return 99 ;;
  esac
}}
'''

    def test_disk_guard_prevents_swap_allocation(self):
        result = self.shell(self.prepare_mocks(available=27 * 1024**2) + "prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Insufficient disk space", result.stdout)
        self.assertFalse((self.root / "calls").exists())

    def test_prepare_and_cleanup_only_touch_created_swap(self):
        result = self.shell(self.prepare_mocks() + "prepare\ncleanup")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = (self.root / "calls").read_text()
        self.assertIn("swapoff " + str(self.root / "seecut-linux-swap."), calls)
        self.assertNotIn("swapoff /existing-swap", calls)
        self.assertFalse((self.root / "seecut-engine-diagnostics/memory/swap.path").exists())
        self.assertEqual(list(self.root.glob("seecut-linux-swap.*")), [])

    def test_activation_failure_cleans_partial_file(self):
        result = self.shell(self.prepare_mocks(activate=False) + "prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(self.root.glob("seecut-linux-swap.*")), [])
        self.assertFalse((self.root / "seecut-engine-diagnostics/memory/swap.path").exists())

    def test_cleanup_recognizes_same_file_through_directory_alias(self):
        alias = self.root / "runner-alias"
        alias.symlink_to(self.root, target_is_directory=True)
        result = self.shell(self.prepare_mocks() + r'''
prepare
swap_file=$(cat "$memory_dir/swap.path")
printf '%s/%s\n' "$RUNNER_TEMP/runner-alias" "${swap_file##*/}" > "$RUNNER_TEMP/active"
cleanup
''')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("swapoff " + str(self.root / "seecut-linux-swap."),
                      (self.root / "calls").read_text())
        self.assertFalse((self.root / "active").exists())

    def test_cleanup_rechecks_swapoff_before_removing_file(self):
        result = self.shell(self.prepare_mocks() + r'''
prepare
sudo() { printf '%s\n' "$*" >> "$RUNNER_TEMP/calls"; }
rm() { echo UNEXPECTED_RM >&2; return 98; }
cleanup
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("still active", result.stdout + result.stderr)
        self.assertNotIn("UNEXPECTED_RM", result.stdout + result.stderr)
        self.assertTrue((self.root / "seecut-engine-diagnostics/memory/swap.path").exists())
        self.assertEqual(len(list(self.root.glob("seecut-linux-swap.*"))), 1)

    def test_unlink_failure_keeps_path_for_retry(self):
        result = self.shell(self.prepare_mocks() + r'''
prepare
rm() {
  local path status=0
  for path in "$@"; do
    [[ $path == -- ]] && continue
    if [[ $path == "$RUNNER_TEMP"/seecut-linux-swap.* ]]; then
      echo "rm: cannot remove '$path': Operation not permitted" >&2
      status=1
    else
      command rm -- "$path"
    fi
  done
  return "$status"
}
cleanup
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Operation not permitted", result.stdout + result.stderr)
        self.assertTrue((self.root / "seecut-engine-diagnostics/memory/swap.path").exists())
        self.assertEqual(len(list(self.root.glob("seecut-linux-swap.*"))), 1)

    def test_cleanup_listing_failure_keeps_file_and_path(self):
        result = self.shell(self.prepare_mocks() + r'''
prepare
swapon() { return 1; }
cleanup
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / "seecut-engine-diagnostics/memory/swap.path").exists())
        self.assertEqual(len(list(self.root.glob("seecut-linux-swap.*"))), 1)

    def test_swapoff_failure_keeps_file_for_retry(self):
        result = self.shell(self.prepare_mocks() + '''
prepare
sudo() { return 1; }
cleanup
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue((self.root / "seecut-engine-diagnostics/memory/swap.path").exists())
        self.assertEqual(len(list(self.root.glob("seecut-linux-swap.*"))), 1)


if __name__ == "__main__":
    unittest.main()
