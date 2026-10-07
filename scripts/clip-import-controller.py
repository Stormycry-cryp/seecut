#!/usr/bin/env python3
"""Finite synthetic clip project creation and media-import observation; launcher owns cleanup <=300s.

No field reads, Settings pixels, external/model calls, retries or arbitrary files.
This standalone controller remains loadable when copied as independent-qa.py.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import time

HEAD = '0aa9406247e53f073c0b4df686adc68b40e2f8f6'
APP_SHA = '03fb1752c34adb0c0f4a21521c2302ee80c9b23203728cbcf636d3a4a57eaea2'
GUARD_HEAD = '11ebf203e1a78d3b6a21677c4b17e96223c74b0e'
GUARD_APP_SHA = '8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30'
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


def compact_guard_records(records):
    """Lossless success-only exact-dictionary grouping; failures stay separate."""
    result, seen = [], {}
    for index, record in enumerate(records):
        key = json.dumps(record, ensure_ascii=False, sort_keys=True, separators=(',', ':'))
        if record.get('matched') is True and key in seen:
            target = result[seen[key]]
            target['count'] += 1
            target['occurrence_indices'].append(index)
        else:
            if record.get('matched') is True:
                seen[key] = len(result)
            result.append(dict(record, count=1, occurrence_indices=[index]))
    return result


def bounded_report_bytes(report):
    report['guards'] = compact_guard_records(report['guards'])
    raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
    if len(raw) <= 16384:
        return raw
    original_status, original_reason = report['status'], report.get('blocking_reason')
    report['status'] = 'blocked'
    report['blocking_reason'] = 'report_16KiB_exceeded_incomplete_evidence'
    return (json.dumps({'schema': report['schema'], 'scope': report['scope'],
        'status': 'blocked', 'blocking_reason': 'report_16KiB_exceeded_incomplete_evidence',
        'complete_report_saved': False, 'original_status': original_status,
        'original_blocking_reason': original_reason, 'source_head': report['source_head'],
        'action_count': len(report['actions']), 'capture_count': len(report['captures']),
        'public_metadata_count': len(report['public_metadata']),
        'guard_count': sum(g['count'] for g in report['guards']),
        'field_values_read': report['field_values_read'],
        'settings_pixels_captured': report['settings_pixels_captured']},
        ensure_ascii=False, separators=(',', ':')) + '\n').encode()


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
            or ui.get('observed_head') != GUARD_HEAD or ui.get('observed_app_sha256') != GUARD_APP_SHA
            or ui.get('runtime_head') != HEAD or ui.get('runtime_app_sha256') != APP_SHA
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
    rows = projection(nodes)
    for row in rows:
        if row[0] == [0] and row[1] == 23:
            index = PUBLIC_KEYS.index('focusable')
            if type(row[index]) is not bool:
                raise Stop('actual_dialog_window_focusable_boolean_required')
            row[index] = True
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
             'controller_alias': ('clip-editor-controller.py', ui.get('controller_sha256')),
             'post_insert': ('clip-post-insert.py', ui.get('post_insert_sha256'))}
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
    for key, basename, pin in (('import_guard', 'clip-import-guard.json', 'import_guard_sha256'),
                               ('native_action', 'clip-import-native-action.py', 'native_action_sha256'),
                               ('timeline_guard', 'clip-timeline-guard.json', 'timeline_guard_sha256')):
        path = Path(__file__).with_name(basename)
        if (path.is_symlink() or not path.is_file() or path.stat().st_size > 65536
                or path.stat().st_uid != os.getuid() or hashlib.sha256(path.read_bytes()).hexdigest() != ui.get(pin)):
            raise Stop('reviewed_import_dependency_required:' + key)
        result[key] = path
    return result


IMPORT_COLUMNS = PUBLIC_KEYS + ('focused', 'selected', 'checked', 'pressed')


TIMELINE_COLUMNS = ('action_interface', 'allowed_actions', 'bounds', 'button', 'checked', 'dialog', 'editable', 'editable_text_interface', 'enabled', 'entry', 'file_chooser', 'focusable', 'focused', 'label', 'modal', 'panel', 'path', 'radio', 'role', 'selected', 'sensitive', 'showing')


def timeline_rows(nodes):
    required = set(TIMELINE_COLUMNS) - {'bounds', 'allowed_actions'}
    booleans = required - {'path', 'role', 'label'}
    if not isinstance(nodes, list) or not 1 <= len(nodes) <= 512:
        raise Stop('invalid_timeline_public_node_schema_no_input')
    for node in nodes:
        if (not isinstance(node, dict) or set(node) - set(TIMELINE_COLUMNS)
                or not required.issubset(node)
                or any(type(node[k]) is not bool for k in booleans)
                or type(node['role']) is not int or node['role'] < 0
                or not isinstance(node['path'], list) or len(node['path']) > 24
                or any(type(i) is not int or not 0 <= i < 128 for i in node['path'])
                or (node['label'] is not None and not isinstance(node['label'], str))
                or (any(node[k] for k in ('entry', 'editable', 'editable_text_interface'))
                    and node['label'] is not None)):
            raise Stop('invalid_timeline_public_node_schema_no_input')
        if 'bounds' in node:
            bounds = node['bounds']
            if (not isinstance(bounds, dict) or set(bounds) != {'x', 'y', 'width', 'height'}
                    or any(type(v) is not int for v in bounds.values())
                    or bounds['width'] < 0 or bounds['height'] < 0):
                raise Stop('invalid_timeline_public_node_schema_no_input')
        if ('allowed_actions' in node) != ((node['button'] or node['radio']) and node['action_interface']):
            raise Stop('invalid_timeline_public_node_schema_no_input')
        if 'allowed_actions' in node:
            actions = node['allowed_actions']
            if (not isinstance(actions, list) or len(actions) > 8
                    or any(a not in ('click', 'activate', 'press') for a in actions)
                    or not (node['button'] or node['radio']) or not node['action_interface']):
                raise Stop('invalid_timeline_public_node_schema_no_input')
    return [[n[k] if k in n else {'absent': True} for k in TIMELINE_COLUMNS] for n in nodes]


def timeline_canonical(rows):
    return json.dumps(rows, ensure_ascii=False, sort_keys=True, separators=(',', ':'))


def imported_projection(nodes, ui):
    rows = timeline_rows(nodes)
    if len(rows) == 114 and nodes[-1]['path'] == [0, 108]:
        bounds = nodes[-1].get('bounds')
        if bounds is not None and type(bounds['y']) is int and 852 <= bounds['y'] <= 864:
            adjusted = list(rows)
            adjusted[-1] = list(rows[-1])
            adjusted[-1][TIMELINE_COLUMNS.index('bounds')] = dict(bounds, y=852)
            if timeline_canonical(adjusted) == timeline_canonical(ui['imported_rows']):
                return adjusted
    return rows


def validate_timeline_ui(ui):
    if (set(ui) != {'schema', 'scope', 'source_head', 'columns', 'transient_read_only_rows',
                   'imported_rows', 'thumbnail_guard', 'caption_guard', 'target_path',
                   'target_bounds', 'double_click_xy', 'observed_113_read_only_sha256'} or ui['schema'] != 1
            or ui['scope'] != 'clip-media-import' or ui['source_head'] != HEAD
            or ui['columns'] != list(TIMELINE_COLUMNS)
            or len(ui['transient_read_only_rows']) != 111 or len(ui['imported_rows']) != 114
            or ui['observed_113_read_only_sha256'] != 'e9c8a432771528099e10dc644959cbabb61c7146c9f9785a429a527bb009ba0e'
            or ui['target_path'] != [0, 28]
            or ui['target_bounds'] != {'x': 201, 'y': 182, 'width': 117, 'height': 66}
            or ui['double_click_xy'] != [259, 215]
            or ui['thumbnail_guard'] != {'region': [201, 182, 117, 66],
                'rgb_sha256': '93e23aef4022c4a98d310cbf1d3600d9912a757ab9acc9c397d279362cf00701'}
            or ui['caption_guard'] != {'region': [201, 252, 117, 14],
                'rgb_sha256': '9fd1cbfa63a215843fbea2d163e84900ef08af0d73c143b9a1383a219fc85bf6'}):
        raise Stop('exact_observed_timeline_declaration_required')


def imported_state(data, ui, pid):
    public_context(data, pid)
    current = timeline_canonical(imported_projection(data['nodes'], ui))
    if (current == timeline_canonical(ui['transient_read_only_rows'])
            or (len(data['nodes']) == 113 and
                hashlib.sha256(current.encode()).hexdigest() == ui['observed_113_read_only_sha256'])):
        return 'observed_transient_read_only'
    if current != timeline_canonical(ui['imported_rows']):
        raise Stop('unknown_import_result_main_review_required')
    target = [n for n in data['nodes'] if n['path'] == ui['target_path']]
    images = [n for n in data['nodes'] if n.get('role') == 27 and n.get('showing')
              and n.get('bounds') == ui['target_bounds']]
    if (len(target) != 1 or len(images) != 1 or images[0] != target[0]
            or target[0].get('bounds') != ui['target_bounds']
            or not all(target[0].get(k) is True for k in ('showing', 'enabled', 'sensitive'))
            or any(target[0].get(k) is not False for k in ('focused', 'focusable', 'entry',
                'editable', 'editable_text_interface', 'action_interface', 'button', 'dialog', 'modal'))):
        raise Stop('exact_unique_observed_thumbnail_required')
    return 'observed_imported'


def full_projection(nodes):
    return [[n.get(k, False if k == 'pressed' else None) for k in IMPORT_COLUMNS] for n in nodes]


BEFORE_HEIGHT_PATHS = frozenset((
    (0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 1),
    (0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 2),
    (0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 1),
    (0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 2),
    (0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0),
))


def before_rows_projection(rows):
    result, seen = [], set()
    bounds_column = IMPORT_COLUMNS.index('bounds')
    for row in rows:
        projected = list(row)
        path = tuple(row[0])
        if path in BEFORE_HEIGHT_PATHS:
            bounds = row[bounds_column]
            if (path in seen or not isinstance(bounds, dict)
                    or set(bounds) != {'x', 'y', 'width', 'height'}
                    or any(type(bounds[k]) is not int or bounds[k] != 0 for k in ('x', 'y', 'width'))
                    or type(bounds['height']) is not int or not 0 <= bounds['height'] <= 65535):
                raise Stop('exact_bounded_before_container_bounds_required')
            seen.add(path)
            projected[bounds_column] = dict(bounds, height=0)
        result.append(projected)
    if seen != BEFORE_HEIGHT_PATHS:
        raise Stop('all_five_fixed_before_containers_required')
    return result


def before_projection(nodes):
    for node in nodes:
        if tuple(node['path']) not in BEFORE_HEIGHT_PATHS:
            continue
        if (type(node.get('role')) is not int or node['role'] != 39
                or node.get('panel') is not True or 'label' not in node or node['label'] is not None
                or node.get('showing') is not True or node.get('sensitive') is not True
                or any(node.get(k, False if k == 'pressed' else None) is not False for k in (
                    'enabled', 'focused', 'focusable', 'selected', 'checked', 'pressed',
                    'modal', 'file_chooser', 'dialog', 'button', 'radio', 'entry',
                    'editable', 'editable_text_interface', 'action_interface'))):
            raise Stop('exact_noninteractive_before_container_required')
    return before_rows_projection(full_projection(nodes))


# Only these three reviewed location containers remain zero-width and inert.
# The two earlier before-only paths now have exact heights 34/0 and are excluded.
LOCATION_HEIGHT_PATHS = frozenset((
    (0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 1),
    (0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 2),
    (0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0),
))


def location_rows_projection(rows):
    result, seen = [], set()
    bounds_column = IMPORT_COLUMNS.index('bounds')
    for row in rows:
        projected = list(row)
        path = tuple(row[0])
        if path in LOCATION_HEIGHT_PATHS:
            bounds = row[bounds_column]
            if (path in seen or not isinstance(bounds, dict)
                    or set(bounds) != {'x', 'y', 'width', 'height'}
                    or any(type(bounds[k]) is not int or bounds[k] != 0 for k in ('x', 'y', 'width'))
                    or type(bounds['height']) is not int or not 0 <= bounds['height'] <= 65535):
                raise Stop('exact_bounded_location_container_bounds_required')
            seen.add(path)
            projected[bounds_column] = dict(bounds, height=0)
        result.append(projected)
    if seen != LOCATION_HEIGHT_PATHS:
        raise Stop('all_three_fixed_location_containers_required')
    return result


def location_projection(nodes):
    for node in nodes:
        bounds = node.get('bounds')
        if bounds is not None and (not isinstance(bounds, dict)
                or set(bounds) != {'x', 'y', 'width', 'height'}
                or any(type(bounds[k]) is not int for k in ('x', 'y', 'width', 'height'))):
            raise Stop('integer_location_bounds_required')
        if tuple(node['path']) not in LOCATION_HEIGHT_PATHS:
            continue
        if (type(node.get('role')) is not int or node['role'] != 39
                or node.get('panel') is not True or 'label' not in node or node['label'] is not None
                or node.get('showing') is not True or node.get('sensitive') is not True
                or any(node.get(k, False if k == 'pressed' else None) is not False for k in (
                    'enabled', 'focused', 'focusable', 'selected', 'checked', 'pressed',
                    'modal', 'file_chooser', 'dialog', 'button', 'radio', 'entry',
                    'editable', 'editable_text_interface', 'action_interface'))):
            raise Stop('exact_noninteractive_location_container_required')
    return location_rows_projection(full_projection(nodes))


def validate_import_ui(ui):
    if (ui.get('schema') != 1 or ui.get('reviewed_by') != 'main-reviewer'
            or ui.get('scope') != 'clip-media-import' or ui.get('observed_head') != HEAD
            or ui.get('observed_app_sha256') != APP_SHA
            or ui.get('public_node_columns') != list(IMPORT_COLUMNS)
            or ui.get('native_window_bounds') != [0, 0, 825, 384]
            or ui.get('import_xy') != [248, 160]
            or ui.get('import_guard', {}).get('region') != [201, 146, 94, 28]
            or ui.get('fixture') != {'basename': 'opaque-quadrants.png', 'bytes': 800,
                'sha256': FIXTURE_SHA, 'input_directory_basename': 'asset-clip-inputs'}):
        raise Stop('exact_reviewed_import_scope_required')
    for key in ('editor_public_nodes', 'native_before_public_nodes', 'native_location_public_nodes'):
        rows = ui.get(key)
        if (not isinstance(rows, list) or not 1 <= len(rows) <= 512
                or any(not isinstance(row, list) or len(row) != len(IMPORT_COLUMNS) for row in rows)):
            raise Stop('complete_fixed_import_template_required')
    target = ui.get('import_target', {})
    if (target.get('path') != [0, 25] or target.get('role') != 29 or target.get('label') != 'Import'
            or target.get('bounds') != {'x': 241, 'y': 146, 'width': 36, 'height': 28}
            or target.get('action_interface') is not False or target.get('button') is not False):
        raise Stop('exact_observed_nonAction_Import_label_required')


def exact_fixture(inputs):
    if not inputs.is_absolute() or inputs.name != 'asset-clip-inputs' or '..' in inputs.parts:
        raise Stop('exact_owned_fixture_directory_required')
    for ancestor in (inputs, *inputs.parents):
        if ancestor.is_symlink() or not ancestor.is_dir():
            raise Stop('real_fixture_directory_ancestors_required')
    if (inputs.stat().st_uid != os.getuid() or inputs.stat().st_mode & 0o777 != 0o700
            or set(p.name for p in inputs.iterdir()) != {'opaque-quadrants.png'}):
        raise Stop('fresh_0700_single_fixture_directory_required')
    path = inputs / 'opaque-quadrants.png'
    if (path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid()
            or path.stat().st_nlink != 1 or path.stat().st_size != 800
            or path.stat().st_mode & 0o777 != 0o600
            or hashlib.sha256(path.read_bytes()).hexdigest() != FIXTURE_SHA):
        raise Stop('exact_owned_800B_fixture_required')
    return path


def import_target(data, ui, pid):
    public_context(data, pid)
    if full_projection(data['nodes']) != ui['editor_public_nodes']:
        raise Stop('complete_current_empty_editor_tree_changed_no_Import_input')
    targets = [n for n in data['nodes'] if n.get('label') == 'Import' and n.get('showing')]
    if len(targets) != 1 or targets[0] != ui['import_target']:
        raise Stop('exact_current_visible_Import_label_required')
    return targets[0]


def native_template(data, ui, pid, phase):
    showing = public_context(data, pid, allow_dialog=True, native=True)
    if phase not in ('before', 'location'):
        raise Stop('known_GTK_template_phase_required')
    current = before_projection(data['nodes']) if phase == 'before' else location_projection(data['nodes'])
    expected = before_rows_projection(ui['native_before_public_nodes']) if phase == 'before' else location_rows_projection(ui['native_location_public_nodes'])
    if current != expected:
        raise Stop('complete_current_GTK_' + phase + '_template_changed_no_input')
    roots = [n for n in showing if n.get('dialog') and n.get('path') == [0]]
    accept = [n for n in showing if n.get('button') and n.get('label') in ('OK', 'Ok', 'Open', '打开')
              and n.get('action_interface') and (n.get('enabled') or n.get('sensitive'))
              and n.get('allowed_actions') == ['click']]
    cancel = [n for n in showing if n.get('button') and n.get('label') in ('Cancel', '取消')
              and n.get('action_interface') and (n.get('enabled') or n.get('sensitive'))
              and n.get('allowed_actions') == ['click']]
    if len(roots) != 1 or len(accept) != 1 or len(cancel) != 1:
        raise Stop('exact_current_GTK_dialog_accept_cancel_required')
    return showing, accept[0]


def bounded_native_metadata_observation(native, expected, read_sample, check_window, deadline, record,
                                        clock=time.monotonic, sleep=time.sleep):
    """Read at most three trees in <=2s; stability alone never grants input authority."""
    started = clock()
    until = min(deadline, started + 2.0)
    record.update(window=dict(native), max_samples=3, wait_limit_seconds=2.0,
                  samples=[], stable=False, file_input_authorized=False)
    previous = None
    try:
        for sample in range(1, 4):
            if until - clock() < 0.25:
                break
            check_window()
            if until - clock() < 0.25:
                break
            data = read_sample(sample)
            check_window()
            if clock() >= until:
                raise Stop('native_metadata_observation_deadline_no_input')
            # The existing probe supplies complete, same-PID, nonfield trees.
            public_context(data, native['pid'], allow_dialog=True, native=True)
            nodes = data['nodes']
            current = before_projection(nodes)
            digest = hashlib.sha256(json.dumps(current, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
            showing = [n for n in nodes if n.get('showing')]
            x, y, width, height = native['bounds']
            invalid_bounds = 0
            for node in showing:
                b = node.get('bounds', {})
                if (any(type(b.get(k)) is not int for k in ('x', 'y', 'width', 'height'))
                        or b['width'] < 0 or b['height'] < 0
                        or b['x'] < x or b['y'] < y
                        or b['x'] + b['width'] > x + width
                        or b['y'] + b['height'] > y + height):
                    invalid_bounds += 1
            record['samples'].append({'sample': sample, 'nodes': len(nodes), 'projection_sha256': digest,
                'before_matched': current == before_rows_projection(expected),
                'diagnostic_container_heights': [{'path': n['path'], 'height': n['bounds']['height']}
                    for n in nodes if tuple(n['path']) in BEFORE_HEIGHT_PATHS],
                'showing_enabled_count': sum(n.get('enabled') is True for n in showing),
                'showing_sensitive_count': sum(n.get('sensitive') is True for n in showing),
                'showing_bounds_outside_window_count': invalid_bounds,
                'Media_label_count': sum(n.get('label') == 'Media' for n in showing)})
            if digest == previous:
                record['stable'] = True
                break
            previous = digest
            if sample < 3 and until - clock() >= 0.35:
                sleep(min(0.1, until - clock() - 0.25))
    finally:
        record['elapsed_seconds'] = round(clock() - started, 3)
    record['verdict'] = 'stable_metadata_no_file_input' if record['stable'] else 'metadata_not_stable_no_input'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('app-pid', 'window-id'):
        parser.add_argument('--' + name, type=int, required=True)
    for name in ('work-dir', 'output', 'identity-approval', 'ui-approval'):
        parser.add_argument('--' + name, type=Path, required=True)
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
    size = [1280, 900]
    read_only_until = None
    report = {'schema': 'seecut-clip-media-import-v1', 'scope': 'clip-media-import', 'status': 'blocked',
              'source_head': HEAD, 'actions': [], 'captures': [], 'guards': [], 'public_metadata': [],
              'window_observations': [], 'project_created': False, 'project_create_attempted': False,
              'field_values_read': False, 'file_lists_read': False, 'settings_opened': False,
              'settings_pixels_captured': False, 'login_attempted': False, 'model_started': False,
              'external_upload_requested': False, 'sourcecopy_verified': False,
              'product_verdict': 'pending_main_actual_image_review', 'cleanup_owner': 'launcher <=300s'}

    def command(argv, binary=False, search=False):
        left = min(end, read_only_until or end) - time.monotonic()
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

    def observe_import_native_metadata(native):
        nonlocal read_only_until
        # Exact 0aa filtered chooser; historical disabled/zero bounds stay raw.
        if native['bounds'] != [0, 0, 825, 384]:
            raise Stop('unknown_import_GTK_geometry_observed_no_input')
        record = {}
        report['native_metadata_observation'] = record
        read_only_until = min(end, time.monotonic() + 2.0)
        try:
            bounded_native_metadata_observation(native, import_ui['native_before_public_nodes'],
                lambda sample: probe(f'12-import-native-sample-{sample:02d}-public', native=native, allow_dialog=True),
                lambda: focus_native(native), read_only_until, record)
        finally:
            read_only_until = None
        if not record['stable'] or not all(n['before_matched'] for n in record['samples']):
            raise Stop('filtered_import_GTK_before_unstable_or_changed_no_input')

    def collect_import_native():
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
                    snapshot('12-import-native-observed', native=now, allow_dialog=True)
                    if now['bounds'] != import_ui['native_window_bounds']:
                        raise Stop('unknown_import_GTK_geometry_observed_no_input')
                    observe_import_native_metadata(now)
                    return now
                if now != previous:
                    previous, stable = now, time.monotonic()
            else:
                previous, stable = None, None
            time.sleep(0.25)
        command(['xdotool', 'windowfocus', '--sync', str(args.window_id)])
        snapshot('12-import-app-unknown-result', allow_dialog=True)
        raise Stop('no_unique_stable_owned_import_native_main_review_required')

    def native_action(native, node, mode):
        focus_native(native)
        # Fresh complete phase match immediately before spawning the independently
        # guarded helper. No target reindexing, synthetic keyboard accept or replay.
        native_template(probe('14-before-' + mode + '-public', native=native, allow_dialog=True),
                        import_ui, native['pid'], 'location')
        report['actions'].append({'kind': 'one_owned_public_native_attempt', 'mode': mode,
                                  'node_path': node['path']})
        completed = subprocess.run([args.probe_python, '-B', str(dependencies['native_action']),
            '--target-pid', str(native['pid']), '--owned-root-pid', str(args.app_pid),
            '--window-id', str(native['window']), '--owned-path', str(fixture), '--input-dir', str(args.input_dir),
            '--ui-approval', str(dependencies['import_guard']), '--mode', mode,
            '--node-public', json.dumps(node, separators=(',', ':')),
            '--window-bounds', json.dumps(native['bounds']), '--deadline-monotonic', str(min(end, time.monotonic() + 3)),
            '--private-accessibility-bus'], capture_output=True, timeout=min(4, max(0.25, end-time.monotonic())))
        if len(completed.stdout) > 8192:
            raise Stop('bounded_native_action_report_required')
        data = json.loads(completed.stdout)
        raw = (json.dumps(data, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        path = directory / ('14-native-' + mode + '-action.json')
        with path.open('xb') as stream:
            path.chmod(0o600)
            stream.write(raw)
        if (sum(p.stat().st_size for p in directory.iterdir() if p.is_file()) + 16384 > 15728640):
            path.unlink()
            raise Stop('native_action_total_evidence_budget_exceeded')
        if (completed.returncode or data.get('success') is not True or data.get('field_values_read') is not False
                or data.get('file_lists_read') is not False):
            raise Stop('native_import_UI_action_unconfirmed_no_retry')

    def import_flow():
        report['project_manifest_before_import'] = report['project_manifest']
        focus_main()
        command(['xdotool', 'windowsize', '--sync', str(args.window_id), '1280', '900'])
        pause()
        fields = dict(line.split('=', 1) for line in command(
            ['xdotool', 'getwindowgeometry', '--shell', str(args.window_id)]).splitlines() if '=' in line)
        if [int(fields[k]) for k in ('X', 'Y', 'WIDTH', 'HEIGHT')] != [0, 0, 1280, 900]:
            raise Stop('actual_restored_editor_geometry_changed')
        size[:] = [1280, 900]
        command(['xdotool', 'mousemove', '--window', str(args.window_id), '100', '650'])
        import_target(probe('11-before-import-public'), import_ui, args.app_pid)
        guard(pixels(), 'visible-import-button', import_ui['import_guard'])
        import_target(probe('11-import-target-recheck-public'), import_ui, args.app_pid)
        guard(pixels(), 'visible-import-button-recheck', import_ui['import_guard'])
        report['import_attempted'] = True
        click(import_ui['import_xy'], 'visible-Import')
        native = collect_import_native()
        data = probe('12-import-native-before-public', native=native, allow_dialog=True)
        native_template(data, import_ui, native['pid'], 'before')
        guard(pixels(native), 'native-footer-before-location', import_ui['native_footer_guard'], 825, 384)
        focus_native(native)
        report['actions'].append({'kind': 'one_location_popup_attempt', 'key': 'ctrl+l'})
        command(['xdotool', 'key', '--clearmodifiers', 'ctrl+l'])
        pause()
        snapshot('13-import-native-location', native=native, allow_dialog=True)
        data = probe('13-import-native-location-recheck-public', native=native, allow_dialog=True)
        nodes, _accept = native_template(data, import_ui, native['pid'], 'location')
        entries = [n for n in nodes if n.get('entry') and n.get('editable_text_interface')
                   and (n.get('enabled') or n.get('sensitive')) and n.get('focused')]
        if len(entries) != 1:
            raise Stop('exact_unique_focused_GTK_location_required')
        report['file_input_attempted'] = True
        native_action(native, entries[0], 'set-location')
        pause()
        data = probe('14-native-before-accept-public', native=native, allow_dialog=True)
        _nodes, accept = native_template(data, import_ui, native['pid'], 'location')
        native_action(native, accept, 'accept')
        until = min(end, time.monotonic() + 12)
        while native_windows() and time.monotonic() < until:
            time.sleep(0.25)
        if native_windows():
            raise Stop('native_import_not_gone_main_review_no_retry')
        command(['xdotool', 'windowfocus', '--sync', str(args.window_id)])
        focus_main()
        prior = None
        for sample in range(1, 5):
            data = probe(f'15-import-result-sample-{sample:02d}-public', allow_dialog=True)
            try:
                state = imported_state(data, timeline_ui, args.app_pid)
            except Stop as error:
                if str(error) != 'unknown_import_result_main_review_required':
                    raise
                state = 'observed_transient_read_only'
            if state == 'observed_transient_read_only':
                report.setdefault('import_readonly_pending', []).append(
                    {'sample': sample, 'status': 'readonly_import_pending'})
                prior = None
                pause()
                continue
            frame = pixels()
            guard(frame, 'imported-thumbnail', timeline_ui['thumbnail_guard'])
            guard(frame, 'imported-caption', timeline_ui['caption_guard'])
            current = (timeline_canonical(imported_projection(data['nodes'], timeline_ui)), hashlib.sha256(frame).hexdigest())
            if current == prior:
                report['import_result_stable_observed'] = True
                break
            prior = current
            pause()
        if not report.get('import_result_stable_observed'):
            snapshot('15-import-result', allow_dialog=True)
            raise Stop('import_result_stability_unconfirmed_main_review_required')
        exact_fixture(args.input_dir)
        report['fixture_source_unchanged'] = True
        # Native clip import references the input; do not claim or scan a personal-
        # library copy. Only the already authorized owned concat.json bytes/SHA.
        report['project_manifest_after_import'] = project_manifest_record(args.work_dir)
        report['project_manifest_changed'] = (report['project_manifest_after_import'].get('sha256')
                != report['project_manifest_before_import'].get('sha256'))
        snapshot('15-import-result', allow_dialog=True)
        report['status'] = 'clip_import_result_observed_main_review_required'

    def timeline_flow():
        report['status'] = 'blocked'
        # Only the exact imported tree can authorize the one declared gesture.
        data = probe('16-before-insert-public')
        if imported_state(data, timeline_ui, args.app_pid) != 'observed_imported':
            raise Stop('transient_import_tree_cannot_authorize_input')
        frame = pixels()
        guard(frame, 'fresh-imported-thumbnail', timeline_ui['thumbnail_guard'])
        guard(frame, 'fresh-imported-caption', timeline_ui['caption_guard'])
        focus_main()
        report['timeline_insert_attempted'] = True
        report['actions'].append({'kind': 'one_guarded_double_click_attempt',
            'node_path': timeline_ui['target_path'], 'xy': timeline_ui['double_click_xy'],
            'clicks': 2, 'delay_ms': 120})
        command(['xdotool', 'mousemove', '--window', str(args.window_id),
                 *map(str, timeline_ui['double_click_xy'])])
        focus_main()
        command(['xdotool', 'click', '--repeat', '2', '--delay', '120', '1'])
        command(['xdotool', 'mousemove', '--window', str(args.window_id), '100', '650'])
        pause()
        prior = None
        for sample in range(1, 5):
            # New timeline state is observation only; no semantic success gate or input.
            data = probe(f'16-timeline-result-sample-{sample:02d}-public')
            current = (timeline_canonical(timeline_rows(data['nodes'])), hashlib.sha256(pixels()).hexdigest())
            if current == prior:
                report['timeline_result_stable_observed'] = True
                break
            prior = current
            pause()
        exact_fixture(args.input_dir)
        report['fixture_source_unchanged_after_insert'] = True
        report['project_manifest_after_insert'] = project_manifest_record(args.work_dir)
        snapshot('16-timeline-result')
        if not report.get('timeline_result_stable_observed'):
            raise Stop('timeline_result_stability_unconfirmed_main_review_required')
        report['status'] = 'clip_timeline_result_observed_main_review_required'

    def probe(name, native=None, allow_dialog=False):
        if native is None:
            focus_main()
        else:
            focus_native(native)
        pid = native['pid'] if native else args.app_pid
        io_end = min(end, read_only_until or end)
        left = io_end - time.monotonic()
        if left < 0.25:
            raise Stop('public_metadata_observation_deadline_no_input')
        completed = subprocess.run([args.probe_python, '-B', str(helper), '--app-pid', str(pid),
            '--owned-root-pid', str(args.app_pid), '--output', str(directory), '--output-name', name + '.json',
            '--deadline-monotonic', str(min(io_end, time.monotonic() + 3)), '--private-accessibility-bus'],
            capture_output=True, timeout=min(5, left))
        path = directory / (name + '.json')
        if completed.returncode or path.is_symlink() or not path.is_file() or path.stat().st_size > 131072:
            raise Stop('bounded_public_metadata_unavailable_no_retry')
        if sum(p.stat().st_size for p in directory.iterdir() if p.is_file()) + 16384 > 15728640:
            path.unlink()
            raise Stop('metadata_total_budget_exceeded_no_further_capture')
        data = json.loads(path.read_bytes())
        if (native is None and allow_dialog and report['scope'] == 'clip-media-import'
                and tuple(size) == (1280, 900) and helper == dependencies['post_insert']
                and re.fullmatch(r'20-file-menu-observed(?:-sample-0[1-4])?-public', name)):
            post.file_menu_context(data, pid, public_context, timeline_rows, Stop)
        else:
            public_context(data, pid, allow_dialog=allow_dialog, native=native is not None, width=size[0], height=size[1])
        report['public_metadata'].append(path.name)
        return data

    def pixels(native=None):
        focus_native(native) if native else focus_main()
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
        import_ui = json.loads(dependencies['import_guard'].read_bytes())
        validate_import_ui(import_ui)
        timeline_ui = json.loads(dependencies['timeline_guard'].read_bytes())
        validate_timeline_ui(timeline_ui)
        report['timeline_guard_sha256'] = ui['timeline_guard_sha256']
        if args.input_dir != args.work_dir / 'asset-clip-inputs':
            raise Stop('launcher_owned_fixture_subdirectory_required')
        fixture = exact_fixture(args.input_dir)
        report['import_guard_sha256'] = ui['import_guard_sha256']
        helper = dependencies['probe']
        dims = list(map(int, command(['xdotool', 'getdisplaygeometry']).split()))
        if len(dims) != 2 or not (1440 <= dims[0] <= 1920 and 900 <= dims[1] <= 1200):
            raise Stop('private_display_dimensions_outside_finite_bounds')
        project_output = owned_project_output(args.work_dir, fresh=True)
        fresh_directory = args.output / 'main-qa-clip-media-import'
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
        probe('01-after-quick-public')
        before = probe('02-before-navigation-public')
        for name, expected in ui['navigation'].items():
            matches = [n for n in before['nodes'] if n.get('showing') and n.get('label') == expected['label'] and n.get('role') == 43]
            if len(matches) != 1 or any(matches[0].get(k) != expected.get(k) for k in PUBLIC_KEYS):
                raise Stop('current_navigation_metadata_changed')
        guard(pixels(), 'nav-target')
        click(ui['navigation_xy'], ui['page'])
        probe('02-page-public')
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
        probe('03-current-new-clip-dialog-public', allow_dialog=True)
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
        # Four empty-editor sizes already have actual acceptance evidence.
        import_flow()
        timeline_flow()
        post_spec = importlib.util.spec_from_file_location('clip_post_insert', dependencies['post_insert'])
        post = importlib.util.module_from_spec(post_spec)
        post_spec.loader.exec_module(post)
        helper = dependencies['post_insert']
        report['post_insert_sha256'] = ui['post_insert_sha256']
        report['status'] = 'blocked'
        post.run(context=public_context, rows=timeline_rows, stop=Stop, probe=probe,
                 pixels=pixels, guard=guard, focus=focus_main, command=command, pause=pause,
                 snapshot=snapshot, report=report, args=args, remaining=lambda: end - time.monotonic(),
                 fixture=exact_fixture, manifest=project_manifest_record)

    except Stop as exc:
        report['blocking_reason'] = str(exc)
    except subprocess.TimeoutExpired:
        report['blocking_reason'] = 'owned_helper_timeout_no_retry'
    except Exception:
        report['blocking_reason'] = 'unexpected_owned_runtime_error_raw_withheld'
    finally:
        report['elapsed_seconds'] = round(time.monotonic() - started, 3)
        raw = bounded_report_bytes(report)
        if directory is not None:
            files = list(directory.iterdir())
            if any(p.is_symlink() or not p.is_file() for p in files) or sum(p.stat().st_size for p in files) + len(raw) > 15728640:
                report['status'] = 'artifact_budget_exceeded_do_not_publish'
                raw = (json.dumps({'schema': report['schema'], 'scope': report['scope'],
                    'status': 'artifact_budget_exceeded_do_not_publish',
                    'blocking_reason': report.get('blocking_reason'),
                    'complete_report_saved': False}, separators=(',', ':')) + '\n').encode()
            if len(raw) <= 16384:
                path = directory / 'clip-import-report.json'
                with path.open('xb') as stream:
                    path.chmod(0o600)
                    stream.write(raw)
        print(json.dumps({k: report[k] for k in ('scope', 'status', 'blocking_reason', 'sourcecopy_verified') if k in report}))
    return 0 if report['status'] == 'clip_timeline_result_observed_main_review_required' else 2


if __name__ == '__main__':
    raise SystemExit(main())
