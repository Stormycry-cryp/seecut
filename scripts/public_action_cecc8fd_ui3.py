#!/usr/bin/env python3
"""One bounded public Action on a freshly named, healthy, owned UI control.

Only empty-canvas creation, professional mode, dark theme and Settings close.
No fields, coordinates, unknown names, copy, grant, file, client or paid actions.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time

from public_probe_cecc8fd_ui3 import collect, owned_by_app, target_root, safe_label

HEAD = 'cecc8fdf9578675051dae58bda25f0ff805ce235'
APP_SHA = '8859b10e7b8a58785d6a60454995f33d56efea554011825287917d06064aff55'
LABELS = {
    'professional': {'专业', '专业模式', 'Professional', 'Professional mode'},
    'quick': {'快速', '快速模式', 'Quick', 'Quick mode'},
    'dark': {'深色', '深色主题', '深色模式', 'Dark', 'Dark theme'},
    'light': {'浅色', '浅色主题', '浅色模式', 'Light', 'Light theme'},
    'close-settings': {'关闭', '关闭设置', 'Close', 'Close settings', 'Close Settings'},
    'create-blank': {'创建'},
}
KNOWN_CONTAINERS = {
    'mode': ('工作模式', {'x': 596, 'y': 147, 'width': 188, 'height': 36}),
    'theme': ('外观', {'x': 596, 'y': 222, 'width': 188, 'height': 36}),
}


def healthy(node):
    return (all(node.get(k) for k in ('showing', 'enabled', 'sensitive'))
            and not node.get('entry') and not node.get('editable')
            and not node.get('editable_text_interface'))


def container(nodes, kind):
    label, bounds = KNOWN_CONTAINERS[kind]
    found = [n for n in nodes if healthy(n) and n.get('role') == 39
             and n.get('label') == label and n.get('bounds') == bounds]
    return found[0] if len(found) == 1 else None


def settings_visible(data):
    return bool(container(data.get('nodes', []), 'mode')
                and container(data.get('nodes', []), 'theme'))


def select_target(data, intent):
    """Pure selector: observed labels determine meaning; positions never do."""
    if (data.get('status') != 'public_metadata_observed'
            or not data.get('coverage_complete')):
        return None
    nodes = data.get('nodes', [])
    if intent in ('professional', 'dark', 'quick', 'light'):
        kind = 'mode' if intent in ('professional', 'quick') else 'theme'
        parent = container(nodes, kind)
        if not settings_visible(data) or not parent:
            return None
        children = [n for n in nodes if n.get('path', [])[:-1] == parent['path']
                    and n.get('role') == 44 and healthy(n)]
        pair = ('quick', 'professional') if kind == 'mode' else ('light', 'dark')
        # Both alternatives must have actual names and exactly one checked state.
        if (len(children) != 2 or sum(bool(n.get('checked')) for n in children) != 1
                or any(sum(n.get('label') in LABELS[p] for n in children) != 1 for p in pair)):
            return None
        candidates = [n for n in children if n.get('label') in LABELS[intent]]
    elif intent == 'close-settings':
        if not settings_visible(data):
            return None
        # A1 observed this button, but its function is unknown until its own name is safe.
        candidates = [n for n in nodes if n.get('role') == 43 and healthy(n)
                      and n.get('label') in LABELS[intent]
                      and n.get('bounds') == {'x': 1216, 'y': 68, 'width': 26, 'height': 128}]
    elif intent == 'create-blank':
        if settings_visible(data):
            return None
        # All three must share an actually exposed local dialog/modal or named panel.
        # The whole App/window, sibling names or geometry alone are not local context.
        contexts = [n for n in nodes if n.get('showing') and n.get('path')
                    and not any(n.get(k) for k in ('entry', 'editable', 'editable_text_interface'))
                    and (n.get('dialog') or (len(n['path']) >= 2 and (n.get('modal')
                         or (n.get('role') == 39 and n.get('label') == '新建画布'))))]
        candidates = []
        for context in contexts:
            prefix = context['path']
            local = [n for n in nodes if n.get('path', [])[:len(prefix)] == prefix]
            title = [n for n in local if n.get('showing') and n.get('role') == 29
                     and n.get('label') == '新建画布' and not n.get('editable')
                     and not n.get('editable_text_interface')]
            cancel = [n for n in local if n.get('role') == 43 and healthy(n) and n.get('label') == '取消']
            create = [n for n in local if n.get('role') == 43 and healthy(n) and n.get('label') == '创建']
            if len(title) == len(cancel) == len(create) == 1:
                if not any(n['path'] == create[0]['path'] for n in candidates):
                    candidates.append(create[0])
    else:
        return None
    candidates = [n for n in candidates if n.get('action_interface')
                  and any(a in n.get('allowed_actions', []) for a in ('click', 'activate', 'press'))]
    return candidates[0] if len(candidates) == 1 else None


def regular_json(path):
    if (not path.is_absolute() or path.is_symlink() or not path.is_file()
            or path.stat().st_size > 16384):
        raise RuntimeError()
    return json.loads(path.read_bytes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-pid', type=int, required=True)
    parser.add_argument('--window-bounds', type=int, nargs=4, required=True)
    parser.add_argument('--intent', choices=['professional', 'dark', 'close-settings', 'create-blank'], required=True)
    parser.add_argument('--target-path', required=True)
    parser.add_argument('--expected-label', required=True)
    parser.add_argument('--identity-approval', type=Path, required=True)
    parser.add_argument('--ui-approval', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--private-accessibility-bus', action='store_true')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--output-name', required=True)
    args = parser.parse_args()
    start = time.monotonic()
    deadline = min(args.deadline_monotonic, start + 4)
    report = {'status': 'blocked', 'intent': args.intent, 'field_values_read': False,
              'action_attempted': False, 'action_call_returned': False, 'product_verdict': 'pending_actual_result'}
    try:
        if not args.private_accessibility_bus or not os.environ.get('DISPLAY') or args.app_pid < 2:
            raise RuntimeError()
        if not owned_by_app(args.app_pid, args.app_pid):
            raise RuntimeError()
        identity, ui = regular_json(args.identity_approval), regular_json(args.ui_approval)
        if (identity.get('schema') != 2 or identity.get('reviewed_by') != 'main-reviewer'
                or identity.get('runtime_head') != HEAD or identity.get('runtime_app_sha256') != APP_SHA
                or identity.get('change_scope') != 'product-candidate'
                or ui.get('schema') != 1 or ui.get('reviewed_by') != 'independent-qa'
                or ui.get('observed_head') != HEAD or ui.get('observed_app_sha256') != APP_SHA
                or ui.get('stage') != 'a2-conditional-public-controls'
                or args.intent not in ui.get('public_action_intents', [])):
            raise RuntimeError()
        helper = Path(__file__).with_name('public_probe_cecc8fd_ui3.py')
        if hashlib.sha256(helper.read_bytes()).hexdigest() != ui.get('public_probe_sha256'):
            raise RuntimeError()
        digest = hashlib.sha256()
        with (Path('/proc') / str(args.app_pid) / 'exe').open('rb') as stream:
            while True:
                if time.monotonic() >= deadline:
                    raise RuntimeError()
                chunk = stream.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
        if digest.hexdigest() != APP_SHA:
            raise RuntimeError()
        data = collect(args.app_pid, deadline)
        data['status'] = 'public_metadata_observed'
        node = select_target(data, args.intent)
        path = json.loads(args.target_path)
        if (node is None or node['path'] != path or node.get('label') != args.expected_label
                or not isinstance(path, list) or not all(type(i) is int and 0 <= i < 128 for i in path)):
            raise RuntimeError()
        x, y, w, h = args.window_bounds
        b = node.get('bounds', {})
        if (w != 1280 or h != 900 or x < 0 or y < 0
                or not all(type(b.get(k)) is int for k in ('x', 'y', 'width', 'height'))
                or b['width'] <= 0 or b['height'] <= 0 or b['x'] < x or b['y'] < y
                or b['x'] + b['width'] > x + w or b['y'] + b['height'] > y + h):
            raise RuntimeError()
        # Recollect the entire public context immediately before final node access.
        # A matching child alone cannot preserve a changed parent or checked pair.
        final_data = collect(args.app_pid, deadline)
        final_data['status'] = 'public_metadata_observed'
        final_node = select_target(final_data, args.intent)
        if (final_node is None or any(final_node.get(k) != node.get(k)
                for k in ('path', 'label', 'role', 'bounds', 'checked'))
                or time.monotonic() >= deadline):
            raise RuntimeError()
        node = final_node
        root, Atspi = target_root(args.app_pid, deadline)
        current = root
        for index in path:
            current = current.get_child_at_index(index)
            if current is None:
                raise RuntimeError()
        current.clear_cache_single()
        states = current.get_state_set()
        component = current.get_component_iface()
        extent = component.get_extents(Atspi.CoordType.SCREEN)
        fresh_bounds = {'x': int(extent.x), 'y': int(extent.y), 'width': int(extent.width), 'height': int(extent.height)}
        interfaces = set(current.get_interfaces())
        if (int(current.get_role()) != node['role'] or 'EditableText' in interfaces
                or states.contains(Atspi.StateType.EDITABLE)
                or (node['role'] == 44 and bool(states.contains(Atspi.StateType.CHECKED)) != bool(node.get('checked')))
                or safe_label(current.get_name()) != node['label'] or fresh_bounds != b
                or not all(states.contains(getattr(Atspi.StateType, k)) for k in ('SHOWING', 'ENABLED', 'SENSITIVE'))
                or time.monotonic() >= deadline):
            raise RuntimeError()
        action = current.get_action_iface()
        indices = [i for i in range(min(action.get_n_actions(), 8))
                   if action.get_action_name(i) in ('click', 'activate', 'press')]
        if len(indices) != 1:
            raise RuntimeError()
        report.update(path=path, label=node['label'], action_attempted=True)
        # Once only, including failure/timeout; the caller must never resend this action.
        returned = bool(action.do_action(indices[0]))
        report['action_call_returned'] = returned
        report['status'] = 'one_action_returned_result_pending' if returned else 'action_failed_no_retry'
    except Exception:
        report['blocking_reason'] = 'public_identity_name_context_or_action_gate_failed_no_retry'
    report['elapsed_seconds'] = round(time.monotonic() - start, 3)
    raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
    if (not args.output.is_absolute() or args.output.is_symlink() or not args.output.is_dir()
            or args.output.stat().st_uid != os.getuid() or len(args.output_name) > 80
            or not args.output_name.endswith('.json') or '..' in args.output_name
            or any(c not in 'abcdefghijklmnopqrstuvwxyz0123456789-.' for c in args.output_name)):
        return 2
    path = args.output / args.output_name
    with path.open('xb') as stream:
        os.chmod(path, 0o600)
        stream.write(raw)
    print(raw.decode(), end='')
    return 0 if report['status'] == 'one_action_returned_result_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
