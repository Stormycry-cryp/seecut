#!/usr/bin/env python3
"""Scope-limited public structure and one named Settings Action; no field/pixel reads in Settings."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

HEAD = '0aa9406247e53f073c0b4df686adc68b40e2f8f6'
APP_SHA = '03fb1752c34adb0c0f4a21521c2302ee80c9b23203728cbcf636d3a4a57eaea2'
SCOPE = 'visual-theme-matrix-observation'
DIRECTORY = 'main-qa-visual-theme-matrix'
PUBLIC_NAMES = frozenset(('00-before-quick-public.json','01-after-quick-public.json','02-before-settings-public.json','03-settings-entry-public.json','04-dark-settings-public.json','05-after-close-public.json',*[f'{theme}-{width}x{height}-public.json' for theme in ('light','dark') for width,height in ((1024,900),(1280,900),(1440,900),(1280,720))]))
ACTION_NAMES = frozenset(('02-settings-action.json','04-dark-theme-action.json','05-close-settings-action.json'))
SETTING_BOUNDS = {'x': 18, 'y': 784, 'width': 44, 'height': 44}
BOOL_KEYS = ('showing', 'enabled', 'sensitive', 'focused', 'focusable', 'selected',
    'checked', 'modal', 'file_chooser', 'dialog', 'button', 'radio', 'panel', 'entry',
    'editable', 'editable_text_interface', 'action_interface')
TARGET = dict(path=[0, 6], role=43, label='设置', bounds=SETTING_BOUNDS,
    allowed_actions=['click'], **{k: k in ('showing', 'enabled', 'sensitive', 'focusable',
        'button', 'action_interface') for k in BOOL_KEYS})
MODE_GUARD = {'region': [420, 388, 520, 155],
    'rgb_sha256': '2e513f04d1897fef7c6e6a0710c7afae9b7d6d592ddf1e316cb7e3fa89eb1b97'}
# These are source-controlled nonfield names, not arbitrary caption reads.
GROUPS = { (596, 147, 188, 36): ('工作模式', ('工作模式，快速', '工作模式，专业')),
           (596, 222, 188, 36): ('外观', ('外观，浅色', '外观，深色')) }


class Stop(Exception):
    pass


def read_decl(path):
    if (not path.is_absolute() or path.is_symlink() or not path.is_file()
            or path.stat().st_size > 16384 or path.stat().st_uid != os.getuid()):
        raise Stop('bounded_owned_declaration_required')
    return json.loads(path.read_bytes())


def validate_ui(ui, controller=None):
    if (ui.get('schema') != 1 or ui.get('reviewed_by') != 'main-reviewer'
            or ui.get('scope') != SCOPE or ui.get('runtime_head') != HEAD
            or ui.get('runtime_app_sha256') != APP_SHA or ui.get('window') != [1280, 900]
            or ui.get('quick_xy') != [558, 500] or ui.get('mode_guard') != MODE_GUARD
            or ui.get('settings_target') != TARGET or ui.get('settings_guard', {}).get('region') != [18, 784, 44, 44]
            or ui.get('runtime_limits') != {'controller': 120, 'App_and_cleanup': 300, 'reserve': 15}
            or ui.get('artifact_limits') != {'PNG_count': 8, 'PNG_each': 2097152, 'PNG_total': 14680064, 'all': 15728640, 'metadata_each': 131072, 'action_each': 8192, 'report_each': 16384}
            or ui.get('settings_stop') != 'one_dark_Action_one_close_Action_then_observation_only'):
        raise Stop('exact_visual_settings_scope_required')
    if ui.get('public_sha256') != hashlib.sha256(Path(__file__).read_bytes()).hexdigest():
        raise Stop('exact_scope_helper_SHA_required')
    if controller is not None and (controller.is_symlink() or not controller.is_file()
            or controller.stat().st_size > 65536
            or hashlib.sha256(controller.read_bytes()).hexdigest() != ui.get('controller_sha256')):
        raise Stop('exact_controller_copy_SHA_required')


def rgb_matches(raw, guard):
    if len(raw) != 1280 * 900 * 3:
        return False
    x, y, w, h = guard['region']
    if any(type(v) is not int for v in (x, y, w, h)) or min(x, y) < 0 or min(w, h) < 1 or x+w > 1280 or y+h > 900:
        return False
    crop = b''.join(raw[(r*1280+x)*3:(r*1280+x+w)*3] for r in range(y, y+h))
    return hashlib.sha256(crop).hexdigest() == guard['rgb_sha256']


def target_root(pid, deadline):
    import gi
    gi.require_version('Atspi', '2.0')
    from gi.repository import Atspi
    desktop = Atspi.get_desktop(0)
    if desktop is None or desktop.get_child_count() > 64:
        raise Stop('public_accessibility_unavailable')
    matches = []
    for i in range(desktop.get_child_count()):
        if time.monotonic() >= deadline:
            raise Stop('public_accessibility_deadline')
        child = desktop.get_child_at_index(i)
        if child is not None and child.get_process_id() == pid:
            matches.append(child)
    if len(matches) != 1:
        raise Stop('unique_owned_public_root_required')
    return matches[0], Atspi


def populate_names(records, handles, mode):
    """Names are forbidden before access for every field, its ancestors and unknown controls."""
    blocked = {tuple(n['path']) for n in records if n['entry'] or n['editable'] or n['editable_text_interface']}
    windows = [n for n in records if n['path']==[0] and n['role']==23]
    height = windows[0].get('bounds',{}).get('height',0) if len(windows)==1 else 0
    setting_bounds = dict(x=18,y=height-116,width=44,height=44)
    def clean(path):
        p = tuple(path)
        return not any(q[:len(p)] == p or p[:len(q)] == q for q in blocked)
    by_path = {tuple(n['path']): n for n in records}
    if mode == 'before':
        for n in records:
            if n['role'] == 43 and n['bounds'] == setting_bounds and n['button'] and clean(n['path']):
                name = handles[tuple(n['path'])].get_name()
                n['label'] = '设置' if name == '设置' else None
        return
    for n in records:
        b = n.get('bounds', {})
        geometry = tuple(b.get(k) for k in ('x', 'y', 'width', 'height'))
        if n['role'] != 39 or geometry not in GROUPS or not n['panel'] or not clean(n['path']):
            continue
        children = [r for r in records if r['path'][:-1] == n['path'] and r['radio'] and r['role'] == 44]
        if len(children) != 2 or any(not clean(r['path']) for r in children):
            continue
        parent_label, captions = GROUPS[geometry]
        for record, expected in [(n, parent_label), *zip(sorted(children, key=lambda r: r['bounds'].get('x', -1)), captions)]:
            record['label'] = expected if handles[tuple(record['path'])].get_name() == expected else None
    for n in records:
        if (n['role'] == 43 and n['button'] and n.get('bounds') == {'x': 1216, 'y': 68, 'width': 26, 'height': 128}
                and clean(n['path'])):
            n['label'] = '关闭设置' if handles[tuple(n['path'])].get_name() == '关闭设置' else None


def collect(pid, deadline, mode, root_and_api=None):
    root, A = root_and_api if root_and_api is not None else target_root(pid, deadline)
    pending, records, handles = [(root, [], 0)], [], {}
    complete = True
    while pending:
        if time.monotonic() >= deadline or len(records) >= 512:
            complete = False
            break
        node, path, depth = pending.pop()
        if depth > 24:
            complete = False
            continue
        node.clear_cache_single()
        states, role, interfaces = node.get_state_set(), node.get_role(), set(node.get_interfaces())
        skip = role in (A.Role.TABLE, A.Role.TREE, A.Role.TREE_TABLE, A.Role.LIST, A.Role.DIRECTORY_PANE)
        n = dict(path=path, role=int(role), label=None, bounds={})
        for key in ('showing', 'enabled', 'sensitive', 'focused', 'focusable', 'selected', 'checked', 'modal'):
            n[key] = bool(states.contains(getattr(A.StateType, key.upper())))
        n.update(file_chooser=role == A.Role.FILE_CHOOSER, dialog=role == A.Role.DIALOG,
            button=role == A.Role.PUSH_BUTTON, radio=role == A.Role.RADIO_BUTTON, panel=role == A.Role.PANEL,
            entry=role in (A.Role.ENTRY, A.Role.TEXT), editable=bool(states.contains(A.StateType.EDITABLE)),
            editable_text_interface='EditableText' in interfaces, action_interface='Action' in interfaces,
            allowed_actions=[])
        if (n['button'] or n['radio']) and n['action_interface']:
            action = node.get_action_iface()
            count = action.get_n_actions()
            if not 0 <= count <= 8:
                complete = False
            for i in range(min(count, 8)):
                value = action.get_action_name(i)
                if value in ('click', 'activate', 'press'):
                    n['allowed_actions'].append(value)
        component = node.get_component_iface()
        extent = component.get_extents(A.CoordType.SCREEN) if component else None
        if extent:
            n['bounds'] = {k: int(getattr(extent, k)) for k in ('x', 'y', 'width', 'height')}
        records.append(n); handles[tuple(path)] = node
        if skip:
            continue
        count = node.get_child_count()
        if not 0 <= count <= 128:
            complete = False
        for i in reversed(range(min(count, 128))):
            child = node.get_child_at_index(i)
            if child is not None:
                pending.append((child, path+[i], depth+1))
    # Do not read any names on incomplete traversal: an omitted editable descendant could mask an ancestor.
    if complete:
        populate_names(records, handles, mode)
    return dict(status='public_metadata_observed', app_pid=pid, coverage_complete=complete,
        nodes=records, field_values_read=False, editable_or_descendant_names_read=False,
        text_or_value_interfaces_read=False, ui_actions=[], screenshots=[], scope=SCOPE)


def context(data, pid, before=False, width=1280, height=900):
    nodes = data.get('nodes')
    if (data.get('status') != 'public_metadata_observed' or data.get('app_pid') != pid
            or data.get('coverage_complete') is not True or data.get('field_values_read') is not False
            or data.get('editable_or_descendant_names_read') is not False
            or data.get('text_or_value_interfaces_read') is not False
            or data.get('ui_actions') != [] or data.get('screenshots') != [] or not isinstance(nodes, list)
            or not 1 <= len(nodes) <= 512 or len({tuple(n['path']) for n in nodes}) != len(nodes)):
        raise Stop('complete_owned_nonfield_metadata_required')
    visible = [n for n in nodes if n.get('showing')]
    windows = [n for n in visible if n['role'] == 23]
    if (len(windows) != 1 or windows[0]['path'] != [0]
            or windows[0]['bounds'] != dict(x=0, y=0, width=width, height=height)
            or not windows[0]['enabled'] or not windows[0]['sensitive']
            or any(n['modal'] or n['dialog'] or n['file_chooser'] for n in visible)
            or any(n['focused'] and (n['entry'] or n['editable'] or n['editable_text_interface']) for n in visible)):
        raise Stop('same_window_no_modal_or_editable_focus_required')
    if before and any(n.get('bounds') in [dict(zip(('x','y','width','height'), g)) for g in GROUPS]
                      or n.get('label') == '关闭设置' for n in visible):
        raise Stop('already_Settings_no_pixels_or_input')
    return visible


def entry_target(data, pid):
    visible = context(data, pid, before=True)
    targets = [n for n in visible if n.get('label') == '设置']
    if len(targets) != 1 or targets[0] != TARGET:
        raise Stop('exact_current_named_Settings_Action_required')
    return targets[0]



def read_guard(ui):
    path = Path(__file__).with_name('visual-theme-matrix-guard.json')
    if (path.is_symlink() or not path.is_file() or path.stat().st_size > 65536
            or hashlib.sha256(path.read_bytes()).hexdigest() != ui.get('guard_sha256')):
        raise Stop('exact_observed_guard_copy_SHA_required')
    data = json.loads(path.read_bytes())
    if len(data.get('page_nodes',[])) != 39 or len(data.get('settings_nodes',[])) != 52:
        raise Stop('complete_actual_guard_rows_required')
    return data


def safe_schema(nodes):
    for n in nodes:
        if (set(n) != set(TARGET) or not isinstance(n['path'],list) or len(n['path']) > 24
                or any(type(i) is not int or not 0 <= i < 128 for i in n['path'])
                or type(n['role']) is not int or n['label'] not in (None,'设置','关闭设置','工作模式','工作模式，快速','工作模式，专业','外观','外观，浅色','外观，深色')
                or any(type(n[k]) is not bool for k in BOOL_KEYS)
                or not isinstance(n['allowed_actions'],list) or any(v not in ('click','activate','press') for v in n['allowed_actions'])
                or not isinstance(n['bounds'],dict) or set(n['bounds']) not in (set(),{'x','y','width','height'})
                or any(type(v) is not int or not -65535 <= v <= 65535 for v in n['bounds'].values())
                or n['bounds'].get('width',0)<0 or n['bounds'].get('height',0)<0):
            raise Stop('strict_public_structure_schema_required')


def settings_gate(data, pid, guard, phase):
    context(data,pid);safe_schema(data['nodes'])
    expected = json.loads(json.dumps(guard['settings_nodes']))
    current = json.loads(json.dumps(data['nodes']))
    current = canonical_page_toast(current, expected, settings=True)
    if phase == 'dark':
        for n in expected:
            if n['path']==[0,30,0]: n['checked']=False
            if n['path']==[0,30,1]: n['checked']=True
            if n['path']==[0,26]: n['focused']=False
            # run37633928579: exact observed public group focus, no inferred alternative.
            if n['path']==[0,30]: n['focused']=True; n['focusable']=True
    elif phase != 'light':
        raise Stop('closed_Settings_phase_required')
    if current != expected:
        raise Stop('complete_Settings_structure_or_checked_changed_STOP')
    return next(n for n in data['nodes'] if n['path']==([0,30,1] if phase=='light' else [0,26]))



def canonical_page_toast(current, expected, settings=False):
    """Only complete observed39-page or52-Settings synchronous0/+12 endpoints.

    Settings +12 is bound to run37630729355's complete public metadata.
    The caller passes a private projection; raw metadata stays unchanged.
    Intermediate/mixed motion or other property changes are rejected.
    """
    count = 52 if settings else 39
    paths = ((0,34),(0,35),(0,35,0),(0,35,1)) if settings else ((0,26),(0,27),(0,27,0),(0,27,1))
    if len(current)!=count or len(expected)!=count:
        return current
    actual={tuple(n['path']):n for n in current if tuple(n['path']) in paths}
    baseline={tuple(n['path']):n for n in expected if tuple(n['path']) in paths}
    if len(actual)!=4 or len(baseline)!=4:
        return current
    offset=actual[paths[0]]['bounds'].get('y',-65536)-baseline[paths[0]]['bounds']['y']
    if offset not in (0,12):
        return current
    for path in paths:
        wanted=json.loads(json.dumps(baseline[path]))
        wanted['bounds']['y']+=offset
        if actual[path]!=wanted:
            return current
    for path in paths:
        actual[path]['bounds']['y']=baseline[path]['bounds']['y']
    return current

def page_gate(data, pid, guard, phase, width=1280, height=900, exact_bounds=True):
    context(data,pid,before=True,width=width,height=height);safe_schema(data['nodes'])
    current=json.loads(json.dumps(data['nodes']));expected=json.loads(json.dumps(guard['page_nodes']))
    if exact_bounds and (width,height)==(1280,900):
        current=canonical_page_toast(current,expected)
    if len(current)!=39:
        raise Stop('unknown_closed_or_generator_structure_observed_STOP')
    for n in current:
        # Sheet close has a source-defined rail focus return, not a fabricated
        # observed focus template. The wrapper FocusScope can have no public focus.
        if phase=='closed':
            if n['focused'] and n['path'] != [0,6]:
                raise Stop('unknown_close_focus_observed_STOP')
            if n['path']==[0,6]: n['focused']=False
        elif phase=='entry':
            if n['focused'] and n['path'] != [0,25]:
                raise Stop('unknown_entry_focus_observed_STOP')
            if n['path']==[0,25]: n['focused']=False
        else:
            raise Stop('closed_page_phase_required')
    for n in expected:
        n['focused']=False
    if not exact_bounds:
        if any(bool(a['bounds']) != bool(b['bounds']) for a,b in zip(current,expected)):
            raise Stop('unknown_resize_geometry_coverage_STOP')
        # Resize is authorized observation only. Keep every path/role/state/action
        # exact while recording all real layout bounds without calling them an exact template.
        for n in current: n['bounds']={}
        for n in expected: n['bounds']={}
    if current!=expected:
        raise Stop('unknown_closed_or_generator_structure_observed_STOP')
    return data['nodes']


def settings_action_once(target, role, label, resolve, guard, record, deadline):
    guard()
    node,A=resolve(target);node.clear_cache_single()
    states,interfaces=node.get_state_set(),set(node.get_interfaces())
    if (node.get_role()!=role or node.get_child_count()!=0 or 'EditableText' in interfaces
            or 'Action' not in interfaces or states.contains(A.StateType.EDITABLE)):
        raise Stop('known_nonfield_leaf_Action_required')
    for key in ('showing','enabled','sensitive','focused','focusable','selected','checked','modal'):
        if bool(states.contains(getattr(A.StateType,key.upper()))) != target[key]:
            raise Stop('final_Settings_target_state_changed')
    if node.get_name()!=label:
        raise Stop('closed_static_Settings_target_label_required')
    extent=node.get_component_iface().get_extents(A.CoordType.SCREEN)
    if {k:int(getattr(extent,k)) for k in ('x','y','width','height')} != target['bounds']:
        raise Stop('final_Settings_target_bounds_changed')
    action=node.get_action_iface()
    if action.get_n_actions()!=1 or action.get_action_name(0)!='click':
        raise Stop('unique_advertised_Settings_Action_required')
    guard()
    if time.monotonic()>=deadline: raise Stop('deadline_before_Settings_Action')
    record['named_Action_attempted']=True
    if not action.do_action(0): raise Stop('Settings_Action_false_no_retry')
    record['named_Action_returned_true']=True

def open_once(target, resolve, guard, record, deadline):
    guard()
    node, A = resolve(target)
    node.clear_cache_single()
    states, interfaces = node.get_state_set(), set(node.get_interfaces())
    if (node.get_role() != A.Role.PUSH_BUTTON or 'EditableText' in interfaces or 'Action' not in interfaces
            or states.contains(A.StateType.EDITABLE)
            or not all(states.contains(v) for v in (A.StateType.SHOWING, A.StateType.ENABLED, A.StateType.SENSITIVE))):
        raise Stop('fresh_Settings_Action_state_required')
    if node.get_child_count() != 0:
        raise Stop('fresh_named_Settings_leaf_required')
    for key in ('showing', 'enabled', 'sensitive', 'focused', 'focusable', 'selected', 'checked', 'modal'):
        if bool(states.contains(getattr(A.StateType, key.upper()))) != TARGET[key]:
            raise Stop('fresh_Settings_public_state_changed')
    if node.get_name() != '设置':
        raise Stop('fresh_Settings_closed_label_required')
    extent = node.get_component_iface().get_extents(A.CoordType.SCREEN)
    if {k: int(getattr(extent, k)) for k in ('x', 'y', 'width', 'height')} != SETTING_BOUNDS:
        raise Stop('fresh_Settings_bounds_required')
    action = node.get_action_iface()
    if not 0 <= action.get_n_actions() <= 8:
        raise Stop('bounded_Settings_Action_required')
    indices = [i for i in range(min(action.get_n_actions(), 8)) if action.get_action_name(i) == 'click']
    if len(indices) != 1:
        raise Stop('one_named_Settings_click_required')
    guard()
    if time.monotonic() >= deadline:
        raise Stop('deadline_before_Settings_Action')
    record['Settings_Action_attempted'] = True
    # The guard never runs after this call, including false/error returns.
    if not action.do_action(indices[0]):
        raise Stop('Settings_Action_false_no_retry')
    record['Settings_Action_returned_true'] = True


def owned_descendant(pid, owner):
    visited = set()
    while pid >= 2 and pid not in visited and len(visited) < 16:
        visited.add(pid)
        proc = Path('/proc') / str(pid)
        if proc.stat().st_uid != os.getuid():
            return False
        if pid == owner:
            return True
        pid = int(next(v for v in (proc/'status').read_text().splitlines() if v.startswith('PPid:')).split()[1])
    return False


def owned_window(command, pid, window, width=1280, height=900):
    if (int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != pid
            or int(command(['xdotool', 'getwindowfocus']).strip()) != window
            or set(command(['xdotool', 'search', '--onlyvisible', '--pid', str(pid)], search=True).split()) != {str(window)}):
        raise Stop('same_owned_focused_window_required')
    fields = dict(v.split('=',1) for v in command(['xdotool','getwindowgeometry','--shell',str(window)]).splitlines() if '=' in v)
    if [int(fields[k]) for k in ('X','Y','WIDTH','HEIGHT')] != [0,0,width,height]:
        raise Stop('exact_window_geometry_required')
    pending, seen = [pid], {pid}
    while pending:
        tasks = list((Path('/proc')/str(pending.pop())/'task').iterdir())
        if len(tasks) > 64:
            raise Stop('owned_task_limit')
        for task in tasks:
            try:
                children = list(map(int, (task/'children').read_text().split()))
            except FileNotFoundError:
                continue
            for child in children:
                if child not in seen and owned_descendant(child, pid):
                    if len(seen) >= 24:
                        raise Stop('owned_descendant_limit')
                    seen.add(child); pending.append(child)
                    if command(['xdotool','search','--onlyvisible','--pid',str(child)], search=True).strip():
                        raise Stop('owned_extra_window_no_input_or_pixels')


def write_record(directory, name, data, limit):
    if (name not in PUBLIC_NAMES | ACTION_NAMES | {'visual-theme-report.json'}
            or not directory.is_absolute() or directory.name != DIRECTORY or directory.is_symlink()
            or not directory.is_dir() or directory.stat().st_uid != os.getuid()):
        raise Stop('closed_owned_output_required')
    raw = (json.dumps(data, ensure_ascii=False, separators=(',', ':'))+'\n').encode()
    if len(raw) > limit or sum(p.stat().st_size for p in directory.iterdir()) + len(raw) + 16384 > 15728640:
        raise Stop('fixed_artifact_budget_required')
    with (directory/name).open('xb') as f:
        os.fchmod(f.fileno(), 0o600); f.write(raw)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('app-pid','window-id'):
        p.add_argument('--'+name,type=int,required=True)
    for name in ('work-dir','output','ui-approval'):
        p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--deadline-monotonic',type=float,required=True)
    p.add_argument('--private-accessibility-bus',action='store_true')
    p.add_argument('--intent',choices=('observe','open-settings','dark-theme','close-settings'),required=True)
    p.add_argument('--mode',choices=('before','settings'),required=True)
    p.add_argument('--output-name', choices=tuple(sorted(PUBLIC_NAMES)))
    p.add_argument('--width',type=int,default=1280)
    p.add_argument('--height',type=int,default=900)
    args = p.parse_args()
    end = min(args.deadline_monotonic, time.monotonic()+10)
    record = dict(scope=SCOPE, Settings_Action_attempted=False, Settings_Action_returned_true=False,
        screenshots=[], field_values_read=False, intent=args.intent, status='blocked',
        app_pid=args.app_pid, window=args.window_id, fresh_before_guard_passes=[])
    def command(argv,binary=False,search=False):
        left = end-time.monotonic()
        if left < .25:
            raise Stop('deadline_no_input_or_read')
        result = subprocess.run(argv,capture_output=True,text=not binary,timeout=min(3,left))
        if result.returncode and not (search and result.returncode == 1):
            raise Stop('owned_command_failed_raw_withheld')
        return result.stdout
    def guard():
        owned_window(command,args.app_pid,args.window_id,args.width,args.height)
        current = collect(args.app_pid,min(end,time.monotonic()+2),'before')
        entry_target(current,args.app_pid)
        page_gate(current,args.app_pid,guard_data,'entry')
        if not rgb_matches(command(['import','-window',str(args.window_id),'-depth','8','rgb:-'],binary=True),ui['settings_guard']):
            raise Stop('fresh_Settings_button_RGB_required')
        record['fresh_before_guard_passes'].append({'metadata_sha256': hashlib.sha256(
            json.dumps(current,ensure_ascii=False,separators=(',',':')).encode()).hexdigest(),
            'region':ui['settings_guard']['region'],'rgb_sha256':ui['settings_guard']['rgb_sha256'],
            'owned_window_focus':True})
    def resolve(target):
        current = collect(args.app_pid,min(end,time.monotonic()+2),'before')
        page_gate(current,args.app_pid,guard_data,'entry')
        if entry_target(current,args.app_pid) != target:
            raise Stop('fresh_named_target_changed')
        root,A = target_root(args.app_pid,end)
        for i in target['path']:
            root = root.get_child_at_index(i)
            if root is None:
                raise Stop('fresh_named_target_missing')
        # Final subtree check before name access; no descendants with fields can be read.
        if root.get_child_count() != 0:
            raise Stop('named_Settings_leaf_required')
        return root,A
    try:
        if (not args.private_accessibility_bus or not os.environ.get('DISPLAY') or args.app_pid < 2
                or (Path('/proc')/str(args.app_pid)).stat().st_uid != os.getuid()
                or (Path('/proc')/str(args.app_pid)/'exe').resolve().parent != args.work_dir):
            raise Stop('owned_private_App_required')
        ui = read_decl(args.ui_approval)
        validate_ui(ui,Path(__file__).with_name('independent-qa.py'))
        owned_window(command,args.app_pid,args.window_id,args.width,args.height)
        guard_data=read_guard(ui)
        if (args.width,args.height) not in ((1024,900),(1280,900),(1440,900),(1280,720)):
            raise Stop('closed_matrix_dimensions_required')
        if args.intent in ('dark-theme','close-settings'):
            if (args.width,args.height)!=(1280,900) or args.mode!='settings' or args.output_name is not None:
                raise Stop('exact_settings_action_context_required')
            phase='light' if args.intent=='dark-theme' else 'dark'
            def state():
                owned_window(command,args.app_pid,args.window_id)
                data=collect(args.app_pid,min(end,time.monotonic()+2),'settings')
                return settings_gate(data,args.app_pid,guard_data,phase)
            target=state()
            def final_guard():
                if state()!=target: raise Stop('fresh_full_Settings_target_changed')
            def final_resolve(expected):
                final_guard();node,A=target_root(args.app_pid,end)
                for index in expected['path']:
                    node=node.get_child_at_index(index)
                    if node is None: raise Stop('fresh_named_target_missing')
                return node,A
            role=44 if args.intent=='dark-theme' else 43
            label='外观，深色' if args.intent=='dark-theme' else '关闭设置'
            settings_action_once(target,role,label,final_resolve,final_guard,record,end)
            record['status']='named_Settings_Action_returned_once'
            write_record(args.output,'04-dark-theme-action.json' if args.intent=='dark-theme' else '05-close-settings-action.json',record,8192)
        elif args.intent == 'open-settings':
            if args.mode != 'before' or args.output_name is not None or (args.width,args.height)!=(1280,900):
                raise Stop('closed_Settings_Action_intent_required')
            target = entry_target(collect(args.app_pid,min(end,time.monotonic()+2),'before'),args.app_pid)
            open_once(target,resolve,guard,record,end)
            record['status'] = 'Settings_Action_returned_once_no_theme_action'
            write_record(args.output,'02-settings-action.json',record,8192)
        else:
            if args.output_name is None or (args.output_name in ('03-settings-entry-public.json','04-dark-settings-public.json')) != (args.mode == 'settings'):
                raise Stop('closed_observation_mode_required')
            data = collect(args.app_pid,min(end,time.monotonic()+3),args.mode)
            write_record(args.output,args.output_name,data,131072)
            context(data,args.app_pid,before=args.mode == 'before',width=args.width,height=args.height)
    except Exception as exc:
        # Known errors are codes only; IPC/runtime exceptions may contain field strings.
        record['blocking_reason'] = str(exc) if isinstance(exc,Stop) else 'private_runtime_error_raw_withheld'
        filename={'open-settings':'02-settings-action.json','dark-theme':'04-dark-theme-action.json','close-settings':'05-close-settings-action.json'}.get(args.intent)
        if filename and not (args.output/filename).exists():
            write_record(args.output,filename,record,8192)
        return 2
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
