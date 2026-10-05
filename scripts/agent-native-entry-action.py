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


def load(name):
    path = Path(__file__).with_name(name)
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
              'status': 'blocked', 'field_values_read': False, 'screenshots': [], 'product_verdict': 'not_tested'}
    end = min(args.deadline_monotonic, time.monotonic() + 8)
    def command(argv, raw=False, input=None):
        left = end - time.monotonic()
        if left < 0.1:
            raise ValueError()
        return subprocess.run(argv, input=input, check=True, capture_output=True,
                              text=not raw, timeout=min(3, left)).stdout
    try:
        if (not args.private_accessibility_bus or not os.environ.get('DISPLAY') or args.app_pid < 2
                or not args.output.is_absolute() or args.output.is_symlink() or not args.output.is_dir()
                or args.output.stat().st_uid != os.getuid() or args.output.stat().st_mode & 0o777 != 0o700
                or (Path('/proc') / str(args.app_pid)).stat().st_uid != os.getuid()):
            raise ValueError()
        controller = load('agent-native-entry-controller.py')
        public = load('agent-native-public-probe.py').load_probe()
        data = public.collect(args.app_pid, min(end, time.monotonic() + 2))
        data.update(status='public_metadata_observed', field_values_read=False, ui_actions=[])
        target = controller.entry_candidate(data)
        if target is None:
            raise ValueError()
        if (int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id):
            raise ValueError()
        # Current pixels stay in memory; no credential or configuration surface is read.
        pixels = command(['import', '-window', str(args.window_id), '-strip', 'png:-'], raw=True)
        if not 0 < len(pixels) <= 2 * 1024 * 1024:
            raise ValueError()
        rgb = command(['convert', 'png:-', '-crop', '44x44+18+726', '+repage',
                       '-alpha', 'off', '-depth', '8', 'rgb:-'], raw=True, input=pixels)
        if len(rgb) != 5808 or hashlib.sha256(rgb).hexdigest() != controller.ICON_RGB_SHA:
            raise ValueError()
        # Final current public tree and exact node state immediately precede Action.
        latest = public.collect(args.app_pid, min(end, time.monotonic() + 2))
        latest.update(status='public_metadata_observed', field_values_read=False, ui_actions=[])
        if controller.entry_candidate(latest) != target:
            raise ValueError()
        root, Atspi = public.target_root(args.app_pid, end)
        node = root.get_child_at_index(0).get_child_at_index(5)
        node.clear_cache_single()
        states = node.get_state_set()
        extent = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
        bounds = dict(x=int(extent.x), y=int(extent.y), width=int(extent.width), height=int(extent.height))
        if (node.get_role() != Atspi.Role.TOGGLE_BUTTON or node.get_name() != '助手'
                or bounds != controller.ICON_BOUNDS or bounds != target['bounds']
                or not all(states.contains(state) for state in (Atspi.StateType.SHOWING, Atspi.StateType.ENABLED, Atspi.StateType.SENSITIVE))
                or states.contains(Atspi.StateType.EDITABLE) or states.contains(Atspi.StateType.CHECKED)
                or 'EditableText' in set(node.get_interfaces()) or time.monotonic() >= end):
            raise ValueError()
        action = node.get_action_iface()
        indices = [i for i in range(min(action.get_n_actions(), 8)) if action.get_action_name(i) == 'click']
        if len(indices) != 1:
            raise ValueError()
        if (int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id
                or time.monotonic() >= end):
            raise ValueError()
        report['action_attempted'] = True
        if not action.do_action(indices[0]):
            raise ValueError()
        report['status'] = 'one_action_returned_result_pending'
    except Exception:
        report['blocking_reason'] = 'entry_guard_unconfirmed_no_retry_raw_error_withheld'
    raw = (json.dumps(report, separators=(',', ':')) + '\n').encode()
    if args.output.is_absolute() and args.output.is_dir() and not args.output.is_symlink() and args.output.stat().st_uid == os.getuid():
        path = args.output / '04-entry-action.json'
        with path.open('xb') as stream:
            os.chmod(path, 0o600)
            stream.write(raw)
    print(json.dumps({'status': report['status'], 'action_attempted': report['action_attempted']}))
    return 0 if report['status'] == 'one_action_returned_result_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
