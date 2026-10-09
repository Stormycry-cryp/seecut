#!/usr/bin/env python3
"""One public configuration entry/known close Action; metadata only, no pixels."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time

PHASES = frozenset({'private_context', 'load_controller', 'load_probe', 'read_UI_declaration',
    'collect_before', 'select_before', 'window_focus', 'collect_final', 'resolve_node',
    'final_role_state', 'final_label_bounds', 'advertised_action', 'focus_final', 'dispatch', 'complete'})
CODES = frozenset({'private_context_unconfirmed', 'named_dependency_missing_or_not_regular',
    'deadline_no_command', 'live_public_target_unconfirmed', 'owned_window_or_focus_changed',
    'live_public_target_changed', 'final_node_role_state_unconfirmed', 'final_label_bounds_unconfirmed',
    'one_advertised_click_required', 'deadline_before_dispatch', 'public_Action_returned_false'})


class Stop(Exception):
    def __init__(self, code):
        self.code = code if code in CODES else 'unknown_guard_code_withheld'


def load(name):
    if name not in ('agent-config-controller.py', 'agent-config-probe.py'):
        raise Stop('named_dependency_missing_or_not_regular')
    path = Path(__file__).with_name(name)
    if not path.is_file() or path.is_symlink() or path.stat().st_size > 64 * 1024:
        raise Stop('named_dependency_missing_or_not_regular')
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), path)
    module = importlib.util.module_from_spec(spec)
    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
    return module


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--app-pid', type=int, required=True)
    p.add_argument('--window-id', type=int, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--intent', choices=['open-settings', 'close-from-settings'], required=True)
    p.add_argument('--output-name', choices=['02-open-settings-action.json', '04-close-assistant-action.json'], required=True)
    p.add_argument('--deadline-monotonic', type=float, required=True)
    p.add_argument('--private-accessibility-bus', action='store_true')
    args = p.parse_args()
    report = {'schema': 'seecut-agent-config-action-v1', 'status': 'blocked', 'phase': 'private_context',
        'intent': args.intent, 'action_attempted': False, 'field_values_read': False, 'screenshots': [],
        'product_verdict': 'not_tested'}
    end = min(args.deadline_monotonic, time.monotonic() + 8)

    def command(argv):
        left = end - time.monotonic()
        if left < 0.1:
            raise Stop('deadline_no_command')
        return subprocess.run(argv, check=True, capture_output=True, text=True, timeout=min(3, left)).stdout

    def focus():
        if (int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id):
            raise Stop('owned_window_or_focus_changed')

    try:
        names = {'open-settings': '02-open-settings-action.json', 'close-from-settings': '04-close-assistant-action.json'}
        if (names[args.intent] != args.output_name or not args.private_accessibility_bus or not os.environ.get('DISPLAY')
                or args.app_pid < 2 or not args.output.is_absolute() or not args.output.is_dir() or args.output.is_symlink()
                or args.output.stat().st_uid != os.getuid() or args.output.stat().st_mode & 0o777 != 0o700
                or (Path('/proc') / str(args.app_pid)).stat().st_uid != os.getuid()):
            raise Stop('private_context_unconfirmed')
        report['phase'] = 'load_controller'
        controller = load('agent-config-controller.py')
        report['phase'] = 'load_probe'
        public = load('agent-config-probe.py').load_probe()
        report['phase'] = 'read_UI_declaration'
        ui = controller.read_ui(Path(__file__).with_name('agent-config-ui.json'))
        report['phase'] = 'collect_before'
        data = public.collect(args.app_pid, min(end, time.monotonic() + 2))
        data.update(status='public_metadata_observed', field_values_read=False, ui_actions=[])
        report['phase'] = 'select_before'
        target = controller.action_target(data, ui, args.intent)
        if target is None:
            raise Stop('live_public_target_unconfirmed')
        report['phase'] = 'window_focus'
        focus()
        report['phase'] = 'collect_final'
        data = public.collect(args.app_pid, min(end, time.monotonic() + 2))
        data.update(status='public_metadata_observed', field_values_read=False, ui_actions=[])
        if controller.action_target(data, ui, args.intent) != target:
            raise Stop('live_public_target_changed')
        report['phase'] = 'resolve_node'
        root, Atspi = public.target_root(args.app_pid, end)
        node = root
        for index in target['path']:
            node = node.get_child_at_index(index)
        node.clear_cache_single()
        states = node.get_state_set()
        interfaces = set(node.get_interfaces())
        report['phase'] = 'final_role_state'
        # Reject editable nodes before get_name. Fields never disclose their names.
        if (node.get_role() != Atspi.Role.PUSH_BUTTON or states.contains(Atspi.StateType.EDITABLE)
                or 'EditableText' in interfaces or 'Action' not in interfaces
                or not all(states.contains(s) for s in (Atspi.StateType.SHOWING, Atspi.StateType.ENABLED, Atspi.StateType.SENSITIVE))):
            raise Stop('final_node_role_state_unconfirmed')
        report['phase'] = 'final_label_bounds'
        extent = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
        bounds = dict(x=int(extent.x), y=int(extent.y), width=int(extent.width), height=int(extent.height))
        if node.get_name() != target['label'] or bounds != target['bounds']:
            raise Stop('final_label_bounds_unconfirmed')
        report['phase'] = 'advertised_action'
        action = node.get_action_iface()
        actions = [i for i in range(min(action.get_n_actions(), 8)) if action.get_action_name(i) == 'click']
        if len(actions) != 1:
            raise Stop('one_advertised_click_required')
        report['phase'] = 'focus_final'
        focus()
        if time.monotonic() >= end:
            raise Stop('deadline_before_dispatch')
        report['phase'] = 'dispatch'
        report['action_attempted'] = True
        if not action.do_action(actions[0]):
            raise Stop('public_Action_returned_false')
        report['status'] = 'one_action_returned_result_pending'
        report['phase'] = 'complete'
    except Stop as exc:
        report['blocking_reason'] = exc.code
    except subprocess.TimeoutExpired:
        report['blocking_reason'] = 'owned_command_deadline_no_retry'
    except subprocess.CalledProcessError:
        report['blocking_reason'] = 'owned_command_failed_no_retry'
    except Exception:
        report['blocking_reason'] = 'unexpected_OS_or_UI_error_raw_withheld'
    raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
    if args.output.is_absolute() and args.output.is_dir() and not args.output.is_symlink() and args.output.stat().st_uid == os.getuid():
        path = args.output / args.output_name
        with path.open('xb') as stream:
            os.chmod(path, 0o600); stream.write(raw)
    print(json.dumps({k: report[k] for k in ('status', 'phase', 'blocking_reason') if k in report}))
    return 0 if report['status'] == 'one_action_returned_result_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
