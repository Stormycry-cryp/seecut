#!/usr/bin/env python3
"""Finite public UI observation, with current-frame guards before every click.

No application internals, file dialog typing, grants, clipboard, or MCP calls.
The launcher owns the one private App instance and its cleanup within 300s.
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

HEAD = 'ef1831769d52daceb1f38bcd15679c61f595408d'
APP_SHA = 'a734649f619c317e5d051b0d98b5d590f8ee7db8cddfaa437d19fe4143b8db67'


class Stop(Exception):
    pass


def read_json(path):
    if not path.is_absolute() or path.is_symlink() or not path.is_file():
        raise Stop('explicit_regular_declaration_required')
    raw = path.read_bytes()
    if len(raw) > 32768:
        raise Stop('declaration_size_limit')
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-pid', type=int, required=True)
    parser.add_argument('--window-id', type=int)
    parser.add_argument('--work-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--identity-approval', type=Path, required=True)
    parser.add_argument('--ui-approval', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--isolated-display-capture', action='store_true')
    parser.add_argument('--private-accessibility-bus', action='store_true')
    parser.add_argument('--probe-python', default='/usr/bin/python3')
    args = parser.parse_args()
    start = time.monotonic()
    deadline = min(start + 270, args.deadline_monotonic - 15)
    report = {'phase': 'finite-workflow-observation', 'status': 'blocked',
              'product_verdict': 'pending_independent_actual_evidence_review',
              'actions': [], 'guards': [], 'captures': [], 'app_pid': args.app_pid,
              'file_input_attempted': False, 'file_selected': False,
              'permissions_granted': False, 'client_started': False,
              'paid_action_requested': False, 'settings_pixels_captured': False,
              'cleanup_owner': 'launcher; whole App run including cleanup <=300s'}
    evidence = None
    settings_entered = False

    def command(argv, timeout=6, binary=False):
        left = deadline - time.monotonic()
        if left < 0.25:
            raise Stop('deadline_no_further_input')
        try:
            return subprocess.run(argv, check=True, capture_output=True,
                                  text=not binary, timeout=min(timeout, left)).stdout
        except subprocess.TimeoutExpired:
            raise Stop('command_timeout_no_retry') from None
        except subprocess.CalledProcessError:
            raise Stop('public_UI_command_failed_raw_output_withheld') from None

    def pause():
        if deadline - time.monotonic() < 1:
            raise Stop('insufficient_capture_time')
        time.sleep(0.7)

    def snapshot(name, root=False):
        if settings_entered:
            raise Stop('settings_pixels_forbidden')
        path = evidence / (name + '.png')
        if path.exists():
            raise Stop('evidence_path_already_exists')
        command(['import', '-window', 'root' if root else str(window),
                 '-strip', str(path)], timeout=8)
        os.chmod(path, 0o600)
        size = path.stat().st_size
        if size > 2 * 1024 * 1024 or sum(x['bytes'] for x in report['captures']) + size > 14 * 1024 * 1024:
            raise Stop('PNG_budget_exceeded_do_not_publish')
        report['captures'].append({'file': path.name, 'bytes': size,
                                  'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                                  'surface': 'dedicated-private-Xvfb' if root else 'App-window'})
        return path

    def guard(frame, name):
        spec = ui['guards'][name]
        x, y, width, height = spec['region']
        raw = command(['convert', str(frame), '-crop', f'{width}x{height}+{x}+{y}',
                       '+repage', '-alpha', 'off', '-depth', '8', 'rgb:-'], binary=True)
        digest = hashlib.sha256(raw).hexdigest()
        matched = len(raw) == width * height * 3 and digest == spec['rgb_sha256']
        report['guards'].append({'frame': frame.name, 'control_context': name,
                                 'rgb_sha256': digest, 'matched': matched})
        if not matched:
            raise Stop('current_control_context_differs:' + name)

    def click(key, frame, contexts, direct=False):
        for context in contexts:
            guard(frame, context)
        xy = ui['direct_coordinates' if direct else 'conditional_coordinates'][key]
        # Recheck exact identity/focus before input; no old coordinates alone authorize it.
        if int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid:
            raise Stop('App_window_identity_changed')
        command(['xdotool', 'mousemove', '--window', str(window), *map(str, xy)])
        command(['xdotool', 'click', '1'])
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        report['actions'].append({'kind': 'click', 'target': key,
                                  'guarded_frame': frame.name, 'xy': xy})
        pause()

    def owned_focus(pid):
        visited = set()
        while pid >= 2 and pid not in visited and len(visited) < 16:
            visited.add(pid)
            proc = Path('/proc') / str(pid)
            if proc.stat().st_uid != os.getuid():
                return False
            if pid == args.app_pid:
                return True
            lines = (proc / 'status').read_text().splitlines()
            parent = next((line for line in lines if line.startswith('PPid:')), None)
            if parent is None:
                return False
            pid = int(parent.split()[1])
        return False

    def cancel_to_editor(name):
        focused = int(command(['xdotool', 'getwindowfocus']).strip())
        pid = int(command(['xdotool', 'getwindowpid', str(focused)]).strip())
        if not owned_focus(pid):
            raise Stop('unknown_focus_owner_no_Escape')
        command(['xdotool', 'key', '--clearmodifiers', 'Escape'])
        report['actions'].append({'kind': 'key', 'key': 'Escape',
                                  'focused_window': focused, 'owned_pid': pid})
        pause()
        frame = snapshot(name)
        if int(command(['xdotool', 'getwindowfocus']).strip()) != window:
            raise Stop('dialog_not_proven_closed_no_next_input')
        for context in ('editor-toolbar', 'editor-tools', 'blank-canvas'):
            guard(frame, context)
        return frame

    try:
        if args.app_pid < 2 or deadline <= start or not args.isolated_display_capture or not os.environ.get('DISPLAY'):
            raise Stop('live_private_Xvfb_and_deadline_required')
        for directory in (args.work_dir, args.output):
            if not directory.is_absolute() or directory.is_symlink() or not directory.is_dir() or directory.stat().st_uid != os.getuid():
                raise Stop('explicit_owned_isolated_directory_required')
        runtime, runtime_sha = read_json(args.identity_approval)
        ui, ui_sha = read_json(args.ui_approval)
        if (runtime.get('schema') != 2 or runtime.get('reviewed_by') != 'main-reviewer'
                or runtime.get('runtime_head') != HEAD or runtime.get('runtime_app_sha256') != APP_SHA
                or runtime.get('change_scope') not in ('tests-and-runtime-only', 'product-candidate')):
            raise Stop('main_runtime_identity_mismatch')
        if (ui.get('schema') != 2 or ui.get('reviewed_by') != 'independent-qa'
                or ui.get('observed_head') != HEAD or ui.get('observed_app_sha256') != APP_SHA
                or ui.get('window') != {'width': 1280, 'height': 900}):
            raise Stop('independent_UI_identity_mismatch')
        for spec in ui['guards'].values():
            if (len(spec['region']) != 4 or any(type(n) is not int for n in spec['region'])
                    or not re.fullmatch('[0-9a-f]{64}', spec['rgb_sha256'])):
                raise Stop('invalid_visual_guard')
            x, y, width, height = spec['region']
            if min(x, y) < 0 or min(width, height) < 1 or x + width > 1280 or y + height > 900:
                raise Stop('guard_outside_current_window')
        for mapping in ('direct_coordinates', 'conditional_coordinates'):
            for xy in ui[mapping].values():
                if len(xy) != 2 or any(type(n) is not int for n in xy) or not (0 <= xy[0] < 1280 and 32 <= xy[1] < 900):
                    raise Stop('invalid_UI_coordinate')
        for tool in ('xdotool', 'import', 'convert'):
            if not shutil.which(tool):
                raise Stop('missing_visible_UI_dependency:' + tool)
        proc = Path('/proc') / str(args.app_pid)
        if proc.stat().st_uid != os.getuid():
            raise Stop('App_not_same_user')
        digest = hashlib.sha256()
        with (proc / 'exe').open('rb') as stream:
            while True:
                if time.monotonic() >= deadline:
                    raise Stop('identity_hash_deadline')
                chunk = stream.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
        if digest.hexdigest() != APP_SHA:
            raise Stop('actual_App_binary_SHA_mismatch_no_input')
        report.update(app_sha256=digest.hexdigest(), source_head=HEAD,
                      identity_declaration_sha256=runtime_sha, UI_declaration_sha256=ui_sha)
        dims = command(['xdotool', 'getdisplaygeometry']).split()
        if len(dims) != 2 or not (1280 <= int(dims[0]) <= 1920 and 900 <= int(dims[1]) <= 1200):
            raise Stop('private_display_dimensions_outside_bounds')
        ids = sorted(set(map(int, command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)]).split())))
        if args.window_id is None:
            if len(ids) != 1:
                raise Stop('one_visible_App_window_required')
            window = ids[0]
        else:
            window = args.window_id
            if window not in ids:
                raise Stop('supplied_window_not_visible')
        if int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid:
            raise Stop('window_App_PID_mismatch')
        command(['xdotool', 'windowsize', '--sync', str(window), '1280', '900'])
        command(['xdotool', 'windowfocus', '--sync', str(window)])
        fields = dict(line.split('=', 1) for line in command(['xdotool', 'getwindowgeometry', '--shell', str(window)]).splitlines() if '=' in line)
        if fields.get('WIDTH') != '1280' or fields.get('HEIGHT') != '900':
            raise Stop('actual_window_size_mismatch')
        evidence = args.output / 'independent-qa-workflow'
        evidence.mkdir(mode=0o700)
        report['window_id'] = window
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        pause()
        frame = snapshot('01-current-initial')
        click('quick_mode', frame, ['first-mode'], direct=True)
        frame = snapshot('02-after-quick')
        click('canvas_navigation', frame, ['canvas-navigation'], direct=True)
        frame = snapshot('03-gallery')
        click('new_project', frame, ['new-project'])
        frame = snapshot('04-create-dialog')
        click('new_create', frame, ['create-dialog'])
        frame = snapshot('05-editor')
        click('editor_folder', frame, ['editor-toolbar', 'editor-tools', 'blank-canvas'])
        frame = snapshot('06-open-menu')
        click('open_local_image', frame, ['open-menu'])
        snapshot('07-local-picker-private-display', root=True)
        frame = cancel_to_editor('08-after-picker-Escape')
        click('editor_save', frame, ['editor-toolbar', 'editor-tools', 'blank-canvas'])
        snapshot('09-save-private-display', root=True)
        frame = cancel_to_editor('10-after-save-Escape')
        click('editor_export', frame, ['editor-toolbar', 'editor-tools', 'blank-canvas'])
        snapshot('11-export-private-display', root=True)
        frame = cancel_to_editor('12-after-export-Escape')
        # Secrets may already be present when Settings opens. Never capture it.
        if args.private_accessibility_bus:
            helper = Path(__file__).with_name('public_ui_probe.py')
            if hashlib.sha256(helper.read_bytes()).hexdigest() != 'b0f3724aabe7782f9b19dd166140a273929d5a29b01fa4d3317121d8c6e10460':
                raise Stop('reviewed_public_metadata_helper_changed_no_settings_input')
            click('settings', frame, ['settings-icon', 'editor-toolbar', 'editor-tools'])
            settings_entered = True
            completed = subprocess.run([args.probe_python, '-B', str(helper), '--app-pid', str(args.app_pid),
                                        '--output', str(evidence), '--deadline-monotonic', str(min(deadline, time.monotonic() + 3)),
                                        '--private-accessibility-bus'], capture_output=True, timeout=min(5, max(0.25, deadline - time.monotonic())))
            report['settings_public_probe_exit'] = completed.returncode
            # Only the bounded helper file is retained; raw stdout/stderr are discarded.
        else:
            report['settings_observation'] = 'not_opened_private_public_metadata_bus_missing'
        report['status'] = 'bounded_UI_observation_completed_review_pending'
    except (Stop, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as exc:
        report['blocking_reason'] = str(exc) if isinstance(exc, Stop) else 'input_or_public_runtime_unavailable_raw_error_withheld'
    finally:
        report['elapsed_seconds'] = round(time.monotonic() - start, 3)
        report['settings_entered'] = settings_entered
        raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        if evidence is not None:
            artifact_bytes = sum(p.stat().st_size for p in evidence.rglob('*') if p.is_file())
            if artifact_bytes + len(raw) > 15 * 1024 * 1024:
                report['status'] = 'artifact_budget_exceeded_do_not_publish'
                raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
            with (evidence / 'workflow.json').open('xb') as stream:
                os.chmod(evidence / 'workflow.json', 0o600)
                stream.write(raw)
        print(raw.decode(), end='')
    return 0 if report['status'] == 'bounded_UI_observation_completed_review_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
