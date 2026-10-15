#!/usr/bin/env python3
"""Finite assets import on one fresh frozen App; launcher owns cleanup <=300s.

No field reads, Settings pixels, external/model calls, retries or arbitrary files.
This standalone controller remains loadable when copied as independent-qa.py.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time

HEAD = '11ebf203e1a78d3b6a21677c4b17e96223c74b0e'
APP_SHA = '8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30'
FIXTURE_SHA = '0928c47fa44250879270def6198e04fd939dd8250864179760203f0d334a6d63'
SCOPE = 'assets-import'
PUBLIC_KEYS = ('path', 'role', 'label', 'showing', 'enabled', 'sensitive', 'focusable',
               'button', 'radio', 'entry', 'editable', 'editable_text_interface',
               'action_interface', 'modal', 'dialog', 'file_chooser', 'bounds', 'allowed_actions')
SETTINGS_LABELS = frozenset(('工作模式', '外观', '主题', '通用', '常规',
                           'General', 'Appearance', 'Theme', 'Work mode'))
SETTINGS_BOUNDS = ({'x': 596, 'y': 147, 'width': 188, 'height': 36},
                   {'x': 596, 'y': 222, 'width': 188, 'height': 36},
                   {'x': 144, 'y': 68, 'width': 48, 'height': 24},
                   {'x': 1216, 'y': 68, 'width': 26, 'height': 128})


class Stop(Exception):
    pass


def declaration(path):
    if not path.is_absolute() or path.is_symlink() or not path.is_file() or path.stat().st_size > 16384:
        raise Stop('explicit_bounded_regular_declaration_required')
    raw = path.read_bytes()
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def public_context(data, pid, allow_dialog=False, native=False):
    nodes = data.get('nodes')
    if (data.get('status') != 'public_metadata_observed' or data.get('app_pid') != pid
            or data.get('coverage_complete') is not True or data.get('field_values_read') is not False
            or data.get('ui_actions') != [] or not isinstance(nodes, list) or not 1 <= len(nodes) <= 512
            or any(not isinstance(n, dict) or not isinstance(n.get('path'), list)
                   or any(type(i) is not int or not 0 <= i < 128 for i in n['path']) for n in nodes)):
        raise Stop('complete_same_PID_nonfield_public_metadata_required')
    if len({tuple(n['path']) for n in nodes}) != len(nodes):
        raise Stop('duplicate_public_path_no_input_or_pixels')
    showing = [n for n in nodes if n.get('showing')]
    if any((n.get('role') == 39 and n.get('label') in SETTINGS_LABELS)
           or n.get('bounds') in SETTINGS_BOUNDS for n in showing):
        raise Stop('Settings_context_no_input_or_pixels')
    if not allow_dialog and any(n.get('dialog') or n.get('modal') or n.get('file_chooser') for n in showing):
        raise Stop('unknown_modal_no_input')
    if native:
        if data.get('toolkit') != 'GTK':
            raise Stop('actual_GTK_metadata_required')
    else:
        windows = [n for n in showing if n.get('role') == 23]
        if (len(windows) != 1 or windows[0].get('path') != [0]
                or windows[0].get('bounds') != {'x': 0, 'y': 0, 'width': 1280, 'height': 900}
                or not all(windows[0].get(k) is True for k in ('enabled', 'sensitive'))):
            raise Stop('current_public_App_window_changed')
    return showing


def projection(nodes):
    return [[n.get(k) for k in PUBLIC_KEYS] for n in nodes if n.get('showing')]


def page_target(data, ui, pid):
    showing = public_context(data, pid)
    if projection(showing) != ui['page_public_nodes']:
        raise Stop('current_full_page_public_metadata_changed_no_input')
    if SCOPE == 'assets-import':
        targets = [n for n in showing if n.get('label') == '导入素材' and n.get('button')]
        if len(targets) != 1 or targets[0]['path'] != [0, 18]:
            raise Stop('current_import_control_unknown_or_ambiguous')
        return targets[0]
    titles = [n for n in showing if n.get('label') == '新建项目' and n.get('role') == 29]
    headings = [n for n in showing if n.get('label') == '剪辑' and n.get('role') == 29]
    if len(titles) != 1 or len(headings) != 1 or titles[0]['path'] != [0, 13]:
        raise Stop('current_clip_heading_or_card_title_unknown')
    return titles[0]


def rgb_matches(raw, guard, width=1280, height=900):
    if len(raw) != width * height * 3:
        return False
    x, y, w, h = guard['region']
    if min(x, y) < 0 or min(w, h) <= 0 or x + w > width or y + h > height:
        return False
    crop = b''.join(raw[(row * width + x) * 3:(row * width + x + w) * 3] for row in range(y, y + h))
    return hashlib.sha256(crop).hexdigest() == guard['rgb_sha256']


def owned_descendant(pid, owner):
    seen = set()
    while pid >= 2 and pid not in seen and len(seen) < 16:
        seen.add(pid)
        proc = Path('/proc') / str(pid)
        if proc.stat().st_uid != os.getuid():
            return False
        if pid == owner:
            return True
        line = next((v for v in (proc / 'status').read_text().splitlines() if v.startswith('PPid:')), None)
        if line is None:
            return False
        pid = int(line.split()[1])
    return False


def validate_ui(ui):
    if (ui.get('schema') != 1 or ui.get('reviewed_by') != 'main-reviewer' or ui.get('scope') != SCOPE
            or ui.get('observed_head') != HEAD or ui.get('observed_app_sha256') != APP_SHA
            or ui.get('window') != [1280, 900] or ui.get('quick_xy') != [558, 500]
            or ui.get('runtime_limits') != {'controller': 120, 'App_and_cleanup': 300, 'reserve': 15}
            or ui.get('artifact_limits') != {'PNG_count': 10, 'PNG_each': 2097152,
                'PNG_total': 14680064, 'all': 15728640, 'metadata_each': 131072}
            or ui.get('mode_guard') != {'region': [420, 388, 520, 155],
                'rgb_sha256': '2e513f04d1897fef7c6e6a0710c7afae9b7d6d592ddf1e316cb7e3fa89eb1b97'}):
        raise Stop('exact_main_scope_declaration_required')
    expected = ('assets', [40, 286], [1180, 86]) if SCOPE == 'assets-import' else ('clip', [40, 228], [290, 251])
    if (ui.get('page'), ui.get('navigation_xy'), ui.get('target_xy')) != expected:
        raise Stop('exact_scope_action_points_required')


def runtime_dependencies(ui):
    name = 'asset-clip-public-probe.py' if SCOPE == 'assets-import' else 'public_probe_11ebf20_ui4.py'
    if ui.get('public_probe_filename') != name:
        raise Stop('exact_scoped_probe_basename_required')
    specs = {'probe': (name, ui.get('public_probe_sha256'))}
    if SCOPE == 'assets-import':
        specs['action'] = ('asset-clip-native-action.py', ui.get('native_action_sha256'))
    result = {}
    for key, (basename, digest) in specs.items():
        path = Path(__file__).with_name(basename)
        if (not isinstance(digest, str) or not re.fullmatch(r'[a-f0-9]{64}', digest)
                or path.is_symlink() or not path.is_file() or path.stat().st_size > 65536
                or path.stat().st_uid != os.getuid() or hashlib.sha256(path.read_bytes()).hexdigest() != digest):
            raise Stop('reviewed_scoped_dependency_required:' + key)
        result[key] = path
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('app-pid', 'window-id'):
        parser.add_argument('--' + name, type=int, required=True)
    for name in ('work-dir', 'output', 'identity-approval', 'ui-approval'):
        parser.add_argument('--' + name, type=Path, required=True)
    if SCOPE == 'assets-import':
        parser.add_argument('--input-dir', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--expected-sha', default=HEAD)
    parser.add_argument('--probe-python', default='/usr/bin/python3')
    parser.add_argument('--isolated-display-capture', action='store_true')
    parser.add_argument('--private-accessibility-bus', action='store_true')
    args = parser.parse_args()
    started = time.monotonic()
    end = min(args.deadline_monotonic - 15, started + 120)
    directory = None
    report = {'schema': 'seecut-asset-clip-finite-v1', 'scope': SCOPE, 'status': 'blocked',
              'source_head': HEAD, 'actions': [], 'captures': [], 'guards': [], 'public_metadata': [],
              'window_observations': [], 'project_created': False, 'file_input_attempted': False,
              'field_values_read': False, 'file_lists_read': False, 'settings_opened': False,
              'settings_pixels_captured': False, 'login_attempted': False, 'model_started': False,
              'external_upload_requested': False, 'sourcecopy_verified': False,
              'product_verdict': 'pending_main_actual_image_review', 'cleanup_owner': 'launcher <=300s'}

    def command(argv, binary=False, search=False):
        left = end - time.monotonic()
        if left < 0.25:
            raise Stop('deadline_no_further_input')
        try:
            result = subprocess.run(argv, capture_output=True, text=not binary, timeout=min(8, left))
        except subprocess.TimeoutExpired:
            raise Stop('owned_UI_command_timeout_no_retry') from None
        if result.returncode and not (search and result.returncode == 1):
            raise Stop('owned_UI_command_failed_no_retry_raw_withheld')
        return result.stdout

    def pause():
        if end - time.monotonic() < 1:
            raise Stop('insufficient_observation_time')
        time.sleep(0.7)

    def descendants():
        found, pending = {args.app_pid}, [args.app_pid]
        while pending:
            pid = pending.pop()
            taskdir = Path('/proc') / str(pid) / 'task'
            tasks = list(taskdir.iterdir())
            if len(tasks) > 64:
                raise Stop('owned_process_task_limit')
            for task in tasks:
                try:
                    children = list(map(int, (task / 'children').read_text().split()))
                except FileNotFoundError:
                    continue
                for child in children:
                    if child not in found and owned_descendant(child, args.app_pid):
                        if len(found) >= 24:
                            raise Stop('owned_descendant_limit')
                        found.add(child)
                        pending.append(child)
        return found

    def native_windows():
        result = []
        for pid in descendants():
            for value in set(command(['xdotool', 'search', '--onlyvisible', '--pid', str(pid)], search=True).split()):
                wid = int(value)
                if wid == args.window_id:
                    continue
                fields = dict(line.split('=', 1) for line in command(
                    ['xdotool', 'getwindowgeometry', '--shell', str(wid)]).splitlines() if '=' in line)
                bounds = [int(fields[k]) for k in ('X', 'Y', 'WIDTH', 'HEIGHT')]
                # No visible owned extra is ignored, even if it is smaller than a chooser.
                if not (pid != args.app_pid and bounds[0] >= 0 and bounds[1] >= 0
                        and bounds[2] >= 1 and bounds[3] >= 1
                        and bounds[0] + bounds[2] <= dims[0] and bounds[1] + bounds[3] <= dims[1]):
                    raise Stop('unknown_owned_extra_window_no_input_or_pixels')
                result.append({'pid': pid, 'window': wid, 'bounds': bounds})
        return sorted(result, key=lambda n: n['window'])

    def focus_main():
        if (not owned_descendant(args.app_pid, args.app_pid) or native_windows()
                or set(command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)], search=True).split()) != {str(args.window_id)}
                or int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id):
            raise Stop('same_owned_App_window_and_focus_required')

    def focus_native(native):
        if native_windows() != [native] or not owned_descendant(native['pid'], args.app_pid):
            raise Stop('exact_current_owned_native_window_required')
        if (int(command(['xdotool', 'getwindowpid', str(native['window'])]).strip()) != native['pid']
                or int(command(['xdotool', 'getwindowfocus']).strip()) != native['window']):
            raise Stop('same_owned_native_PID_and_focus_required')

    def probe(name, native=None, allow_dialog=False):
        if native is None:
            focus_main()
        else:
            focus_native(native)
        pid = native['pid'] if native else args.app_pid
        completed = subprocess.run([args.probe_python, '-B', str(helper), '--app-pid', str(pid),
            '--owned-root-pid', str(args.app_pid), '--output', str(directory), '--output-name', name + '.json',
            '--deadline-monotonic', str(min(end, time.monotonic() + 3)), '--private-accessibility-bus'],
            capture_output=True, timeout=min(5, max(0.25, end - time.monotonic())))
        path = directory / (name + '.json')
        if completed.returncode or path.is_symlink() or not path.is_file() or path.stat().st_size > 131072:
            raise Stop('bounded_public_metadata_unavailable_no_retry')
        data = json.loads(path.read_bytes())
        public_context(data, pid, allow_dialog=allow_dialog, native=native is not None)
        report['public_metadata'].append(path.name)
        return data

    def pixels(native=None):
        focus_native(native) if native else focus_main()
        width, height = native['bounds'][2:] if native else (1280, 900)
        raw = command(['import', '-window', str(native['window'] if native else args.window_id),
                       '-depth', '8', 'rgb:-'], binary=True)
        if len(raw) != width * height * 3:
            raise Stop('actual_RGB_size_changed')
        return raw

    def guard(raw, name, spec=None, width=1280, height=900):
        spec = spec or ui['guards'][name]
        matched = rgb_matches(raw, spec, width, height)
        report['guards'].append({'name': name, 'matched': matched, 'region': spec['region'],
                                 'expected_rgb_sha256': spec['rgb_sha256']})
        if not matched:
            raise Stop('current_RGB_guard_changed_no_input:' + name)

    def snapshot(name, native=None, allow_dialog=False):
        # A fresh complete nonfield metadata proof precedes every saved image.
        probe(name + '-public', native=native, allow_dialog=allow_dialog)
        if len(report['captures']) >= 10:
            raise Stop('PNG_count_limit_before_capture')
        path = directory / (name + '.png')
        if path.exists():
            raise Stop('fresh_flat_capture_path_required')
        focus_native(native) if native else focus_main()
        command(['import', '-window', str(native['window'] if native else args.window_id), '-strip', str(path)])
        path.chmod(0o600)
        raw = path.read_bytes()
        if (len(raw) > 2097152 or sum(n['bytes'] for n in report['captures']) + len(raw) > 14680064
                or sum(p.stat().st_size for p in directory.iterdir() if p.is_file()) + 16384 > 15728640):
            path.unlink()  # Only this controller's newly created over-budget PNG.
            raise Stop('PNG_budget_exceeded_do_not_publish')
        report['captures'].append({'file': path.name, 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(),
                                  'surface': 'owned-native-window' if native else 'owned-App-window'})

    def click(xy, name):
        focus_main()
        report['actions'].append({'kind': 'one_guarded_click_attempt', 'target': name, 'xy': xy})
        command(['xdotool', 'mousemove', '--window', str(args.window_id), *map(str, xy)])
        focus_main()
        command(['xdotool', 'click', '1'])
        # This moves only the pointer; it cannot send another click to a late native.
        command(['xdotool', 'mousemove', '--window', str(args.window_id), '100', '650'])
        pause()

    def collect_native():
        until = min(end, time.monotonic() + 12)
        previous, stable = None, None
        while time.monotonic() < until:
            found = native_windows()
            if len(found) > 1:
                raise Stop('multiple_owned_native_windows_no_input')
            if found:
                now = found[0]
                if now == previous and time.monotonic() - stable >= 0.8:
                    command(['xdotool', 'windowfocus', '--sync', str(now['window'])])
                    focus_native(now)
                    report['window_observations'].append(now)
                    snapshot('03-native-observed', native=now, allow_dialog=True)
                    return now
                if now != previous:
                    previous, stable = now, time.monotonic()
            else:
                previous, stable = None, None
            time.sleep(0.25)
        # No native: current synthetic App surface only; observe unknown modal, no inputs.
        command(['xdotool', 'windowfocus', '--sync', str(args.window_id)])
        snapshot('03-app-unknown-result', allow_dialog=True)
        raise Stop('no_unique_stable_owned_native_return_to_main_review')

    def chooser_nodes(data):
        showing = public_context(data, data['app_pid'], allow_dialog=True, native=True)
        roots = [n for n in showing if n.get('dialog') and n.get('path') == [0]]
        accept = [n for n in showing if n.get('button') and n.get('label') in ('OK', 'Ok', 'Open', '打开')
                  and n.get('action_interface') and (n.get('enabled') or n.get('sensitive'))
                  and n.get('allowed_actions') == ['click']]
        cancel = [n for n in showing if n.get('button') and n.get('label') in ('Cancel', '取消')
                  and n.get('action_interface') and (n.get('enabled') or n.get('sensitive'))
                  and n.get('allowed_actions') == ['click']]
        if len(roots) != 1 or len(accept) != 1 or len(cancel) != 1:
            raise Stop('unknown_native_metadata_main_review_no_input')
        return showing, accept[0]

    def native_action(native, node, mode):
        focus_native(native)
        report['actions'].append({'kind': 'one_owned_public_native_attempt', 'mode': mode, 'node_path': node['path']})
        completed = subprocess.run([args.probe_python, '-B', str(action), '--target-pid', str(native['pid']),
            '--owned-root-pid', str(args.app_pid), '--window-id', str(native['window']),
            '--node-public', json.dumps(node, separators=(',', ':')), '--window-bounds', json.dumps(native['bounds']),
            '--owned-path', str(fixture), '--input-dir', str(args.input_dir), '--ui-approval', str(args.ui_approval),
            '--mode', mode, '--deadline-monotonic', str(min(end, time.monotonic() + 3)), '--private-accessibility-bus'],
            capture_output=True, timeout=min(5, max(0.25, end - time.monotonic())))
        if len(completed.stdout) > 8192:
            raise Stop('bounded_native_action_report_required')
        data = json.loads(completed.stdout)
        path = directory / ('05-native-' + mode + '-action.json')
        raw = (json.dumps(data, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        with path.open('xb') as stream:
            path.chmod(0o600)
            stream.write(raw)
        if completed.returncode or data.get('success') is not True or data.get('field_values_read') is not False:
            raise Stop('native_action_unconfirmed_no_retry')

    def imported_files():
        for path in (args.work_dir / 'portable', args.work_dir / 'portable' / 'personal-library',
                     args.work_dir / 'portable' / 'personal-library' / 'imported'):
            if path.is_symlink():
                raise Stop('owned_managed_sourcecopy_directory_symlink_forbidden')
            if not path.exists():
                return []
            if not path.is_dir() or path.stat().st_uid != os.getuid():
                raise Stop('owned_managed_sourcecopy_directory_required')
        files = list(path.iterdir())
        if len(files) > 1:
            raise Stop('only_one_new_managed_fixture_copy_allowed')
        for file in files:
            if (file.is_symlink() or not file.is_file() or file.stat().st_uid != os.getuid()
                    or not re.fullmatch(r'[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}\.png', file.name)
                    or file.stat().st_size != 800):
                raise Stop('bounded_owned_managed_PNG_only')
        return files

    try:
        if (args.expected_sha != HEAD or args.app_pid < 2 or args.window_id < 1 or end <= started
                or not args.isolated_display_capture or not args.private_accessibility_bus or not os.environ.get('DISPLAY')):
            raise Stop('exact_private_scope_identity_and_deadline_required')
        runtime, runtime_sha = declaration(args.identity_approval)
        ui, ui_sha = declaration(args.ui_approval)
        validate_ui(ui)
        if (runtime.get('schema') != 2 or runtime.get('reviewed_by') != 'main-reviewer'
                or runtime.get('runtime_head') != HEAD or runtime.get('runtime_app_sha256') != APP_SHA
                or runtime.get('change_scope') != 'product-candidate'):
            raise Stop('main_exact_runtime_identity_required')
        for path in (args.work_dir, args.output):
            if not path.is_absolute() or path.is_symlink() or not path.is_dir() or path.stat().st_uid != os.getuid():
                raise Stop('explicit_owned_isolated_directory_required')
        proc = Path('/proc') / str(args.app_pid)
        if proc.stat().st_uid != os.getuid() or (proc / 'exe').resolve().parent != args.work_dir:
            raise Stop('owned_fresh_launcher_App_path_required')
        digest = hashlib.sha256()
        with (proc / 'exe').open('rb') as stream:
            while True:
                if time.monotonic() >= end:
                    raise Stop('deadline_during_binary_verification')
                block = stream.read(1048576)
                if not block:
                    break
                digest.update(block)
        if digest.hexdigest() != APP_SHA:
            raise Stop('actual_App_SHA_differs')
        report.update(app_sha256=digest.hexdigest(), identity_sha256=runtime_sha, ui_sha256=ui_sha)
        dependencies = runtime_dependencies(ui)
        helper = dependencies['probe']
        dims = list(map(int, command(['xdotool', 'getdisplaygeometry']).split()))
        if len(dims) != 2 or not (1440 <= dims[0] <= 1920 and 900 <= dims[1] <= 1200):
            raise Stop('private_display_dimensions_outside_finite_bounds')
        if SCOPE == 'assets-import':
            if ui['public_probe_filename'] != 'asset-clip-public-probe.py':
                raise Stop('assets_closed_label_probe_required')
            if (not args.input_dir.is_absolute() or args.input_dir != args.work_dir / 'asset-clip-inputs'
                    or args.input_dir.is_symlink() or not args.input_dir.is_dir() or args.input_dir.stat().st_uid != os.getuid()):
                raise Stop('exact_launcher_fixture_directory_required')
            fixture = args.input_dir / 'opaque-quadrants.png'
            if (fixture.is_symlink() or not fixture.is_file() or fixture.stat().st_size != 800
                    or fixture.stat().st_uid != os.getuid() or set(p.name for p in args.input_dir.iterdir()) != {'opaque-quadrants.png'}
                    or hashlib.sha256(fixture.read_bytes()).hexdigest() != FIXTURE_SHA):
                raise Stop('exact_single_owned_fixture_required')
            action = dependencies['action']
            portable = args.work_dir / 'portable'
            if portable.is_symlink() or not portable.is_dir() or portable.stat().st_uid != os.getuid():
                raise Stop('fresh_owned_portable_directory_required')
            if imported_files():
                raise Stop('fresh_empty_owned_library_imports_required')
        elif ui['public_probe_filename'] != 'public_probe_11ebf20_ui4.py':
            raise Stop('clip_existing_nonfield_probe_required')
        fresh_directory = args.output / ('main-qa-asset-clip-' + SCOPE)
        fresh_directory.mkdir(mode=0o700)
        directory = fresh_directory
        command(['xdotool', 'windowfocus', '--sync', str(args.window_id)])
        focus_main()
        command(['xdotool', 'windowsize', '--sync', str(args.window_id), '1280', '900'])
        pause()
        fields = dict(line.split('=', 1) for line in command(
            ['xdotool', 'getwindowgeometry', '--shell', str(args.window_id)]).splitlines() if '=' in line)
        if [int(fields[k]) for k in ('X', 'Y', 'WIDTH', 'HEIGHT')] != [0, 0, 1280, 900]:
            raise Stop('actual_App_geometry_changed')
        command(['xdotool', 'mousemove', '--window', str(args.window_id), '100', '650'])
        guard(pixels(), 'initial-mode', ui['mode_guard'])
        click([558, 500], 'quick-mode')
        probe('01-after-quick-prepixels-public')
        guard(pixels(), 'quick-sidebar')
        snapshot('01-after-quick')
        before = probe('02-before-navigation-public')
        for name, expected in ui['navigation'].items():
            matches = [n for n in before['nodes'] if n.get('showing') and n.get('label') == expected['label'] and n.get('role') == 43]
            if len(matches) != 1 or any(matches[0].get(k) != expected.get(k) for k in PUBLIC_KEYS):
                raise Stop('current_navigation_metadata_changed')
        guard(pixels(), 'nav-target')
        click(ui['navigation_xy'], ui['page'])
        snapshot('02-page')
        page_target(probe('02-page-target-public'), ui, args.app_pid)
        frame = pixels()
        for name in ui['page_guard_names']:
            guard(frame, name)
        # Fresh complete metadata and same current RGB immediately before the single target click.
        page_target(probe('02-target-recheck-public'), ui, args.app_pid)
        frame = pixels()
        for name in ui['page_guard_names']:
            guard(frame, name)
        click(ui['target_xy'], 'assets-import' if SCOPE == 'assets-import' else 'new-clip-card')
        if SCOPE == 'clip-create-observation':
            # Unknown actual dialog is the result. No field/name/path/create input is authorized.
            snapshot('03-new-clip-dialog-observed', allow_dialog=True)
            report['status'] = 'new_clip_dialog_observed_main_review_required'
        else:
            native = collect_native()
            data = probe('04-native-before-public', native=native, allow_dialog=True)
            nodes, _accept = chooser_nodes(data)
            width, height = native['bounds'][2:]
            guard(pixels(native), 'native-footer', {'region': [width - 157, height - 46, 145, 34],
                  'rgb_sha256': '4ca359d14fbaec11a20a908116d131c055db707227550009603c0a832d8035ef'}, width, height)
            before_entries = {tuple(n['path']) for n in nodes if n.get('entry') and n.get('editable_text_interface')}
            focus_native(native)
            report['actions'].append({'kind': 'one_location_popup_attempt', 'key': 'ctrl+l'})
            command(['xdotool', 'key', '--clearmodifiers', 'ctrl+l'])
            pause()
            snapshot('04-native-location', native=native, allow_dialog=True)
            data = probe('05-native-location-public', native=native, allow_dialog=True)
            nodes, _accept = chooser_nodes(data)
            entries = [n for n in nodes if n.get('entry') and n.get('editable_text_interface')
                       and (n.get('enabled') or n.get('sensitive')) and n.get('focused')
                       and (tuple(n['path']) not in before_entries or n.get('focused'))]
            if len(entries) != 1:
                raise Stop('unique_current_focused_GTK_location_required')
            report['file_input_attempted'] = True
            native_action(native, entries[0], 'set-location')
            pause()
            data = probe('05-native-before-accept-public', native=native, allow_dialog=True)
            _nodes, accept = chooser_nodes(data)
            native_action(native, accept, 'accept')
            until = min(end, time.monotonic() + 12)
            while native_windows() and time.monotonic() < until:
                time.sleep(0.25)
            if native_windows():
                snapshot('06-native-still-visible', native=native, allow_dialog=True)
                raise Stop('native_not_gone_main_review_no_retry')
            command(['xdotool', 'windowfocus', '--sync', str(args.window_id)])
            focus_main()
            pause()
            if hashlib.sha256(fixture.read_bytes()).hexdigest() != FIXTURE_SHA:
                raise Stop('fixture_changed_during_import')
            copy_until = min(end, time.monotonic() + 12)
            files = imported_files()
            while not files and time.monotonic() < copy_until:
                time.sleep(0.25)
                focus_main()
                files = imported_files()
            if len(files) == 1:
                first = files[0].read_bytes()
                pause()
                second_files = imported_files()
                if (second_files != files or first != files[0].read_bytes()
                        or hashlib.sha256(first).hexdigest() != FIXTURE_SHA):
                    raise Stop('managed_fixture_copy_not_stable_or_exact')
                report.update(sourcecopy_verified=True, sourcecopy_bytes=800, sourcecopy_sha256=FIXTURE_SHA,
                              sourcecopy_path_scope='fresh portable/personal-library/imported only')
            snapshot('06-assets-import-result', allow_dialog=True)
            report['status'] = 'asset_import_result_observed_main_review_required'
    except Stop as exc:
        report['blocking_reason'] = str(exc)
    except subprocess.TimeoutExpired:
        report['blocking_reason'] = 'owned_helper_timeout_no_retry'
    except Exception:
        report['blocking_reason'] = 'unexpected_owned_runtime_error_raw_withheld'
    finally:
        report['elapsed_seconds'] = round(time.monotonic() - started, 3)
        raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        if directory is not None:
            files = list(directory.iterdir())
            if any(p.is_symlink() or not p.is_file() for p in files) or sum(p.stat().st_size for p in files) + len(raw) > 15728640:
                report['status'] = 'artifact_budget_exceeded_do_not_publish'
                raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
            if len(raw) <= 16384:
                path = directory / 'asset-clip-report.json'
                with path.open('xb') as stream:
                    path.chmod(0o600)
                    stream.write(raw)
        print(json.dumps({k: report[k] for k in ('scope', 'status', 'blocking_reason', 'sourcecopy_verified') if k in report}))
    return 0 if report['status'] in ('new_clip_dialog_observed_main_review_required',
                                    'asset_import_result_observed_main_review_required') else 2


if __name__ == '__main__':
    raise SystemExit(main())
