#!/usr/bin/env python3
"""One real AT-SPI click on freshly named assistant entry; no internal dispatch."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time

PHASES = frozenset({'private_context', 'load_controller', 'load_public_probe_module',
    'load_public_probe_dependency', 'collect_before', 'target_before', 'window_before',
    'capture_guard', 'RGB_guard', 'collect_final', 'target_final', 'resolve_node',
    'node_guard', 'action_guard', 'focus_final', 'dispatch', 'complete'})
CODES = frozenset({'controller_dependency_missing', 'controller_dependency_not_regular',
    'probe_module_dependency_missing', 'probe_module_dependency_not_regular',
    'dependency_name_not_allowed', 'private_owned_QA_context_required',
    'deadline_no_UI_command', 'actual_named_entry_unconfirmed', 'owned_window_or_focus_changed',
    'current_pixels_size_outside_bound', 'current_icon_RGB_differs', 'entry_changed_after_recollect',
    'final_node_role_name_bounds_state_unconfirmed', 'one_advertised_click_required',
    'focus_or_deadline_changed_before_dispatch', 'public_Action_returned_false',
    'public_probe_dependency_hash_or_file_unconfirmed'})


class ActionGuardStop(Exception):
    def __init__(self, code):
        self.code = code if code in CODES else 'unknown_guard_code_withheld'


def load(name):
    names = {'agent-native-entry-controller.py': 'controller',
             'agent-native-public-probe.py': 'probe_module'}
    if name not in names:
        raise ActionGuardStop('dependency_name_not_allowed')
    path = Path(__file__).with_name(name)
    if not path.exists():
        raise ActionGuardStop(names[name] + '_dependency_missing')
    if not path.is_file() or path.is_symlink() or path.stat().st_size > 64 * 1024:
        raise ActionGuardStop(names[name] + '_dependency_not_regular')
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), path)
    module = importlib.util.module_from_spec(spec)
    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
    return module


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--app-pid', type=int, required=True)
    p.add_argument('--window-id', type=int, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--deadline-monotonic', type=float, required=True)
    p.add_argument('--private-accessibility-bus', action='store_true')
    args = p.parse_args()
    report = {'schema': 'seecut-agent-native-entry-action-v1', 'action_attempted': False,
              'status': 'blocked', 'phase': 'private_context', 'field_values_read': False,
              'screenshots': [], 'product_verdict': 'not_tested'}
    end = min(args.deadline_monotonic, time.monotonic() + 8)
    def command(argv, raw=False, input=None):
        left = end - time.monotonic()
        if left < 0.1:
            raise ActionGuardStop('deadline_no_UI_command')
        return subprocess.run(argv, input=input, check=True, capture_output=True,
                              text=not raw, timeout=min(3, left)).stdout
    try:
        if (not args.private_accessibility_bus or not os.environ.get('DISPLAY') or args.app_pid < 2
                or not args.output.is_absolute() or args.output.is_symlink() or not args.output.is_dir()
                or args.output.stat().st_uid != os.getuid() or args.output.stat().st_mode & 0o777 != 0o700
                or (Path('/proc') / str(args.app_pid)).stat().st_uid != os.getuid()):
            raise ActionGuardStop('private_owned_QA_context_required')
        report['phase'] = 'load_controller'
        controller = load('agent-native-entry-controller.py')
        report['phase'] = 'load_public_probe_module'
        public_module = load('agent-native-public-probe.py')
        report['phase'] = 'load_public_probe_dependency'
        try:
            public = public_module.load_probe()
        except ValueError:
            raise ActionGuardStop('public_probe_dependency_hash_or_file_unconfirmed') from None
        report['phase'] = 'collect_before'
        data = public.collect(args.app_pid, min(end, time.monotonic() + 2))
        data.update(status='public_metadata_observed', field_values_read=False, ui_actions=[])
        report['phase'] = 'target_before'
        target = controller.entry_candidate(data)
        if target is None:
            raise ActionGuardStop('actual_named_entry_unconfirmed')
        report['phase'] = 'window_before'
        if (int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id):
            raise ActionGuardStop('owned_window_or_focus_changed')
        report['phase'] = 'capture_guard'
        # Current pixels stay in memory; no credential or configuration surface is read.
        pixels = command(['import', '-window', str(args.window_id), '-strip', 'png:-'], raw=True)
        if not 0 < len(pixels) <= 2 * 1024 * 1024:
            raise ActionGuardStop('current_pixels_size_outside_bound')
        report['phase'] = 'RGB_guard'
        rgb = command(['convert', 'png:-', '-crop', '44x44+18+726', '+repage',
                       '-alpha', 'off', '-depth', '8', 'rgb:-'], raw=True, input=pixels)
        if len(rgb) != 5808 or hashlib.sha256(rgb).hexdigest() != controller.ICON_RGB_SHA:
            raise ActionGuardStop('current_icon_RGB_differs')
        # Final current public tree and exact node state immediately precede Action.
        report['phase'] = 'collect_final'
        latest = public.collect(args.app_pid, min(end, time.monotonic() + 2))
        latest.update(status='public_metadata_observed', field_values_read=False, ui_actions=[])
        report['phase'] = 'target_final'
        if controller.entry_candidate(latest) != target:
            raise ActionGuardStop('entry_changed_after_recollect')
        report['phase'] = 'resolve_node'
        root, Atspi = public.target_root(args.app_pid, end)
        node = root.get_child_at_index(0).get_child_at_index(5)
        node.clear_cache_single()
        states = node.get_state_set()
        extent = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
        bounds = dict(x=int(extent.x), y=int(extent.y), width=int(extent.width), height=int(extent.height))
        report['phase'] = 'node_guard'
        if (node.get_role() != Atspi.Role.TOGGLE_BUTTON or node.get_name() != '助手'
                or bounds != controller.ICON_BOUNDS or bounds != target['bounds']
                or not all(states.contains(state) for state in (Atspi.StateType.SHOWING, Atspi.StateType.ENABLED, Atspi.StateType.SENSITIVE))
                or states.contains(Atspi.StateType.EDITABLE) or states.contains(Atspi.StateType.CHECKED)
                or 'EditableText' in set(node.get_interfaces()) or time.monotonic() >= end):
            raise ActionGuardStop('final_node_role_name_bounds_state_unconfirmed')
        report['phase'] = 'action_guard'
        action = node.get_action_iface()
        indices = [i for i in range(min(action.get_n_actions(), 8)) if action.get_action_name(i) == 'click']
        if len(indices) != 1:
            raise ActionGuardStop('one_advertised_click_required')
        report['phase'] = 'focus_final'
        if (int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id
                or time.monotonic() >= end):
            raise ActionGuardStop('focus_or_deadline_changed_before_dispatch')
        report['phase'] = 'dispatch'
        report['action_attempted'] = True
        if not action.do_action(indices[0]):
            raise ActionGuardStop('public_Action_returned_false')
        report['status'] = 'one_action_returned_result_pending'
        report['phase'] = 'complete'
    except ActionGuardStop as exc:
        report['blocking_reason'] = exc.code
    except subprocess.TimeoutExpired:
        report['blocking_reason'] = 'UI_command_deadline_no_retry'
    except subprocess.CalledProcessError:
        report['blocking_reason'] = 'UI_command_failed_no_retry'
    except Exception:
        report['blocking_reason'] = 'unexpected_OS_or_UI_error_raw_withheld'
    raw = (json.dumps(report, separators=(',', ':')) + '\n').encode()
    if args.output.is_absolute() and args.output.is_dir() and not args.output.is_symlink() and args.output.stat().st_uid == os.getuid():
        path = args.output / '04-entry-action.json'
        with path.open('xb') as stream:
            os.chmod(path, 0o600)
            stream.write(raw)
    print(json.dumps({k: report[k] for k in ('status', 'phase', 'action_attempted', 'blocking_reason') if k in report}))
    return 0 if report['status'] == 'one_action_returned_result_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
