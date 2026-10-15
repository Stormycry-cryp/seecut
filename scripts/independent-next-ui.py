#!/usr/bin/env python3
"""Reviewed navigation and safe Settings/control/focus observation.

Seven App PNGs before Settings only; no Settings pixels, values or UI grants.
Theme controls are discovered through bounded public metadata, never activated.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

HEAD = 'cecc8fdf9578675051dae58bda25f0ff805ce235'
APP_SHA = '8859b10e7b8a58785d6a60454995f33d56efea554011825287917d06064aff55'


class Stop(Exception):
    pass


def read_declaration(path):
    if (not path.is_absolute() or path.is_symlink() or not path.is_file()
            or path.stat().st_size > 16384):
        raise Stop('explicit_regular_declaration_required')
    raw = path.read_bytes()
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


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
    parser.add_argument('--next-stage', choices=['navigation'], default='navigation')
    parser.add_argument('--isolated-display-capture', action='store_true')
    parser.add_argument('--private-accessibility-bus', action='store_true')
    parser.add_argument('--probe-python', default='/usr/bin/python3')
    args = parser.parse_args()
    start = time.monotonic()
    deadline = min(args.deadline_monotonic - 15, start + 90)
    evidence = None
    settings_pixels_forbidden = False
    report = {'phase': 'cecc8fd-navigation-settings-observation', 'status': 'blocked',
              'source_head': HEAD, 'product_verdict': 'pending_independent_actual_image_review',
              'actions': [], 'captures': [], 'client_started': False,
              'permissions_granted': False, 'project_created': False,
              'paid_action_requested': False, 'settings_entered': False, 'settings_entry_attempted': False,
              'settings_pixels_captured': False, 'theme_changed': False, 'public_probe_results': [],
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
            raise Stop('Settings_pixels_permanently_forbidden')
        path = evidence / (name + '.png')
        if path.exists():
            raise Stop('capture_path_already_exists')
        command(['import', '-window', str(window), '-strip', str(path)])
        os.chmod(path, 0o600)
        raw = path.read_bytes()
        if len(raw) > 2 * 1024 * 1024 or sum(x['bytes'] for x in report['captures']) + len(raw) > 14 * 1024 * 1024:
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

    def probe(name, required=False):
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
        if (ui.get('schema') != 1 or ui.get('reviewed_by') != 'independent-qa'
                or ui.get('observed_head') != HEAD or ui.get('observed_app_sha256') != APP_SHA
                or ui.get('stage') != 'navigation-settings-observation'
                or ui.get('window') != {'width': 1280, 'height': 900}
                or ui.get('quick_mode') != [558, 500]
                or ui.get('navigation_targets') != {'canvas': [40, 170], 'clip': [40, 227], 'assets': [40, 285], 'settings': [40, 805]}
                or ui.get('mode_guard', {}).get('region') != [420, 388, 520, 155]):
            raise Stop('independent_current_UI_declaration_required')
        helper = Path(__file__).with_name('public_probe_cecc8fd_ui2.py')
        if not helper.is_file() or hashlib.sha256(helper.read_bytes()).hexdigest() != ui.get('public_probe_sha256'):
            raise Stop('reviewed_public_probe_SHA_required')
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
        new_evidence = args.output / 'independent-qa-cecc8fd-ui2'
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
        # No activation keys: only inspect real focus and its reverse traversal.
        key('Tab')
        frame = snapshot('03-workbench-Tab')
        probe('03-workbench-Tab-public')
        key('shift+Tab')
        frame = snapshot('04-workbench-ShiftTab')
        probe('04-workbench-ShiftTab-public')
        for index, target in enumerate(('canvas', 'clip', 'assets'), 5):
            click_navigation(target, frame)
            frame = snapshot(f'{index:02d}-after-' + target + '-navigation')
        # The actual hover identified Settings. Entry is the final pixel action.
        # Set the prohibition BEFORE input; even a timeout cannot permit a screenshot.
        matched_guard(frame, 'nav-settings')
        ensure_main_focus()
        settings_pixels_forbidden = True
        report['settings_entry_attempted'] = True
        command(['xdotool', 'mousemove', '--window', str(window), '40', '805'])
        command(['xdotool', 'click', '1'])
        report['actions'].append({'kind': 'click', 'target': 'actual_Settings_entry', 'xy': [40, 805]})
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        pause()
        metadata = probe('08-settings-public', required=True)
        context_labels = {'主题', '外观', '通用', '常规', '工作模式', '本地 Agent 权限',
                          'General', 'Appearance', 'Theme', 'Local Agent Permissions'}
        if not any(n.get('showing') and n.get('label') in context_labels for n in metadata['nodes']):
            raise Stop('Settings_context_not_publicly_identified_no_more_keys')
        report['settings_entered'] = True
        report['theme_candidates'] = [{k: n.get(k) for k in ('path', 'label', 'role', 'showing', 'enabled',
            'sensitive', 'selected', 'checked', 'action_interface')} for n in metadata['nodes']
            if n.get('showing') and n.get('label') in {'浅色', '深色', '跟随系统', '系统', 'Light', 'Dark', 'System'}]
        # Safe observation only; names/bounds do not authorize theme changes.
        key('Tab')
        probe('09-settings-Tab-public', required=True)
        key('shift+Tab')
        metadata = probe('10-settings-ShiftTab-public', required=True)
        if any(n.get('showing') and n.get('dialog') for n in metadata['nodes']):
            key('Escape')
            probe('11-settings-after-Escape-public', required=True)
            report['Settings_Escape'] = 'one_key_on_publicly_observed_dialog; actual_close/focus_requires_review'
        else:
            report['Settings_Escape'] = 'not_sent_no_public_dialog_evidence'
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
            with (evidence / 'navigation.json').open('xb') as stream:
                os.chmod(evidence / 'navigation.json', 0o600)
                stream.write(raw)
        print(raw.decode(), end='')
    return 0 if report['status'] == 'finite_observation_completed_review_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
