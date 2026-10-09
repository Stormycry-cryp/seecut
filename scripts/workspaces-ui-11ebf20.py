#!/usr/bin/env python3
"""Finite light quick navigation: clip/assets layouts, then return generation.

Reuse the frozen public metadata probe. No page controls, fields or Settings.
The launcher owns the fresh isolated App and cleanup within 300 seconds.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

HEAD = '11ebf203e1a78d3b6a21677c4b17e96223c74b0e'
APP_SHA = '8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30'
NAV_KEYS = ('path', 'role', 'label', 'showing', 'enabled', 'sensitive', 'focusable',
            'button', 'radio', 'entry', 'editable', 'editable_text_interface',
            'action_interface', 'modal', 'dialog', 'file_chooser', 'bounds')


class Stop(Exception):
    pass


def read_declaration(path):
    if (not path.is_absolute() or path.is_symlink() or not path.is_file()
            or path.stat().st_size > 16384):
        raise Stop('explicit_regular_declaration_required')
    raw = path.read_bytes()
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def validate_navigation(data, contract, width=1280, height=900):
    """All four actual nav buttons must be unique and exactly healthy afresh."""
    nodes = data.get('nodes')
    if (data.get('status') != 'public_metadata_observed'
            or data.get('coverage_complete') is not True
            or data.get('field_values_read') is not False
            or not isinstance(nodes, list) or not 1 <= len(nodes) <= 512
            or any(not isinstance(n, dict) or not isinstance(n.get('path'), list)
                   or any(type(i) is not int or i < 0 for i in n['path']) for n in nodes)):
        raise Stop('current_public_metadata_incomplete_no_input_or_pixels')
    paths = [tuple(n.get('path', ())) for n in nodes]
    if len(paths) != len(set(paths)):
        raise Stop('duplicate_public_path_no_input_or_pixels')
    visible = [n for n in nodes if n.get('showing')]
    if any(n.get('dialog') or n.get('modal') or n.get('file_chooser') for n in visible):
        raise Stop('unknown_modal_no_input_or_pixels')
    # Settings nav itself is normal. Its content footprint forbids all pixels.
    settings_bounds = ({'x': 596, 'y': 147, 'width': 188, 'height': 36},
                       {'x': 596, 'y': 222, 'width': 188, 'height': 36},
                       {'x': 144, 'y': 68, 'width': 48, 'height': 24},
                       {'x': 1216, 'y': 68, 'width': 26, 'height': 128})
    if any((n.get('role') == 39 and n.get('label') in ('工作模式', '外观', '主题', '通用', '常规',
                 'General', 'Appearance', 'Theme', 'Work mode')) or n.get('bounds') in settings_bounds
           for n in visible):
        raise Stop('Settings_context_no_input_or_pixels')
    windows = [n for n in visible if n.get('role') == 23]
    if (len(windows) != 1 or windows[0].get('path') != [0]
            or windows[0].get('bounds') != {'x': 0, 'y': 0, 'width': width, 'height': height}
            or not all(windows[0].get(k) is True for k in ('enabled', 'sensitive'))):
        raise Stop('current_public_App_window_changed')
    result = {}
    for name, expected in contract.items():
        found = [n for n in visible if n.get('role') == 43 and n.get('label') == expected['label']]
        if len(found) != 1 or any(found[0].get(k) != expected[k] for k in NAV_KEYS):
            raise Stop('current_navigation_unknown_ambiguous_or_unhealthy:' + name)
        result[name] = found[0]
    return result


def rgb_guard_matches(raw, spec, width=1280, height=900):
    if len(raw) != width * height * 3:
        return False
    x, y, crop_width, crop_height = spec['region']
    if min(x, y) < 0 or min(crop_width, crop_height) < 1 or x + crop_width > width or y + crop_height > height:
        return False
    crop = b''.join(raw[(row * width + x) * 3:(row * width + x + crop_width) * 3]
                    for row in range(y, y + crop_height))
    return hashlib.sha256(crop).hexdigest() == spec['rgb_sha256']


def validate_ui(ui):
    if (ui.get('schema') != 1 or ui.get('reviewed_by') != 'main-reviewer'
            or ui.get('observed_head') != HEAD or ui.get('observed_app_sha256') != APP_SHA
            or ui.get('stage') != 'workspaces-public-navigation-observation-v1'
            or ui.get('window') != {'width': 1280, 'height': 900}
            or ui.get('quick_mode') != [558, 500]
            or ui.get('mode_guard', {}).get('region') != [420, 388, 520, 155]
            or ui.get('mode_guard', {}).get('rgb_sha256') != '2e513f04d1897fef7c6e6a0710c7afae9b7d6d592ddf1e316cb7e3fa89eb1b97'
            or ui.get('layout_matrix') != [[1280, 900], [1024, 900], [1440, 900], [1280, 720]]):
        raise Stop('main_exact_workspace_UI_declaration_required')
    expected_names = ('generate', 'canvas', 'clip', 'assets')
    labels = ('生成', '画布', '剪辑', '资产库')
    if set(ui.get('navigation', {})) != set(expected_names):
        raise Stop('exact_four_public_navigation_controls_required')
    for index, (name, label) in enumerate(zip(expected_names, labels), 1):
        target = ui['navigation'][name]
        y = (90, 148, 206, 264)[index - 1]
        if (target.get('path') != [0, index] or target.get('role') != 43 or target.get('label') != label
                or target.get('bounds') != {'x': 18, 'y': y, 'width': 44, 'height': 44}
                or target.get('xy') != [40, y + 22]
                or not all(target.get(k) is True for k in ('showing', 'enabled', 'sensitive', 'focusable', 'button'))
                or any(target.get(k) is not False for k in ('radio', 'entry', 'editable', 'editable_text_interface',
                                                           'action_interface', 'modal', 'dialog', 'file_chooser'))):
            raise Stop('frozen_actual_public_navigation_contract_required')
    if (ui.get('artifact_limits', {}).get('PNG_count_max') != 10
            or ui['artifact_limits'].get('PNG_each') != 2097152
            or ui['artifact_limits'].get('PNG_total') != 14680064
            or ui['artifact_limits'].get('all') != 15728640
            or ui.get('runtime_limits', {}).get('script_seconds') != 120
            or ui['runtime_limits'].get('whole_App_including_cleanup_seconds') != 300):
        raise Stop('finite_artifact_and_runtime_contract_required')


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
    parser.add_argument('--client-binary', type=Path)  # Launcher compatibility only.
    parser.add_argument('--isolated-display-capture', action='store_true')
    parser.add_argument('--private-accessibility-bus', action='store_true')
    parser.add_argument('--probe-python', default='/usr/bin/python3')
    args = parser.parse_args()
    start = time.monotonic()
    deadline = min(args.deadline_monotonic - 15, start + 120)
    evidence = None
    size = [1280, 900]
    report = {'phase': 'workspaces-11ebf20-v1', 'status': 'blocked', 'source_head': HEAD,
              'product_verdict': 'pending_main_actual_page_and_layout_image_review',
              'actions': [], 'captures': [], 'guards': [], 'public_probe_results': [],
              'project_created': False, 'file_input_attempted': False,
              'client_started': False, 'permissions_granted': False, 'paid_action_requested': False,
              'settings_entered': False, 'settings_pixels_captured': False, 'field_values_read': False,
              'cleanup_owner': 'launcher; whole App run including cleanup <=300 seconds'}

    def command(argv, binary=False):
        left = deadline - time.monotonic()
        if left < 0.25:
            raise Stop('deadline_no_further_input')
        try:
            return subprocess.run(argv, check=True, capture_output=True,
                                  text=not binary, timeout=min(8, left)).stdout
        except (subprocess.TimeoutExpired, subprocess.CalledProcessError) as exc:
            report['failed_command'] = argv[0] if argv[0] in ('xdotool', 'import') else 'withheld'
            report['failed_command_reason'] = ('deadline' if isinstance(exc, subprocess.TimeoutExpired)
                                               else 'nonzero_exit')
            raise Stop('public_UI_command_unavailable_no_retry_raw_output_withheld') from None

    def pause():
        if deadline - time.monotonic() < 1:
            raise Stop('insufficient_observation_time')
        time.sleep(0.7)

    def ensure_focus():
        if int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid:
            raise Stop('App_window_identity_changed')
        if set(command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)]).split()) != {str(window)}:
            raise Stop('unknown_or_multiple_App_windows_no_input_or_pixels')
        if int(command(['xdotool', 'getwindowfocus']).strip()) != window:
            raise Stop('focus_outside_reviewed_App_no_input_or_pixels')

    def geometry(width, height):
        ensure_focus()
        command(['xdotool', 'windowsize', '--sync', str(window), str(width), str(height)])
        pause()
        fields = dict(line.split('=', 1) for line in command(
            ['xdotool', 'getwindowgeometry', '--shell', str(window)]).splitlines() if '=' in line)
        if tuple(int(fields[k]) for k in ('X', 'Y', 'WIDTH', 'HEIGHT')) != (0, 0, width, height):
            raise Stop('actual_window_geometry_differs')
        size[:] = [width, height]
        report['actions'].append({'kind': 'resize', 'width': width, 'height': height})

    def probe(name):
        ensure_focus()
        until = min(deadline, time.monotonic() + 3)
        argv = [args.probe_python, '-B', str(helper), '--app-pid', str(args.app_pid),
                '--owned-root-pid', str(args.app_pid), '--output', str(evidence), '--output-name', name + '.json',
                '--deadline-monotonic', str(until), '--private-accessibility-bus']
        try:
            done = subprocess.run(argv, capture_output=True, timeout=min(5, max(0.25, deadline - time.monotonic())))
        except subprocess.TimeoutExpired:
            raise Stop('public_probe_timeout_no_retry') from None
        path = evidence / (name + '.json')
        if done.returncode != 0 or path.is_symlink() or not path.is_file() or path.stat().st_size > 131072:
            raise Stop('public_metadata_unavailable_no_input_or_pixels')
        data = json.loads(path.read_bytes())
        report['public_probe_results'].append({'file': path.name,
                                               'coverage_complete': data.get('coverage_complete') is True})
        validate_navigation(data, ui['navigation'], *size)
        return data

    def pixels():
        ensure_focus()
        # import supports depth and the RGB output coder, but not convert's -alpha option.
        raw = command(['import', '-window', str(window), '-depth', '8', 'rgb:-'], binary=True)
        if len(raw) != size[0] * size[1] * 3:
            raise Stop('current_RGB_capture_size_differs')
        return raw

    def guard(raw, name, spec=None):
        spec = spec or ui['guards'][name]
        matched = rgb_guard_matches(raw, spec, *size)
        report['guards'].append({'control': name, 'matched': matched, 'region': spec['region'],
                                 'expected_rgb_sha256': spec['rgb_sha256'], 'source': 'fresh_App_RGB_in_memory'})
        if not matched:
            raise Stop('current_control_RGB_differs:' + name)

    def snapshot(name):
        probe(name + '-public')  # Complete absence proof precedes every saved pixel capture.
        if len(report['captures']) >= 10:
            raise Stop('PNG_count_limit_before_capture')
        path = evidence / (name + '.png')
        if path.exists():
            raise Stop('capture_path_already_exists')
        command(['import', '-window', str(window), '-strip', str(path)])
        os.chmod(path, 0o600)
        raw = path.read_bytes()
        if len(raw) > 2097152 or sum(c['bytes'] for c in report['captures']) + len(raw) > 14680064:
            raise Stop('PNG_budget_exceeded_do_not_publish')
        report['captures'].append({'file': path.name, 'bytes': len(raw), 'width': size[0], 'height': size[1],
                                   'sha256': hashlib.sha256(raw).hexdigest(), 'surface': 'App-window'})

    def navigate(name, ordinal):
        if size != [1280, 900]:
            raise Stop('restore_reviewed_1280x900_before_navigation')
        probe(ordinal + '-before-' + name + '-public')
        raw = pixels()  # No saved extra PNG; fresh target plus full nav metadata.
        guard(raw, 'nav-' + name)
        ensure_focus()
        x, y = ui['navigation'][name]['xy']
        # Attempt is recorded before mouse/click; timeout never causes retry.
        report['actions'].append({'kind': 'one_guarded_navigation_attempt', 'target': name, 'xy': [x, y]})
        command(['xdotool', 'mousemove', '--window', str(window), str(x), str(y)])
        command(['xdotool', 'click', '1'])
        command(['xdotool', 'mousemove', '--window', str(window), '100', '650'])
        pause()

    try:
        if args.expected_sha != HEAD or args.app_pid < 2 or deadline <= start:
            raise Stop('exact_candidate_and_live_deadline_required')
        if not args.isolated_display_capture or not args.private_accessibility_bus or not os.environ.get('DISPLAY'):
            raise Stop('private_QA_display_and_bus_attestation_required')
        runtime, runtime_sha = read_declaration(args.identity_approval)
        ui, ui_sha = read_declaration(args.ui_approval)
        if (runtime.get('schema') != 2 or runtime.get('reviewed_by') != 'main-reviewer'
                or runtime.get('runtime_head') != HEAD or runtime.get('runtime_app_sha256') != APP_SHA
                or runtime.get('change_scope') != 'product-candidate'):
            raise Stop('main_exact_runtime_identity_required')
        validate_ui(ui)
        helper = Path(__file__).with_name('public_probe_11ebf20_ui4.py')
        if (helper.is_symlink() or not helper.is_file()
                or hashlib.sha256(helper.read_bytes()).hexdigest() != ui.get('public_probe_sha256')):
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
                block = stream.read(1048576)
                if not block:
                    break
                digest.update(block)
        if digest.hexdigest() != APP_SHA:
            raise Stop('actual_App_SHA_differs')
        report.update(app_sha256=digest.hexdigest(), identity_declaration_sha256=runtime_sha,
                      UI_declaration_sha256=ui_sha)
        dims = command(['xdotool', 'getdisplaygeometry']).split()
        if len(dims) != 2 or not (1440 <= int(dims[0]) <= 1920 and 900 <= int(dims[1]) <= 1200):
            raise Stop('private_display_dimensions_outside_finite_bounds')
        ids = set(command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)]).split())
        if len(ids) != 1 or (args.window_id is not None and str(args.window_id) not in ids):
            raise Stop('one_explicit_visible_App_window_required')
        window = args.window_id if args.window_id is not None else int(ids.pop())
        evidence = args.output / 'main-qa-11ebf20-workspaces'
        evidence.mkdir(mode=0o700)
        command(['xdotool', 'windowfocus', '--sync', str(window)])
        geometry(1280, 900)
        command(['xdotool', 'mousemove', '--window', str(window), '100', '650'])
        # Keep original first-quick guard; the initial complete frame stays in memory.
        guard(pixels(), 'initial-mode-dialog', ui['mode_guard'])
        ensure_focus()
        report['actions'].append({'kind': 'one_guarded_quick_attempt', 'xy': [558, 500]})
        command(['xdotool', 'mousemove', '--window', str(window), '558', '500'])
        command(['xdotool', 'click', '1'])
        command(['xdotool', 'mousemove', '--window', str(window), '100', '650'])
        pause()
        probe('01-after-quick-prepixels-public')
        after_quick = pixels()
        if rgb_guard_matches(after_quick, ui['mode_guard']):
            raise Stop('reviewed_mode_dialog_still_visible')
        guard(after_quick, 'quick-sidebar')
        snapshot('01-after-quick-1280x900')
        for ordinal, page in (('02', 'clip'), ('06', 'assets')):
            geometry(1280, 900)
            navigate(page, ordinal)
            for index, (width, height) in enumerate(ui['layout_matrix']):
                if size != [width, height]:
                    geometry(width, height)
                snapshot(f'{int(ordinal) + index:02d}-{page}-{width}x{height}')
        geometry(1280, 900)
        navigate('generate', '10')
        probe('10-return-generation-prepixels-public')
        guard(pixels(), 'returned-generation-sidebar')
        snapshot('10-return-generation-1280x900')
        report['status'] = 'finite_observation_completed_review_pending'
    except (Stop, OSError, ValueError, KeyError, TypeError) as exc:
        report['blocking_reason'] = str(exc) if isinstance(exc, Stop) else 'public_runtime_unavailable_raw_error_withheld'
    finally:
        report['elapsed_seconds'] = round(time.monotonic() - start, 3)
        raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        if evidence is not None:
            if sum(p.stat().st_size for p in evidence.iterdir() if p.is_file()) + len(raw) > 15728640:
                report['status'] = 'artifact_budget_exceeded_do_not_publish'
                raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
            with (evidence / 'workspaces.json').open('xb') as stream:
                os.chmod(evidence / 'workspaces.json', 0o600)
                stream.write(raw)
        print(raw.decode(), end='')
    return 0 if report['status'] == 'finite_observation_completed_review_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
