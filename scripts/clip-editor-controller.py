#!/usr/bin/env python3
"""Finite synthetic clip project creation and empty-editor observation; launcher owns cleanup <=300s.

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
SCOPE = 'clip-editor-entry'
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


def public_context(data, pid, allow_dialog=False, native=False, width=1280, height=900):
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
                or windows[0].get('bounds') != {'x': 0, 'y': 0, 'width': width, 'height': height}
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


def owned_project_output(work, fresh=False):
    if not work.is_absolute() or '..' in work.parts:
        raise Stop('absolute_launcher_work_directory_required')
    for parent in (work, *work.parents):
        if parent.is_symlink() or not parent.is_dir():
            raise Stop('real_launcher_directory_ancestors_required')
    if work.stat().st_uid != os.getuid():
        raise Stop('owned_launcher_work_directory_required')
    path = work / 'clip-project-output'
    if fresh:
        if path.exists() or path.is_symlink():
            raise Stop('new_owned_clip_output_directory_required')
        path.mkdir(mode=0o700)
    if (path.is_symlink() or not path.is_dir() or path.stat().st_uid != os.getuid()
            or path.stat().st_mode & 0o777 != 0o700):
        raise Stop('exact_owned_0700_clip_output_required')
    return path


def dialog_projection(nodes):
    # Observed root WINDOW drops FOCUSABLE when an entry takes focus. Only this
    # boolean may vary; field focusability and every other public column stay exact.
    rows = projection(nodes)
    for row in rows:
        if row[0] == [0] and row[1] == 23:
            index = PUBLIC_KEYS.index('focusable')
            if type(row[index]) is not bool:
                raise Stop('actual_dialog_window_focusable_boolean_required')
            row[index] = True
    # The same observed toast moves down 12px while its animation settles.
    # Normalize only this complete group within the source-defined 0..12px animation.
    toast = {(0, 27): (1004, 852, 188, 32), (0, 28): (1206, 858, 52, 20),
             (0, 28, 0): (1206, 858, 1, 1), (0, 28, 1): (1220, 863, 24, 10)}
    group = {tuple(row[0]): row for row in rows if tuple(row[0]) in toast}
    bounds_index = PUBLIC_KEYS.index('bounds')
    if len(group) == 4 and all(
            isinstance(group[path][bounds_index], dict)
            and type(group[path][bounds_index].get('y')) is int
            and y <= group[path][bounds_index]['y'] <= y + 12
            and group[path][bounds_index] == dict(zip(('x', 'y', 'width', 'height'),
                (x, group[path][bounds_index]['y'], w, h)))
            for path, (x, y, w, h) in toast.items()):
        for path, bounds in toast.items():
            group[path][bounds_index] = dict(zip(('x', 'y', 'width', 'height'), bounds))
    return rows


def dialog_target(data, ui, pid, intent, focused=False):
    showing = public_context(data, pid, allow_dialog=True)
    if dialog_projection(showing) != ui['dialog_public_nodes']:
        raise Stop('complete_current_clip_dialog_metadata_changed')
    if intent not in ('name', 'path', 'create'):
        raise Stop('exact_clip_dialog_intent_required')
    expected = ui['dialog_targets'][intent]
    found = [n for n in showing if n['path'] == expected['path']]
    if len(found) != 1 or any(found[0].get(k) != expected[k] for k in PUBLIC_KEYS):
        raise Stop('current_clip_dialog_target_changed')
    node = found[0]
    if intent in ('name', 'path'):
        if (node.get('role') != 79 or not all(node.get(k) for k in ('entry', 'editable', 'focusable'))
                or node.get('editable_text_interface') is not False or node.get('label') is not None):
            raise Stop('actual_nonfield_entry_semantics_required')
        if focused:
            focus = [n for n in showing if n.get('focused') and n.get('editable') and n.get('entry')]
            if len(focus) != 1 or focus[0]['path'] != expected['path']:
                raise Stop('exact_unique_current_focused_field_required')
    elif (node.get('label') != '创建' or not node.get('button') or node.get('allowed_actions') != ['click']):
        raise Stop('one_current_create_Action_required')
    return node


def masked_dialog_matches(raw, ui):
    if len(raw) != 1280 * 900 * 3:
        return False
    masked = bytearray(raw)
    for x, y, w, h in ui['dialog_field_masks']:
        for row in range(y, y + h):
            masked[(row * 1280 + x) * 3:(row * 1280 + x + w) * 3] = bytes(w * 3)
    return rgb_matches(bytes(masked), ui['dialog_chrome_guard'])


def editor_context(data, pid, width=1280, height=900):
    showing = public_context(data, pid, allow_dialog=True, width=width, height=height)
    labels = {n.get('label') for n in showing}
    return ({'Media', 'Preview', 'Nothing under the playhead'}.issubset(labels)
            and not labels & {'新建剪辑项目', '项目名称', '保存位置', '创建', '取消', 'Cancel'}
            and not any(n.get('dialog') or n.get('modal') or n.get('file_chooser') for n in showing))


def project_manifest_record(work):
    base = owned_project_output(work)
    project = base / 'qa-clip-project'
    path = project / 'concat.json'
    if not project.exists() or not path.exists():
        return {'status': 'not_observed', 'product_success': False}
    if (project.is_symlink() or not project.is_dir() or project.stat().st_uid != os.getuid()
            or path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid()
            or path.stat().st_nlink != 1 or not 0 < path.stat().st_size <= 65536):
        raise Stop('bounded_owned_project_manifest_required')
    before = path.stat()
    raw = path.read_bytes()
    after = path.stat()
    if (before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_ino, after.st_size, after.st_mtime_ns):
        raise Stop('owned_project_manifest_changed_during_read')
    return {'status': 'owned_manifest_bytes_observed', 'relative_path': 'qa-clip-project/concat.json',
            'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(), 'contents_parsed': False,
            'product_success': False}


def runtime_dependencies(ui):
    specs = {'probe': ('clip-editor-public-probe.py', ui.get('public_probe_sha256')),
             'action': ('clip-editor-action.py', ui.get('action_sha256')),
             'controller_alias': ('clip-editor-controller.py', ui.get('controller_sha256'))}
    if ui.get('public_probe_filename') != 'clip-editor-public-probe.py':
        raise Stop('exact_clip_editor_probe_basename_required')
    result = {}
    for key, (basename, digest) in specs.items():
        path = Path(__file__).with_name(basename)
        if (not isinstance(digest, str) or not re.fullmatch(r'[a-f0-9]{64}', digest)
                or path.is_symlink() or not path.is_file() or path.stat().st_size > 65536
                or path.stat().st_uid != os.getuid() or hashlib.sha256(path.read_bytes()).hexdigest() != digest):
            raise Stop('reviewed_scoped_dependency_required:' + key)
        result[key] = path
    if hashlib.sha256(Path(__file__).read_bytes()).hexdigest() != ui.get('controller_sha256'):
        raise Stop('QA_and_named_controller_alias_must_have_identical_SHA')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('app-pid', 'window-id'):
        parser.add_argument('--' + name, type=int, required=True)
    for name in ('work-dir', 'output', 'identity-approval', 'ui-approval'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--expected-sha', default=HEAD)
    parser.add_argument('--probe-python', default='/usr/bin/python3')
    parser.add_argument('--isolated-display-capture', action='store_true')
    parser.add_argument('--private-accessibility-bus', action='store_true')
    args = parser.parse_args()
    started = time.monotonic()
    end = min(args.deadline_monotonic - 15, started + 120)
    directory = None
    size = [1280, 900]
    report = {'schema': 'seecut-clip-editor-entry-v1', 'scope': SCOPE, 'status': 'blocked',
              'source_head': HEAD, 'actions': [], 'captures': [], 'guards': [], 'public_metadata': [],
              'window_observations': [], 'project_created': False, 'project_create_attempted': False,
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


    def probe(name, native=None, allow_dialog=False):
        if native is None:
            focus_main()
        else:
            raise Stop('native_windows_outside_clip_editor_scope')
        pid = args.app_pid
        completed = subprocess.run([args.probe_python, '-B', str(helper), '--app-pid', str(pid),
            '--owned-root-pid', str(args.app_pid), '--output', str(directory), '--output-name', name + '.json',
            '--deadline-monotonic', str(min(end, time.monotonic() + 3)), '--private-accessibility-bus'],
            capture_output=True, timeout=min(5, max(0.25, end - time.monotonic())))
        path = directory / (name + '.json')
        if completed.returncode or path.is_symlink() or not path.is_file() or path.stat().st_size > 131072:
            raise Stop('bounded_public_metadata_unavailable_no_retry')
        if sum(p.stat().st_size for p in directory.iterdir() if p.is_file()) + 16384 > 15728640:
            path.unlink()
            raise Stop('metadata_total_budget_exceeded_no_further_capture')
        data = json.loads(path.read_bytes())
        public_context(data, pid, allow_dialog=allow_dialog, native=native is not None, width=size[0], height=size[1])
        report['public_metadata'].append(path.name)
        return data

    def pixels(native=None):
        focus_main()
        width, height = native['bounds'][2:] if native else size
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
        focus_main()
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





    def action_attempt(intent, filename):
        focus_main()
        report['actions'].append({'kind': 'one_clip_dialog_input_attempt', 'intent': intent})
        completed = subprocess.run([args.probe_python, '-B', str(dependencies['action']),
            '--app-pid', str(args.app_pid), '--window-id', str(args.window_id), '--work-dir', str(args.work_dir),
            '--ui-approval', str(args.ui_approval), '--intent', intent,
            '--deadline-monotonic', str(min(end, time.monotonic() + 10)), '--private-accessibility-bus'],
            capture_output=True, timeout=min(11, max(0.25, end - time.monotonic())))
        if len(completed.stdout) > 8192:
            raise Stop('bounded_clip_action_report_required')
        data = json.loads(completed.stdout)
        raw = (json.dumps(data, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        with (directory / filename).open('xb') as stream:
            os.chmod(directory / filename, 0o600)
            stream.write(raw)
        if completed.returncode or data.get('success') is not True or data.get('field_values_read') is not False:
            snapshot('11-unknown-editor-result', allow_dialog=True)
            raise Stop('clip_dialog_input_unconfirmed_no_retry')
        pause()

    def await_editor():
        until = min(end, time.monotonic() + 12)
        prior = None
        for sample in range(1, 9):
            if time.monotonic() >= until:
                break
            data = probe(f'07-editor-sample-{sample:02d}-public', allow_dialog=True)
            if editor_context(data, args.app_pid):
                current = (projection(data['nodes']), hashlib.sha256(pixels()).hexdigest())
                if current == prior:
                    report['editor_stable_observed'] = True
                    return
                prior = current
            else:
                prior = None
                labels = {n.get('label') for n in data['nodes'] if n.get('showing')}
                if '新建剪辑项目' not in labels:
                    snapshot('11-unknown-editor-result', allow_dialog=True)
                    raise Stop('unrecognized_creation_result_main_review_required')
            pause()
        snapshot('11-unknown-editor-result', allow_dialog=True)
        raise Stop('editor_stability_not_established_main_review_required')

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
        project_output = owned_project_output(args.work_dir, fresh=True)
        fresh_directory = args.output / 'main-qa-clip-editor-entry'
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
        snapshot('03-current-new-clip-dialog', allow_dialog=True)
        initial = probe('03-dialog-recheck-public', allow_dialog=True)
        dialog_target(initial, ui, args.app_pid, 'name')
        guard(pixels(), 'initial-dialog', ui['dialog_guard'])
        action_attempt('name', '04-name-action.json')
        action_attempt('path', '05-path-action.json')
        current = probe('06-before-create-public', allow_dialog=True)
        dialog_target(current, ui, args.app_pid, 'create')
        if not masked_dialog_matches(pixels(), ui):
            raise Stop('current_dialog_chrome_changed_before_create')
        action_attempt('create', '06-create-action.json')
        report['project_create_attempted'] = True
        await_editor()
        report['project_manifest'] = project_manifest_record(args.work_dir)
        for ordinal, (width, height) in enumerate(((1280, 900), (1024, 900), (1440, 900), (1280, 720)), 7):
            if size != [width, height]:
                focus_main()
                command(['xdotool', 'windowsize', '--sync', str(args.window_id), str(width), str(height)])
                pause()
                fields = dict(line.split('=', 1) for line in command(
                    ['xdotool', 'getwindowgeometry', '--shell', str(args.window_id)]).splitlines() if '=' in line)
                if [int(fields[k]) for k in ('X', 'Y', 'WIDTH', 'HEIGHT')] != [0, 0, width, height]:
                    raise Stop('actual_editor_resize_geometry_changed')
                size[:] = [width, height]
            data = probe(f'{ordinal:02d}-editor-prepixels-public', allow_dialog=True)
            if not editor_context(data, args.app_pid, *size):
                snapshot('11-unknown-editor-result', allow_dialog=True)
                raise Stop('unknown_editor_or_dialog_main_image_review_required')
            snapshot(f'{ordinal:02d}-editor-{width}x{height}', allow_dialog=True)
        report['status'] = 'empty_clip_editor_observed_main_review_required'

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
                path = directory / 'clip-editor-report.json'
                with path.open('xb') as stream:
                    path.chmod(0o600)
                    stream.write(raw)
        print(json.dumps({k: report[k] for k in ('scope', 'status', 'blocking_reason', 'sourcecopy_verified') if k in report}))
    return 0 if report['status'] == 'empty_clip_editor_observed_main_review_required' else 2


if __name__ == '__main__':
    raise SystemExit(main())
