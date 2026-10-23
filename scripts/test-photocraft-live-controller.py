#!/usr/bin/env python3
"""Linux-only real-process regressions for the existing disposable probe controller.

No editor, Xvfb, GUI, Cargo or existing user process is launched or targeted.
Every signal target is either the actual new-session guardian passed to the
production cleanup helper, or a retained pidfd for a process this test started.
"""
import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import time
import unittest

sys.dont_write_bytecode = True
CONFIG = {}
CASES = []


def proc_identity(pid):
    data = Path(f'/proc/{pid}/stat').read_text()
    fields = data[data.rfind(')') + 2:].split()
    return {'pid': pid, 'pgid': int(fields[2]), 'sid': int(fields[3]), 'startTicks': int(fields[19])}


def read_event(stream, timeout=3):
    deadline, data = time.monotonic() + timeout, bytearray()
    with selectors.DefaultSelector() as selector:
        selector.register(stream, selectors.EVENT_READ)
        while b'\n' not in data:
            if not selector.select(max(0, deadline - time.monotonic())):
                raise TimeoutError('owned fixture readiness timed out')
            block = os.read(stream.fileno(), 4096 - len(data))
            if not block:
                raise RuntimeError('owned fixture ended before readiness')
            data.extend(block)
            if len(data) >= 4096:
                raise RuntimeError('fixture event budget exceeded')
    return json.loads(data.split(b'\n', 1)[0])


def fixture_worker(mode):
    signal.signal(signal.SIGTERM, signal.SIG_IGN if mode == 'ignore' else signal.SIG_DFL)
    print(json.dumps(proc_identity(os.getpid())), flush=True)
    while True:
        signal.pause()


def fixture_guardian(mode):
    # Equivalent ownership shape to session_guardian: retained SID leader, one
    # ordinary group and additional child groups, then wait for parent cleanup.
    signal.signal(signal.SIGTERM, signal.SIG_IGN if mode == 'ignore' else signal.SIG_DFL)
    children, identities = [], []
    try:
        for group in (None, 0, 0):
            child = subprocess.Popen([sys.executable, '-B', str(Path(__file__).resolve()), '--fixture-worker', mode],
                                     stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                     process_group=group)
            children.append(child)
            identities.append(read_event(child.stdout, timeout=1.5))
            child.stdout.close()
        print(json.dumps({'guardian': proc_identity(os.getpid()), 'workers': identities}), flush=True)
        sys.stdin.buffer.read(1)
    finally:
        # Startup failure/parent EOF: only these exact direct Child handles.
        for child in children:
            if child.poll() is None:
                child.kill()
        for child in children:
            child.wait(timeout=1)


class Owned:
    def __init__(self, identity):
        self.identity = identity
        self.fd = os.pidfd_open(identity['pid'], 0)
        try:
            if proc_identity(identity['pid']) != identity:
                raise RuntimeError('fixture PID identity changed during pidfd capture')
        except BaseException:
            os.close(self.fd)
            raise

    def kill(self):
        try:
            signal.pidfd_send_signal(self.fd, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def close(self):
        os.close(self.fd)


def wait_exact_child(pid, timeout=3):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            found, status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return None
        if found:
            return status
        time.sleep(0.01)
    raise TimeoutError('owned child was not reaped within the fixture deadline')


@unittest.skipUnless(sys.platform == 'linux', 'Linux /proc/session/pidfd regression; no processes started on other systems')
class SessionCleanupTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'):
            raise RuntimeError('Linux pidfd support is required for identity-safe fixture cleanup')
        # Prove kernel support before creating any fixture process.
        probe_fd = os.pidfd_open(os.getpid(), 0)
        os.close(probe_fd)
        cls.libc = ctypes.CDLL(None, use_errno=True)
        original = ctypes.c_int()
        if cls.libc.prctl(37, ctypes.byref(original), 0, 0, 0) != 0:  # PR_GET_CHILD_SUBREAPER
            raise RuntimeError('cannot inspect fixture child-subreaper state')
        cls.original_subreaper = original.value
        if cls.libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
            raise RuntimeError('cannot retain exact orphan fixture children for reaping')
        cls.addClassCleanup(cls.restore_subreaper)
        runner_path = CONFIG['workspace'] / 'scripts/photocraft-live-probe.py'
        spec = importlib.util.spec_from_file_location('photocraft_live_probe_under_test', runner_path)
        cls.runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.runner)

    @classmethod
    def restore_subreaper(cls):
        if cls.libc.prctl(36, cls.original_subreaper, 0, 0, 0) != 0:
            raise RuntimeError('cannot restore this test process child-subreaper state')

    def setUp(self):
        self.owned, self.children, self.guardian = [], [], None
        self.row = {'test': self._testMethodName, 'status': 'failed', 'fixtureCleanup': False}
        CASES.append(self.row)

    def remember(self, identity):
        for owned in self.owned:
            if owned.identity == identity:
                return owned
        owned = Owned(identity)
        self.owned.append(owned)
        return owned

    def spawn(self, role, mode):
        env = os.environ.copy()
        env['SEECUT_LIVE_CONTROLLER_FIXTURE'] = '1'
        env['PYTHONDONTWRITEBYTECODE'] = '1'
        child = subprocess.Popen([sys.executable, '-B', str(Path(__file__).resolve()), '--fixture-' + role, mode],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                 start_new_session=True, env=env)
        self.children.append(child)
        identity = proc_identity(child.pid)
        if role == 'guardian':
            self.guardian, self.guardian_identity = child, identity
        self.remember(identity)
        return child, identity

    def collect_owned_descendants(self):
        # Only direct children of this test or the retained, verified guardian.
        # No all-system enumeration and no signalling a PID taken from ps output.
        if self.guardian is None:
            return
        roots = [os.getpid()]
        try:
            if proc_identity(self.guardian.pid) == self.guardian_identity:
                roots.append(self.guardian.pid)
        except FileNotFoundError:
            pass
        for root in roots:
            try:
                children = Path(f'/proc/{root}/task/{root}/children').read_text().split()
            except FileNotFoundError:
                continue
            if len(children) > 16:
                raise RuntimeError('unexpected fixture descendant budget')
            for child in children:
                try:
                    identity = proc_identity(int(child))
                    if identity['sid'] == self.guardian_identity['sid']:
                        self.remember(identity)
                except (FileNotFoundError, ProcessLookupError):
                    pass

    def tearDown(self):
        try:
            # Close only owned fixture pipes; guardian EOF cleans even a partially
            # started child list. Registered pidfds remain valid across PID reuse.
            for child in self.children:
                if child.stdin:
                    child.stdin.close()
            self.collect_owned_descendants()
            for owned in self.owned:
                owned.kill()
            for child in self.children:
                child.wait(timeout=4)
            # A killed guardian's remaining children now belong to this subreaper.
            self.collect_owned_descendants()
            for owned in self.owned:
                owned.kill()
                wait_exact_child(owned.identity['pid'])
            self.row['fixtureCleanup'] = True
        except BaseException:
            self.row['status'] = 'failed'
            raise
        finally:
            for child in self.children:
                if child.stdout:
                    child.stdout.close()
            for owned in self.owned:
                owned.close()

    def check_mode(self, mode, expected_signal):
        outside, outside_identity = self.spawn('worker', 'ordinary')
        self.assertEqual(read_event(outside.stdout), outside_identity)
        self.guardian, self.guardian_identity = self.spawn('guardian', mode)
        ready = read_event(self.guardian.stdout, timeout=8)
        self.assertEqual(ready['guardian'], self.guardian_identity)
        self.assertEqual(len(ready['workers']), 3)
        for identity in ready['workers']:
            self.remember(identity)
            self.assertEqual(identity['sid'], self.guardian.pid)
        groups = self.runner.session_groups(self.guardian.pid, time.monotonic() + 2)
        self.assertEqual(groups, {self.guardian_identity['pgid'], *(p['pgid'] for p in ready['workers'])})
        self.assertGreaterEqual(len(groups), 3)
        self.assertNotIn(outside_identity['pgid'], groups)
        self.assertNotEqual(outside_identity['sid'], self.guardian.pid)
        started = time.monotonic()
        self.runner.cleanup_session(self.guardian)  # Actual production helper, never mocked.
        elapsed = time.monotonic() - started
        self.assertEqual(self.guardian.returncode, -expected_signal)
        self.assertFalse(self.runner.session_groups(self.guardian.pid, time.monotonic() + 2))
        worker_signals = []
        for identity in ready['workers']:
            status = wait_exact_child(identity['pid'])
            self.assertIsNotNone(status, 'subreaper must retain each exact worker exit status')
            self.assertTrue(os.WIFSIGNALED(status))
            worker_signals.append(os.WTERMSIG(status))
        self.assertEqual(worker_signals, [expected_signal] * 3)
        self.assertIsNone(outside.poll(), 'the separate session started by this test must survive owned-session cleanup')
        self.assertEqual(proc_identity(outside.pid), outside_identity)
        self.row.update({'status': 'passed', 'mode': mode, 'guardian': self.guardian_identity,
                         'workerIdentities': ready['workers'], 'ownedGroups': sorted(groups),
                         'guardianSignal': expected_signal, 'workerSignals': worker_signals,
                         'externalSessionIdentity': outside_identity, 'externalSessionSurvived': True,
                         'cleanupSeconds': round(elapsed, 4)})

    def test_term_cleans_all_owned_process_groups_without_touching_other_session(self):
        self.check_mode('ordinary', signal.SIGTERM)

    def test_ignored_term_escalates_to_kill_without_touching_other_session(self):
        self.check_mode('ignore', signal.SIGKILL)


def main():
    if len(sys.argv) == 3 and sys.argv[1] in ('--fixture-worker', '--fixture-guardian'):
        if sys.platform != 'linux' or os.environ.get('SEECUT_LIVE_CONTROLLER_FIXTURE') != '1' or sys.argv[2] not in ('ordinary', 'ignore'):
            raise RuntimeError('private Linux fixture invocation required')
        return fixture_worker(sys.argv[2]) if sys.argv[1] == '--fixture-worker' else fixture_guardian(sys.argv[2])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--workspace', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'linux':
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SessionCleanupTests)
        unittest.TextTestRunner(verbosity=2).run(suite)
        return 0
    workspace = args.workspace.resolve(strict=True)
    output = args.output
    managed = workspace / '.toonflow'
    if not output.is_absolute() or '..' in output.parts or not output.is_relative_to(managed):
        raise RuntimeError('fixture output must be an absolute path inside this workspace .toonflow')
    if any(p.is_symlink() for p in (output, *output.parents)):
        raise RuntimeError('fixture output must not contain symlinks')
    output.mkdir(mode=0o700)  # Exclusive new run; never overwrite another test's evidence.
    CONFIG['workspace'] = workspace
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(SessionCleanupTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    runner_bytes = (workspace / 'scripts/photocraft-live-probe.py').read_bytes()
    report = {'status': 'passed' if result.wasSuccessful() else 'failed', 'platform': sys.platform,
              'runnerSha256': hashlib.sha256(runner_bytes).hexdigest(), 'testsRun': result.testsRun,
              'failureCount': len(result.failures), 'errorCount': len(result.errors), 'skippedCount': len(result.skipped),
              'cases': CASES, 'editorStarted': False, 'guiStarted': False, 'cargoExecuted': False}
    (output / 'process-lifecycle.json').write_text(json.dumps(report, indent=2) + '\n')
    return 0 if result.wasSuccessful() else 1


if __name__ == '__main__':
    raise SystemExit(main())
