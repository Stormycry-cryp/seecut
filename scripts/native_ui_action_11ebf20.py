#!/usr/bin/env python3
"""One bounded native chooser UI action on an explicitly verified owned node.

Only writes an exact QA location through EditableText, or invokes a chooser
accept button's advertised action. No text/value reads, traversal of file lists,
clipboard, mouse coordinates, App-internal APIs or credential dialogs.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import time
from public_ui_probe import owned_by_app, target_root


def allowed_path(purpose, path, inputs, outputs, must_be_fresh):
    for directory in (inputs, outputs):
        if not directory.is_absolute() or directory.is_symlink() or not directory.is_dir() or directory.stat().st_uid != os.getuid():
            raise ValueError('owned_explicit_directories_required')
    if path.is_symlink():
        raise ValueError('location_symlink_forbidden')
    if purpose == 'import':
        if path != inputs / 'opaque-quadrants.png' or not path.is_file() or path.stat().st_size != 800:
            raise ValueError('exact_fixture_only')
        if hashlib.sha256(path.read_bytes()).hexdigest() != '0928c47fa44250879270def6198e04fd939dd8250864179760203f0d334a6d63':
            raise ValueError('exact_fixture_SHA_required')
    elif purpose == 'save':
        if path != outputs / 'qa-project' or (must_be_fresh and path.exists()):
            raise ValueError('fresh_fixed_project_path_only')
    elif purpose == 'export':
        if path != outputs / 'qa-export.png' or (must_be_fresh and path.exists()):
            raise ValueError('fresh_fixed_export_path_only')
    elif purpose == 'reopen':
        if (path.parent != outputs or not re.fullmatch(r'qa-project(?:\.[A-Za-z0-9_-]{1,16})?', path.name)
                or not path.is_file() or path.stat().st_size > 2 * 1024 * 1024 or path.stat().st_uid != os.getuid()):
            raise ValueError('exact_generated_project_only')
    else:
        raise ValueError('unknown_purpose')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target-pid', type=int, required=True)
    parser.add_argument('--owned-root-pid', type=int, required=True)
    parser.add_argument('--node-path', required=True, help='Previously observed numeric accessible node path')
    parser.add_argument('--purpose', choices=('import', 'save', 'reopen', 'export'), required=True)
    parser.add_argument('--owned-path', type=Path, required=True)
    parser.add_argument('--input-dir', type=Path, required=True)
    parser.add_argument('--deliverable-dir', type=Path, required=True)
    parser.add_argument('--mode', choices=('set-location', 'accept'), required=True)
    parser.add_argument('--ui-approval', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--private-accessibility-bus', action='store_true')
    args = parser.parse_args()
    report = {'success': False, 'mode': args.mode, 'purpose': args.purpose,
              'field_values_read': False, 'file_lists_read': False, 'screenshots': []}
    try:
        if not args.private_accessibility_bus or not os.environ.get('DISPLAY') or time.monotonic() >= args.deadline_monotonic:
            raise ValueError('private_bus_and_deadline_required')
        if args.target_pid == args.owned_root_pid or not owned_by_app(args.target_pid, args.owned_root_pid):
            raise ValueError('owned_native_descendant_required')
        if not args.ui_approval.is_absolute() or args.ui_approval.is_symlink() or args.ui_approval.stat().st_size > 32768:
            raise ValueError('exact_reviewed_native_contract_required')
        ui = json.loads(args.ui_approval.read_text())
        if (ui.get('schema') != 2 or ui.get('reviewed_by') != 'independent-qa'
                or ui.get('workflow_revision') != 6 or ui.get('observed_head') != '11ebf203e1a78d3b6a21677c4b17e96223c74b0e'
                or ui.get('observed_app_sha256') != '8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30'
                or ui.get('native_action_sha256') != hashlib.sha256(Path(__file__).read_bytes()).hexdigest()):
            raise ValueError('native_contract_identity_mismatch')
        allowed_path(args.purpose, args.owned_path, args.input_dir, args.deliverable_dir, args.mode == 'set-location')
        indices = json.loads(args.node_path)
        if not isinstance(indices, list) or not indices or len(indices) > 24 or any(type(i) is not int or not 0 <= i < 128 for i in indices):
            raise ValueError('bounded_numeric_node_path_required')
        root, Atspi = target_root(args.target_pid, min(args.deadline_monotonic, time.monotonic() + 3))
        toolkit = root.get_toolkit_name()
        if not isinstance(toolkit, str) or not toolkit.lower().startswith('gtk'):
            raise ValueError('actual_GTK_native_required')
        node = root
        for index in indices:
            if time.monotonic() >= args.deadline_monotonic:
                raise ValueError('deadline_no_action')
            if node.get_role() in (Atspi.Role.TABLE, Atspi.Role.TREE, Atspi.Role.TREE_TABLE, Atspi.Role.LIST, Atspi.Role.DIRECTORY_PANE):
                raise ValueError('file_list_traversal_forbidden')
            node = node.get_child_at_index(index)
            if node is None:
                raise ValueError('observed_node_no_longer_exists')
        node.clear_cache_single()
        if not node.get_state_set().contains(Atspi.StateType.SHOWING):
            raise ValueError('observed_node_no_longer_showing')
        states = node.get_state_set()
        if not (states.contains(Atspi.StateType.ENABLED) or states.contains(Atspi.StateType.SENSITIVE)):
            raise ValueError('actual_node_input_availability_not_confirmed')
        interfaces = set(node.get_interfaces())
        if args.mode == 'set-location':
            if node.get_role() not in (Atspi.Role.ENTRY, Atspi.Role.TEXT) or 'EditableText' not in interfaces:
                raise ValueError('actual_editable_entry_interface_required')
            if time.monotonic() >= args.deadline_monotonic:
                raise ValueError('deadline_no_UI_write')
            # Writes the owned path; never reads the old or new text contents.
            report['success'] = bool(node.get_editable_text_iface().set_text_contents(str(args.owned_path)))
        else:
            labels = ui['native_public_contract']['accept_labels_by_phase'][args.purpose]
            if node.get_role() != Atspi.Role.PUSH_BUTTON or node.get_name() not in labels or 'Action' not in interfaces:
                raise ValueError('actual_unique_phase_accept_button_required')
            action = node.get_action_iface()
            actions = [i for i in range(min(action.get_n_actions(), 8)) if action.get_action_name(i) in ('click', 'activate', 'press')]
            if len(actions) != 1:
                raise ValueError('one_advertised_accept_action_required')
            if time.monotonic() >= args.deadline_monotonic:
                raise ValueError('deadline_no_UI_accept')
            report['success'] = bool(action.do_action(actions[0]))
    except Exception:
        report['blocking_reason'] = 'owned_native_UI_action_not_confirmed_raw_error_withheld'
    print(json.dumps(report, ensure_ascii=False, separators=(',', ':')))
    return 0 if report['success'] else 2


if __name__ == '__main__':
    raise SystemExit(main())
