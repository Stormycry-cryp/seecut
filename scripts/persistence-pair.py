#!/usr/bin/env python3
"""Run exactly two finite launchers, serially, inside one dedicated Xvfb CI job.

This outer coordinator is the sole owner of the random persistent state root.
Each launcher still owns exactly one App lifetime and its separate scratch work.
No second App is started by a QA controller; a failed A never starts B.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import secrets
import shutil
import stat
import subprocess
import sys

LIMIT = 15 * 1024 * 1024
SOURCE_HEAD = 'a6cf09ad935bbbc6cf792c4ac6a4b506a47aebd3'
ARTIFACT = '11359607955'


def evidence_bytes(root):
    total = count = 0
    for directory, dirs, files in os.walk(root, followlinks=False):
        for name in dirs:
            if not stat.S_ISDIR((Path(directory) / name).lstat().st_mode):
                raise ValueError('pair_evidence_non_regular_directory')
        for name in files:
            info = (Path(directory) / name).lstat()
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.getuid():
                raise ValueError('pair_evidence_non_owned_regular_file')
            count += 1
            total += info.st_size
            if count > 200 or total > LIMIT - 65536:
                raise ValueError('pair_evidence_budget_exceeded')
    return total


def run_pair(args, runner=subprocess.run):
    helpers = runpy.run_path(str(Path(__file__).with_name('persistence-state.py')))
    output = args.output
    helpers['real_directory'](output.parent)
    if not output.is_absolute() or '..' in output.parts:
        raise ValueError('pair_absolute_fresh_output_required')
    output.mkdir(mode=0o700)
    batch = secrets.token_hex(16)
    state = output.parent / ('seecut-persistence-' + batch)
    report = {'schema': 1, 'status': 'blocked', 'batch': batch, 'state_root': str(state),
              'same_original_path': True, 'state_values_rewritten': False,
              'phases': [], 'state_removed': False, 'product_verdict': 'pending_main_actual_evidence_review'}
    launch = Path(__file__).with_name('run-linux-blackbox.py')
    try:
        for phase in ('seed', 'reopen'):
            controller = Path(__file__).with_name('persistence-' + phase + '-controller.py')
            command = ['timeout', '--signal=TERM', '--kill-after=10s', '290s', 'dbus-run-session', '--',
                       sys.executable, '-B', str(launch), '--binary', str(args.binary),
                       '--qa-script', str(controller), '--output', str(output / phase),
                       '--expected-sha', args.expected_sha, '--seconds', '300',
                       '--next-stage', 'persistence-' + phase + '-observation',
                       '--source-head', SOURCE_HEAD, '--candidate-manifest', str(args.candidate_manifest),
                       '--candidate-artifact-id', ARTIFACT,
                       '--identity-approval', str(args.identity_approval), '--ui-approval', str(args.ui_approval),
                       '--input-dir', str(args.input_dir), '--isolated-display-capture', '--private-accessibility-bus',
                       '--owned-state-root', str(state), '--state-token', batch]
            if phase == 'reopen':
                command.extend(('--seed-record', str(output / 'seed' / 'independent-qa-workflow' / 'persistence-seed.json')))
            completed = runner(command, capture_output=True, timeout=305)
            # No raw stdout/stderr from subprocesses enters the pair artifact or console.
            item = {'phase': phase, 'launcher_exit_code': completed.returncode}
            report['phases'].append(item)
            harness = json.loads(helpers['read_owned'](output / phase / 'harness.json', 32768))
            item['harness_status'] = harness.get('status')
            item['scratch_removed'] = harness.get('owned_work_removed') is True
            if (completed.returncode != 0 or harness.get('status') != 'qa_exited'
                    or harness.get('owned_work_removed') is not True
                    or harness.get('persistence', {}).get('all_owned_process_groups_gone') is not True):
                raise ValueError('launcher_phase_failed_no_next_App')
            record_name = 'persistence-seed.json' if phase == 'seed' else 'persistence-postrun.json'
            record = json.loads(helpers['read_owned'](output / phase / 'independent-qa-workflow' / record_name, 32768))
            helpers['assert_process_gone'](record['app_pid'])
            item['app_pid'] = record['app_pid']
            item['record_sha256'] = hashlib.sha256(helpers['read_owned'](output / phase / 'independent-qa-workflow' / record_name, 32768)).hexdigest()
            if phase == 'reopen' and report['phases'][0]['app_pid'] == record['app_pid']:
                raise ValueError('distinct_App_PID_required')
            evidence_bytes(output)
        report['status'] = 'two_process_UI_checks_completed_review_pending'
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        detail = str(error)
        report['blocking_reason'] = detail if detail.replace('_', '').isalnum() and len(detail) <= 96 else 'pair_runtime_or_record_unavailable'
    finally:
        # Only successful, fully inspected batch state is deleted. On incomplete
        # runs preserve it outside artifacts; the ephemeral runner owns recovery.
        if report['status'] == 'two_process_UI_checks_completed_review_pending':
            try:
                helpers['validate_batch'](state, batch)
                for phase in report['phases']:
                    helpers['assert_process_gone'](phase['app_pid'])
                shutil.rmtree(state)
                report['state_removed'] = not state.exists()
            except (OSError, ValueError) as error:
                report['status'] = 'state_cleanup_failed'
                report['blocking_reason'] = 'owned_state_cleanup_or_identity_failed'
        report['retained_state_outside_artifacts'] = state.exists()
        try:
            report['artifact_bytes_before_pair_record'] = evidence_bytes(output)
        except (OSError, ValueError):
            report['status'] = 'pair_artifact_check_failed'
            report['blocking_reason'] = 'pair_evidence_identity_or_budget_failed'
        raw = (json.dumps(report, separators=(',', ':')) + '\n').encode()
        with (output / 'persistence-pair.json').open('xb') as stream:
            os.fchmod(stream.fileno(), 0o600)
            stream.write(raw)
    return 0 if report['status'] == 'two_process_UI_checks_completed_review_pending' and report['state_removed'] else 2


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'candidate-manifest', 'identity-approval', 'ui-approval', 'input-dir', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--expected-sha', required=True)
    args = parser.parse_args()
    if not os.environ.get('DISPLAY'):
        parser.error('one dedicated outer Xvfb required')
    return run_pair(args)


if __name__ == '__main__':
    raise SystemExit(main())
