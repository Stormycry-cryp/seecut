#!/usr/bin/env python3
"""Clip import only: one guarded public GTK location write or accept. No field reads."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time

HEAD = '0aa9406247e53f073c0b4df686adc68b40e2f8f6'
APP_SHA = '03fb1752c34adb0c0f4a21521c2302ee80c9b23203728cbcf636d3a4a57eaea2'
FIXTURE_SHA = '0928c47fa44250879270def6198e04fd939dd8250864179760203f0d334a6d63'
FOOTER_SHA = '4ca359d14fbaec11a20a908116d131c055db707227550009603c0a832d8035ef'


def allowed_fixture(path, inputs):
    if (not inputs.is_absolute() or inputs.is_symlink() or not inputs.is_dir()
            or inputs.stat().st_uid != os.getuid() or path != inputs / 'opaque-quadrants.png'
            or path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid()
            or path.stat().st_size != 800 or hashlib.sha256(path.read_bytes()).hexdigest() != FIXTURE_SHA):
        raise ValueError('exact_owned_fixture_required')


def load_probe(ui):
    helper = Path(__file__).with_name('clip-editor-public-probe.py')
    if (ui.get('scope') != 'clip-media-import' or ui.get('public_probe_filename') != helper.name
            or helper.is_symlink() or not helper.is_file() or helper.stat().st_size > 65536
            or hashlib.sha256(helper.read_bytes()).hexdigest() != ui.get('public_probe_sha256')):
        raise ValueError('exact_clip_probe_required')
    spec = importlib.util.spec_from_file_location('owned_asset_public_probe', helper)
    probe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(probe)
    return probe


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('target-pid', 'owned-root-pid', 'window-id'):
        p.add_argument('--' + name, type=int, required=True)
    for name in ('owned-path', 'input-dir', 'ui-approval'):
        p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--node-public', required=True)
    p.add_argument('--window-bounds', required=True)
    p.add_argument('--mode', choices=('set-location', 'accept'), required=True)
    p.add_argument('--deadline-monotonic', type=float, required=True)
    p.add_argument('--private-accessibility-bus', action='store_true')
    args = p.parse_args()
    report = {'success': False, 'scope': 'clip-media-import', 'mode': args.mode,
              'field_values_read': False, 'file_lists_read': False, 'screenshots': []}
    end = min(args.deadline_monotonic, time.monotonic() + 3)

    def command(argv, binary=False):
        left = end - time.monotonic()
        if left <= 0.25:
            raise ValueError('deadline_no_action')
        return subprocess.run(argv, check=True, capture_output=True, text=not binary,
                              timeout=min(left, 2)).stdout

    try:
        if not args.private_accessibility_bus or not os.environ.get('DISPLAY'):
            raise ValueError('private_bus_required')
        path = args.ui_approval
        if not path.is_absolute() or path.is_symlink() or not path.is_file() or path.stat().st_size > 65536:
            raise ValueError('reviewed_declaration_required')
        ui = json.loads(path.read_bytes())
        if (ui.get('schema') != 1 or ui.get('reviewed_by') != 'main-reviewer'
                or ui.get('scope') != 'clip-media-import' or ui.get('observed_head') != HEAD
                or ui.get('observed_app_sha256') != APP_SHA
                or ui.get('native_action_sha256') != hashlib.sha256(Path(__file__).read_bytes()).hexdigest()):
            raise ValueError('exact_scope_identity_required')
        probe = load_probe(ui)
        if args.target_pid == args.owned_root_pid or not probe.owned_by_app(args.target_pid, args.owned_root_pid):
            raise ValueError('same_user_owned_native_descendant_required')
        allowed_fixture(args.owned_path, args.input_dir)
        if (args.input_dir.name != 'asset-clip-inputs' or args.input_dir.stat().st_mode & 0o777 != 0o700
                or set(p.name for p in args.input_dir.iterdir()) != {'opaque-quadrants.png'}):
            raise ValueError('exact_single_fixture_directory_required')
        if int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.target_pid:
            raise ValueError('native_pid_changed')
        ids = command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.target_pid)]).split()
        if set(ids) != {str(args.window_id)} or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id:
            raise ValueError('one_owned_focused_native_window_required')
        bounds = json.loads(args.window_bounds)
        if bounds != ui['native_window_bounds']:
            raise ValueError('exact_reviewed_native_geometry_required')
        fields = dict(line.split('=', 1) for line in command(
            ['xdotool', 'getwindowgeometry', '--shell', str(args.window_id)]).splitlines() if '=' in line)
        if [int(fields[k]) for k in ('X', 'Y', 'WIDTH', 'HEIGHT')] != bounds:
            raise ValueError('native_geometry_changed')
        data = probe.collect(args.target_pid, end)
        nodes = data.get('nodes', [])
        if data.get('toolkit') != 'GTK' or data.get('coverage_complete') is not True:
            raise ValueError('complete_GTK_required')
        if len([n for n in nodes if n.get('showing') and n.get('dialog') and n.get('path') == [0]]) != 1:
            raise ValueError('current_GTK_dialog_required')
        columns = ui['public_node_columns']
        full = [[n.get(k, False if k == 'pressed' else None) for k in columns] for n in nodes]
        if full != ui['native_location_public_nodes']:
            raise ValueError('complete_current_location_template_changed')
        expected = json.loads(args.node_public)
        current = [n for n in nodes if n.get('path') == expected.get('path')]
        if len(current) != 1 or current[0] != expected:
            raise ValueError('current_public_node_changed')
        node = current[0]
        if not node.get('showing') or not (node.get('enabled') or node.get('sensitive')):
            raise ValueError('current_node_not_available')
        width, height = bounds[2:]
        raw = command(['import', '-window', str(args.window_id), '-depth', '8', 'rgb:-'], binary=True)
        if len(raw) != width * height * 3 or width < 157 or height < 46:
            raise ValueError('native_RGB_size_changed')
        crop = b''.join(raw[(y * width + width - 157) * 3:(y * width + width - 12) * 3]
                        for y in range(height - 46, height - 12))
        if hashlib.sha256(crop).hexdigest() != FOOTER_SHA:
            raise ValueError('current_native_footer_changed')
        root, Atspi = probe.target_root(args.target_pid, end)
        target = root
        indices = expected['path']
        if not indices or len(indices) > 24 or any(type(i) is not int or not 0 <= i < 128 for i in indices):
            raise ValueError('bounded_numeric_path_required')
        for index in indices:
            if time.monotonic() >= end or target.get_role() in (
                    Atspi.Role.TABLE, Atspi.Role.TREE, Atspi.Role.TREE_TABLE, Atspi.Role.LIST, Atspi.Role.DIRECTORY_PANE):
                raise ValueError('deadline_or_file_list_forbidden')
            target = target.get_child_at_index(index)
            if target is None:
                raise ValueError('current_target_missing')
        target.clear_cache_single()
        states = target.get_state_set()
        interfaces = set(target.get_interfaces())
        if (not states.contains(Atspi.StateType.SHOWING)
                or not (states.contains(Atspi.StateType.ENABLED) or states.contains(Atspi.StateType.SENSITIVE))
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id
                or time.monotonic() >= end):
            raise ValueError('current_state_focus_deadline_required')
        if args.mode == 'set-location':
            if not node.get('entry') or not node.get('focused') or 'EditableText' not in interfaces:
                raise ValueError('current_focused_editable_entry_required')
            report['success'] = bool(target.get_editable_text_iface().set_text_contents(str(args.owned_path)))
        else:
            candidates = [n for n in nodes if n.get('showing') and n.get('button')
                          and n.get('label') in ('OK', 'Ok', 'Open', '打开') and n.get('action_interface')
                          and (n.get('enabled') or n.get('sensitive')) and n.get('allowed_actions') == ['click']]
            if len(candidates) != 1 or candidates[0] != node or target.get_role() != Atspi.Role.PUSH_BUTTON:
                raise ValueError('unique_current_accept_required')
            # Name is a noneditable closed label already read by the bounded probe.
            if target.get_name() not in ('OK', 'Ok', 'Open', '打开') or 'Action' not in interfaces:
                raise ValueError('current_advertised_accept_required')
            action = target.get_action_iface()
            allowed = [i for i in range(min(action.get_n_actions(), 8)) if action.get_action_name(i) == 'click']
            if len(allowed) != 1 or time.monotonic() >= end:
                raise ValueError('one_advertised_action_and_deadline_required')
            report['success'] = bool(action.do_action(allowed[0]))
    except Exception:
        report['blocking_reason'] = 'owned_native_UI_action_unconfirmed_raw_withheld'
    print(json.dumps(report, ensure_ascii=False, separators=(',', ':')))
    return 0 if report['success'] else 2


if __name__ == '__main__':
    raise SystemExit(main())
