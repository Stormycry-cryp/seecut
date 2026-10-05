#!/usr/bin/env python3
"""One finite clip-dialog field input or create Action, never reads field values."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time


def load_verified(name, digest):
    if name not in ('clip-editor-controller.py', 'clip-editor-public-probe.py'):
        raise ValueError('exact_named_dependency_required')
    path = Path(__file__).with_name(name)
    if (path.is_symlink() or not path.is_file() or path.stat().st_size > 65536
            or hashlib.sha256(path.read_bytes()).hexdigest() != digest):
        raise ValueError('named_dependency_SHA_required')
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def field_sequence(intent, value, target, focus, observe, command, guard, record):
    """Current exact metadata + chrome -> focus -> fresh proof -> select -> fresh proof -> type."""
    if intent not in ('name', 'path'):
        raise ValueError('exact_field_intent_required')
    record['phase'] = 'field_before_focus_click'
    focus(); guard()
    bounds = target['bounds']
    xy = [bounds['x'] + bounds['width'] // 2, bounds['y'] + bounds['height'] // 2]
    command(['xdotool', 'mousemove', '--window', str(record['window']), *map(str, xy)])
    focus()
    record['field_focus_click_attempted'] = True
    command(['xdotool', 'click', '1'])
    record['phase'] = 'field_focused_before_select'
    observe(intent, focused=True)
    focus(); guard()
    record['select_all_attempted'] = True
    command(['xdotool', 'key', '--clearmodifiers', 'ctrl+a'])
    record['phase'] = 'field_focused_before_type'
    observe(intent, focused=True)
    focus(); guard()
    record['typed_owned_value_attempted'] = True
    command(['xdotool', 'type', '--clearmodifiers', '--delay', '1', '--', value])
    record['phase'] = 'field_focused_after_type'
    observe(intent, focused=True)


def create_once(target, resolve, focus, guard, record, deadline):
    """No loop/retry: one advertised create Action after final target/state/focus guards."""
    record['phase'] = 'create_final_target_guard'
    focus(); guard()
    node, Atspi = resolve(target)
    states = node.get_state_set()
    interfaces = set(node.get_interfaces())
    if (node.get_role() != Atspi.Role.PUSH_BUTTON or states.contains(Atspi.StateType.EDITABLE)
            or 'EditableText' in interfaces or 'Action' not in interfaces
            or not all(states.contains(s) for s in (Atspi.StateType.SHOWING, Atspi.StateType.ENABLED, Atspi.StateType.SENSITIVE))):
        raise ValueError('final_create_state_required')
    # Name is read only on this positively noneditable closed-label button.
    if node.get_name() != '创建':
        raise ValueError('exact_public_create_label_required')
    extent = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
    if {'x': int(extent.x), 'y': int(extent.y), 'width': int(extent.width), 'height': int(extent.height)} != target['bounds']:
        raise ValueError('final_create_bounds_required')
    action = node.get_action_iface()
    allowed = [i for i in range(min(action.get_n_actions(), 8)) if action.get_action_name(i) == 'click']
    if len(allowed) != 1:
        raise ValueError('one_advertised_create_action_required')
    focus(); guard()
    if time.monotonic() >= deadline:
        raise ValueError('deadline_before_create')
    record['phase'] = 'create_Action_once'
    record['create_Action_attempted'] = True
    if not action.do_action(allowed[0]):
        raise ValueError('create_Action_returned_false_no_retry')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('app-pid', 'window-id'):
        p.add_argument('--' + name, type=int, required=True)
    for name in ('work-dir', 'ui-approval'):
        p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--intent', choices=('name', 'path', 'create'), required=True)
    p.add_argument('--deadline-monotonic', type=float, required=True)
    p.add_argument('--private-accessibility-bus', action='store_true')
    args = p.parse_args()
    end = min(args.deadline_monotonic, time.monotonic() + 10)
    record = {'scope': 'clip-editor-entry', 'intent': args.intent, 'success': False,
              'field_values_read': False, 'window': args.window_id, 'screenshots': [],
              'phase': 'private_context_and_dependencies',
              'field_focus_click_attempted': False, 'select_all_attempted': False,
              'typed_owned_value_attempted': False, 'create_Action_attempted': False}

    def command(argv, binary=False, search=False):
        left = end - time.monotonic()
        if left < 0.1:
            raise ValueError('deadline_no_input')
        result = subprocess.run(argv, capture_output=True, text=not binary, timeout=min(3, left))
        if result.returncode and not (search and result.returncode == 1):
            raise ValueError('owned_command_failed_no_retry')
        return result.stdout

    def focus():
        if (int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id
                or set(command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)], search=True).split()) != {str(args.window_id)}):
            raise ValueError('exact_App_window_and_focus_required')
        pending, seen = [args.app_pid], {args.app_pid}
        while pending:
            tasks = list((Path('/proc') / str(pending.pop()) / 'task').iterdir())
            if len(tasks) > 64:
                raise ValueError('owned_task_limit')
            for task in tasks:
                try:
                    children = list(map(int, (task / 'children').read_text().split()))
                except FileNotFoundError:
                    continue
                for child in children:
                    if child not in seen and controller.owned_descendant(child, args.app_pid):
                        if len(seen) >= 24:
                            raise ValueError('owned_descendant_limit')
                        seen.add(child); pending.append(child)
                        if command(['xdotool', 'search', '--onlyvisible', '--pid', str(child)], search=True).strip():
                            raise ValueError('owned_native_window_forbidden')

    def observe(intent, focused=False):
        focus()
        data = probe.collect(args.app_pid, min(end, time.monotonic() + 2))
        data.update(status='public_metadata_observed', app_pid=args.app_pid, field_values_read=False, ui_actions=[])
        return controller.dialog_target(data, ui, args.app_pid, intent, focused=focused)

    def guard():
        focus()
        raw = command(['import', '-window', str(args.window_id), '-depth', '8', 'rgb:-'], binary=True)
        if not controller.masked_dialog_matches(raw, ui):
            raise ValueError('current_dialog_chrome_changed')

    def resolve(target):
        current = observe('create')
        if current != target:
            raise ValueError('current_create_metadata_changed')
        root, Atspi = probe.target_root(args.app_pid, end)
        node = root
        for index in target['path']:
            if time.monotonic() >= end:
                raise ValueError('deadline_resolving_create')
            node = node.get_child_at_index(index)
            if node is None:
                raise ValueError('current_create_missing')
        node.clear_cache_single()
        return node, Atspi

    try:
        path = args.ui_approval
        if (not args.private_accessibility_bus or not os.environ.get('DISPLAY') or args.app_pid < 2
                or not path.is_absolute() or path.is_symlink() or not path.is_file() or path.stat().st_size > 16384
                or (Path('/proc') / str(args.app_pid)).stat().st_uid != os.getuid()):
            raise ValueError('exact_owned_private_context_required')
        ui = json.loads(path.read_bytes())
        if ui.get('action_sha256') != hashlib.sha256(Path(__file__).read_bytes()).hexdigest():
            raise ValueError('exact_action_SHA_required')
        controller = load_verified('clip-editor-controller.py', ui.get('controller_sha256'))
        controller.validate_ui(ui)
        controller.runtime_dependencies(ui)
        if (Path('/proc') / str(args.app_pid) / 'exe').resolve().parent != args.work_dir:
            raise ValueError('exact_launcher_App_path_required')
        probe = load_verified('clip-editor-public-probe.py', ui.get('public_probe_sha256'))
        project_output = controller.owned_project_output(args.work_dir)
        if list(project_output.iterdir()):
            raise ValueError('fresh_empty_owned_project_destination_required')
        record['phase'] = 'initial_target_observation'
        target = observe(args.intent)
        if args.intent in ('name', 'path'):
            value = 'qa-clip-project' if args.intent == 'name' else str(project_output)
            field_sequence(args.intent, value, target, focus, observe, command, guard, record)
        else:
            create_once(target, resolve, focus, guard, record, end)
        record['phase'] = 'completed'
        record['success'] = True
    except Exception:
        record['blocking_reason'] = 'clip_dialog_input_unconfirmed_raw_withheld_no_retry'
    print(json.dumps(record, ensure_ascii=False, separators=(',', ':')))
    return 0 if record['success'] else 2


if __name__ == '__main__':
    raise SystemExit(main())
