#!/usr/bin/env python3
"""Five-frame 1280-only bootstrap derived from frozen A3 canvas-entry.

Creation guards retain the exact A3 public/RGB/current focus requirements.
No Settings branch, other-width resize, file input or assistant input.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

from public_action_11ebf20_ui4 import settings_visible

HEAD = '0aa9406247e53f073c0b4df686adc68b40e2f8f6'
APP_SHA = '03fb1752c34adb0c0f4a21521c2302ee80c9b23203728cbcf636d3a4a57eaea2'


class Stop(Exception):
    pass


def read_declaration(path):
    if (not path.is_absolute() or path.is_symlink() or not path.is_file()
            or path.stat().st_size > 16384):
        raise Stop('explicit_regular_declaration_required')
    raw = path.read_bytes()
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def canvas_create_target(data):
    """Supplement the exact current modal pixels with its observed public controls.

    Slint currently exposes these as window siblings. This function alone never
    authorizes a click: the controller must also verify the full modal RGB guard.
    """
    if data.get('status') != 'public_metadata_observed' or not data.get('coverage_complete'):
        return None
    nodes = data.get('nodes', [])
    expected = {
        '新建画布': (29, {'x': 538, 'y': 369, 'width': 270, 'height': 42}),
        '取消': (43, {'x': 728, 'y': 503, 'width': 52, 'height': 40}),
        '创建': (43, {'x': 788, 'y': 503, 'width': 52, 'height': 40}),
    }
    selected = {}
    for label, (role, bounds) in expected.items():
        found = [n for n in nodes if n.get('label') == label and n.get('role') == role and n.get('showing')]
        if len(found) != 1:
            return None
        node = found[0]
        if (node.get('bounds') != bounds or any(node.get(k) for k in ('entry', 'editable', 'editable_text_interface'))
                or not node.get('path') or len(node['path']) != 2):
            return None
        if role == 43 and (not all(node.get(k) for k in ('enabled', 'sensitive', 'action_interface'))
                or node.get('allowed_actions') != ['click']):
            return None
        selected[label] = node
    parents = {tuple(n['path'][:-1]) for n in selected.values()}
    if len(parents) != 1 or settings_visible(data):
        return None
    windows = [n for n in nodes if tuple(n.get('path', [])) in parents and n.get('showing')
               and n.get('role') == 23 and n.get('bounds') == {'x': 0, 'y': 0, 'width': 1280, 'height': 900}]
    return selected['创建'] if len(windows) == 1 else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-pid', type=int, required=True)
    parser.add_argument('--work-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--identity-approval', type=Path, required=True)
    parser.add_argument('--ui-approval', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--window-id', type=int)
    parser.add_argument('--expected-sha', default=HEAD)
    parser.add_argument('--client-binary', type=Path)  # Launcher compatibility; never accessed/launched.
    parser.add_argument('--next-stage', choices=['canvas-entry'], required=True)
    parser.add_argument('--isolated-display-capture', action='store_true')
    parser.add_argument('--private-accessibility-bus', action='store_true')
    parser.add_argument('--probe-python', default='/usr/bin/python3')
    args = parser.parse_args()
    start = time.monotonic()
    deadline = min(args.deadline_monotonic - 15, start + 120)
    evidence = None
    settings_pixels_forbidden = False
    report = {'phase': 'agent-native-bootstrap', 'scope': args.next_stage, 'status': 'blocked',
              'source_head': HEAD, 'product_verdict': 'pending_main_actual_image_review',
              'actions': [], 'captures': [], 'client_started': False,
              'permissions_granted': False, 'project_created': False,
              'paid_action_requested': False, 'settings_entered': False, 'settings_entry_attempted': False,
              'settings_pixels_captured': False, 'theme_changed': False, 'mode_changed': False, 'public_probe_results': [],
              'cleanup_owner': 'launcher; whole App run including cleanup <=300 seconds'}

    def command(argv, binary=False):
        left = deadline - time.monotonic()
        if left < 0.25:
            raise Stop('deadline_no_further_input')
        try:
            return subprocess.run(argv, check=True, capture_output=True,
                                  text=not binary, timeout=min(8, left)).stdout
        except (subprocess.TimeoutExpired, subprocess.CalledProcessError):
            raise Stop('public_UI_command_unavailable_no_retry_raw_output_withheld') from None

    def pause():
        if deadline - time.monotonic() < 1:
            raise Stop('insufficient_capture_time')
        time.sleep(0.7)

    def snapshot(name):
        if settings_pixels_forbidden:
            raise Stop('Settings_pixels_forbidden_until_complete_public_close_proof')
        path = evidence / (name + '.png')
        if path.exists():
            raise Stop('capture_path_already_exists')
        command(['import', '-window', str(window), '-strip', str(path)])
        os.chmod(path, 0o600)
        raw = path.read_bytes()
        if len(report['captures']) >= 5 or len(raw) > 2 * 1024 * 1024 or sum(x['bytes'] for x in report['captures']) + len(raw) > 10 * 1024 * 1024:
            raise Stop('PNG_budget_exceeded_do_not_publish')
        report['captures'].append({'file': path.name, 'bytes': len(raw),
                                   'sha256': hashlib.sha256(raw).hexdigest(), 'surface': 'App-window'})
        return path

    def geometry(width):
        if int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid:
            raise Stop('App_window_identity_changed')
        command(['xdotool', 'windowsize', '--sync', str(window), str(width), '900'])
        pause()
        fields = dict(line.split('=', 1) for line in command(
            ['xdotool', 'getwindowgeometry', '--shell', str(window)]).splitlines() if '=' in line)
        if fields.get('WIDTH') != str(width) or fields.get('HEIGHT') != '900':
            raise Stop('actual_window_size_differs')
        x, y = int(fields['X']), int(fields['Y'])
        if x < 0 or y < 0 or x + width > int(dims[0]) or y + 900 > int(dims[1]):
            raise Stop('App_window_outside_private_display')
        report['actions'].append({'kind': 'resize', 'width': width, 'height': 900})

    def modal_digest(frame):
        x, y, width, height = ui['mode_guard']['region']
        raw = command(['convert', str(frame), '-crop', f'{width}x{height}+{x}+{y}',
                       '+repage', '-alpha', 'off', '-depth', '8', 'rgb:-'], binary=True)
        if len(raw) != width * height * 3:
            raise Stop('mode_guard_RGB_size_differs')
        return hashlib.sha256(raw).hexdigest()

    def matched_guard(frame, name):
        spec = ui['guards'][name]
        x, y, width, height = spec['region']
        raw = command(['convert', str(frame), '-crop', f'{width}x{height}+{x}+{y}',
                       '+repage', '-alpha', 'off', '-depth', '8', 'rgb:-'], binary=True)
        digest = hashlib.sha256(raw).hexdigest()
        report.setdefault('guards', []).append({'frame': frame.name, 'control': name, 'RGB_sha256': digest})
        if len(raw) != width * height * 3 or digest != spec['rgb_sha256']:
            raise Stop('current_navigation_control_differs:' + name)

    def ensure_main_focus():
        if int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid:
            raise Stop('App_window_identity_changed')
        if int(command(['xdotool', 'getwindowfocus']).strip()) != window:
            raise Stop('focus_outside_reviewed_App_no_key_or_click')

    def key(name):
        ensure_main_focus()
        command(['xdotool', 'key', '--clearmodifiers', name])
        report['actions'].append({'kind': 'key', 'key': name, 'meaning': 'actual_focus_or_dialog_observation'})
        pause()

    def click_navigation(target, frame):
        matched_guard(frame, 'nav-' + target)
        ensure_main_focus()
        x, y = ui['navigation_targets'][target]
        command(['xdotool', 'mousemove', '--window', str(window), str(x), str(y)])
        command(['xdotool', 'click', '1'])
        report['actions'].append({'kind': 'click', 'target': target, 'xy': [x, y]})
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        pause()

    def probe(name, required=True):
        until = min(deadline, time.monotonic() + 3)
        try:
            completed = subprocess.run([args.probe_python, '-B', str(helper), '--app-pid', str(args.app_pid),
                '--owned-root-pid', str(args.app_pid), '--output', str(evidence), '--output-name', name.lower() + '.json',
                '--deadline-monotonic', str(until), '--private-accessibility-bus'], capture_output=True,
                timeout=min(5, max(0.25, deadline - time.monotonic())))
        except subprocess.TimeoutExpired:
            raise Stop('public_probe_timeout_no_retry') from None
        path = evidence / (name.lower() + '.json')
        data = {}
        if path.is_file() and not path.is_symlink() and path.stat().st_size <= 128 * 1024:
            data = json.loads(path.read_text())
        valid = completed.returncode == 0 and data.get('coverage_complete') and data.get('status') == 'public_metadata_observed'
        report['public_probe_results'].append({'file': path.name, 'complete': bool(valid),
            'focused_nodes': [{k: n.get(k) for k in ('path', 'label', 'role', 'showing')} for n in data.get('nodes', []) if n.get('focused')]})
        if required and not valid:
            raise Stop('public_settings_metadata_incomplete_no_further_input')
        return data


    try:
        if args.expected_sha != HEAD or args.app_pid < 2 or deadline <= start:
            raise Stop('exact_candidate_and_live_deadline_required')
        if not args.isolated_display_capture or not args.private_accessibility_bus or not os.environ.get('DISPLAY'):
            raise Stop('private_QA_display_attestation_required')
        runtime, runtime_sha = read_declaration(args.identity_approval)
        ui, ui_sha = read_declaration(args.ui_approval)
        if (runtime.get('schema') != 2 or runtime.get('reviewed_by') != 'main-reviewer'
                or runtime.get('runtime_head') != HEAD or runtime.get('runtime_app_sha256') != APP_SHA
                or runtime.get('change_scope') != 'product-candidate'):
            raise Stop('main_exact_runtime_identity_required')
        if (ui.get('schema') != 1 or ui.get('reviewed_by') != 'main-reviewer'
                or ui.get('observed_head') != HEAD or ui.get('observed_app_sha256') != APP_SHA
                or ui.get('stage') != '0aa-initial-mode-reviewed-conditional-exact-guards'
                or ui.get('window') != {'width': 1280, 'height': 900}
                or ui.get('quick_mode') != [558, 500]
                or ui.get('navigation_targets') != {'canvas': [40, 170], 'clip': [40, 227], 'assets': [40, 285], 'settings': [40, 805]}
                or ui.get('mode_guard', {}).get('region') != [420, 388, 520, 155]):
            raise Stop('independent_current_UI_declaration_required')
        helper = Path(__file__).with_name('public_probe_11ebf20_ui4.py')
        if not helper.is_file() or hashlib.sha256(helper.read_bytes()).hexdigest() != ui.get('public_probe_sha256'):
            raise Stop('reviewed_public_probe_SHA_required')
        actor = Path(__file__).with_name('public_action_11ebf20_ui4.py')
        if not actor.is_file() or hashlib.sha256(actor.read_bytes()).hexdigest() != ui.get('public_action_sha256'):
            raise Stop('reviewed_public_action_SHA_required')
        if ui.get('new_project') != [290, 251] or ui.get('public_action_intents') != ['professional', 'dark', 'close-settings']:
            raise Stop('exact_A2_control_scope_required')
        for path in (args.work_dir, args.output):
            if not path.is_absolute() or path.is_symlink() or not path.is_dir() or path.stat().st_uid != os.getuid():
                raise Stop('explicit_owned_isolated_directory_required')
        proc = Path('/proc') / str(args.app_pid)
        if proc.stat().st_uid != os.getuid():
            raise Stop('App_same_OS_user_required')
        digest = hashlib.sha256()
        with (proc / 'exe').open('rb') as stream:
            while True:
                if time.monotonic() >= deadline:
                    raise Stop('deadline_during_binary_verification')
                data = stream.read(1024 * 1024)
                if not data:
                    break
                digest.update(data)
        if digest.hexdigest() != APP_SHA:
            raise Stop('actual_App_SHA_differs')
        report.update(app_sha256=digest.hexdigest(), identity_declaration_sha256=runtime_sha,
                      UI_declaration_sha256=ui_sha)
        dims = command(['xdotool', 'getdisplaygeometry']).split()
        if len(dims) != 2 or not (1440 <= int(dims[0]) <= 1920 and 900 <= int(dims[1]) <= 1200):
            raise Stop('private_display_dimensions_outside_finite_bounds')
        if args.window_id is None:
            ids = set(command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)]).split())
            if len(ids) != 1:
                raise Stop('one_visible_App_window_required')
            window = int(ids.pop())
        else:
            window = args.window_id
        if str(window) not in command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)]).split():
            raise Stop('explicit_App_window_not_visible')
        new_evidence = args.output / 'main-qa-agent-native-bootstrap'
        new_evidence.mkdir(mode=0o700)
        evidence = new_evidence
        geometry(1280)
        command(['xdotool', 'windowfocus', '--sync', str(window)])
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        before = snapshot('01-before-quick')
        if modal_digest(before) != ui['mode_guard']['rgb_sha256']:
            raise Stop('current_mode_dialog_differs_no_click')
        command(['xdotool', 'mousemove', '--window', str(window), '558', '500'])
        command(['xdotool', 'click', '1'])
        report['actions'].append({'kind': 'click', 'target': 'visible_quick_mode', 'xy': [558, 500]})
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        pause()
        after = snapshot('02-after-quick-1280x900')
        if modal_digest(after) == ui['mode_guard']['rgb_sha256']:
            raise Stop('reviewed_mode_dialog_still_visible')
        if args.next_stage == 'canvas-entry':
            click_navigation('canvas', after)
            gallery = snapshot('03-canvas-gallery-1280x900')
            matched_guard(gallery, 'new-project')
            ensure_main_focus()
            x, y = ui['new_project']
            command(['xdotool', 'mousemove', '--window', str(window), str(x), str(y)])
            command(['xdotool', 'click', '1'])
            report['actions'].append({'kind': 'click', 'target': 'actual_new_project_card', 'xy': [x, y]})
            command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
            pause()
            frame = snapshot('06-current-new-canvas-dialog')
            matched_guard(frame, 'current-create-dialog')
            data = probe('06-current-new-canvas-dialog-public')
            target = canvas_create_target(data)
            if target is None:
                raise Stop('current_create_dialog_public_context_changed_no_create')
            ensure_main_focus()
            if ui.get('new_create') != [814, 523]:
                raise Stop('exact_reviewed_create_point_required')
            # One click, protected by current full-modal pixels and actual named controls.
            report['project_creation_attempted'] = True
            report['actions'].append({'kind': 'one_current_guarded_click', 'target': 'create-blank',
                                      'guarded_frame': frame.name, 'path': target['path'], 'xy': [814, 523]})
            command(['xdotool', 'mousemove', '--window', str(window), '814', '523'])
            command(['xdotool', 'click', '1'])
            command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
            pause()
            data = probe('07-create-result-public')
            snapshot('07-current-create-result')
            labels = {n.get('label') for n in data['nodes'] if n.get('showing')}
            if ('新建画布' in labels or settings_visible(data)
                    or not {'属性', '图层'}.issubset(labels)):
                raise Stop('actual_blank_editor_requires_review_no_further_input')
            report['project_created'] = 'visible_editor_context_pending_main_pixels_review'
        report['status'] = 'finite_observation_completed_review_pending'
    except (Stop, OSError, ValueError, KeyError, TypeError) as exc:
        report['blocking_reason'] = str(exc) if isinstance(exc, Stop) else 'public_runtime_unavailable_raw_error_withheld'
    finally:
        report['elapsed_seconds'] = round(time.monotonic() - start, 3)
        raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        if evidence is not None:
            if sum(p.stat().st_size for p in evidence.iterdir() if p.is_file()) + len(raw) > 15 * 1024 * 1024:
                report['status'] = 'artifact_budget_exceeded_do_not_publish'
                raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
            with (evidence / 'bootstrap.json').open('xb') as stream:
                os.chmod(evidence / 'bootstrap.json', 0o600)
                stream.write(raw)
        print(raw.decode(), end='')
    return 0 if report['status'] == 'finite_observation_completed_review_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
