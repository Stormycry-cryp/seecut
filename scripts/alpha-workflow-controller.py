#!/usr/bin/env python3
"""Finite UI workflow using reviewed pixels and live public native-dialog roles.

Only exact owned fixture/output paths may be entered in a proven GTK chooser.
No application internals, grants, clipboard, generation or MCP calls.
The launcher owns the one private App instance and its cleanup within 300s.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
from alpha_checks import (locate_fixture, translated, translated_with_canvas_clip,
    gallery_target, gallery_preview, saved_title, crop_sha, exact_rgba, validate_public_capture, read_owned)

GUARD_HEAD = 'a6cf09ad935bbbc6cf792c4ac6a4b506a47aebd3'
GUARD_APP_SHA = 'b7ec5fc769d09e3685f212cd96484dfbb0858e9edb649d1314877a5f0a86f852'
HEAD = '0aa9406247e53f073c0b4df686adc68b40e2f8f6'
APP_SHA = '03fb1752c34adb0c0f4a21521c2302ee80c9b23203728cbcf636d3a4a57eaea2'


class Stop(Exception):
    pass


def read_json(path):
    if not path.is_absolute() or path.is_symlink() or not path.is_file():
        raise Stop('explicit_regular_declaration_required')
    raw = path.read_bytes()
    if len(raw) > 32768:
        raise Stop('declaration_size_limit')
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def validate_export_destination(metadata, contract):
    """Require the complete current public footprint of the reviewed App modal."""
    nodes = metadata.get('nodes')
    if (metadata.get('status') != 'public_metadata_observed'
            or metadata.get('coverage_complete') is not True
            or metadata.get('field_values_read') is not False
            or not isinstance(nodes, list) or not 1 <= len(nodes) <= 512):
        raise Stop('export_destination_metadata_not_complete')
    paths = [tuple(n.get('path', ())) for n in nodes]
    if len(set(paths)) != len(paths):
        raise Stop('export_destination_duplicate_public_path')
    if any(n.get('showing') and (n.get('dialog') or n.get('modal') or n.get('file_chooser')) for n in nodes):
        raise Stop('unknown_public_dialog_before_export_destination')
    x, y, width, height = contract['region']
    actual = []
    for node in nodes:
        bounds = node.get('bounds') or {}
        bx, by, bw, bh = (bounds.get(key, -1) for key in ('x', 'y', 'width', 'height'))
        if (node.get('showing') and bw > 0 and bh > 0 and bx >= x and by >= y
                and bx + bw <= x + width and by + bh <= y + height):
            actual.append({key: node.get(key) for key in contract['node_keys']})
    if actual != contract['nodes']:
        raise Stop('export_destination_modal_nodes_or_health_changed')
    for label in ('导出至资产库', '导出至其他文件夹'):
        buttons = [n for n in nodes if n.get('showing') and n.get('button') and n.get('label') == label]
        if len(buttons) != 1:
            raise Stop('export_destination_button_not_unique:' + label)
    return next(n for n in actual if n.get('button') and n.get('label') == '导出至其他文件夹')


def rgb_guard_matches(raw, spec, width=1280, height=900):
    """Use fresh pixels held in memory without creating an extra screenshot."""
    if len(raw) != width * height * 3:
        return False
    x, y, crop_width, crop_height = spec['region']
    crop = b''.join(raw[(row * width + x) * 3:(row * width + x + crop_width) * 3]
                    for row in range(y, y + crop_height))
    return hashlib.sha256(crop).hexdigest() == spec['rgb_sha256']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-pid', type=int, required=True)
    parser.add_argument('--window-id', type=int)
    parser.add_argument('--work-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--identity-approval', type=Path, required=True)
    parser.add_argument('--ui-approval', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--isolated-display-capture', action='store_true')
    parser.add_argument('--private-accessibility-bus', action='store_true')
    parser.add_argument('--probe-python', default='/usr/bin/python3')
    parser.add_argument('--input-dir', type=Path, required=True,
                        help='Owned directory containing the exact three QA fixtures and manifest')
    args = parser.parse_args()
    start = time.monotonic()
    deadline = min(start + 270, args.deadline_monotonic - 15)
    report = {'phase': 'finite-workflow-observation', 'status': 'blocked',
              'product_verdict': 'pending_main_actual_evidence_review',
              'actions': [], 'guards': [], 'captures': [], 'app_pid': args.app_pid,
              'file_input_attempted': False, 'file_selected': False,
              'permissions_granted': False, 'client_started': False,
              'paid_action_requested': False, 'settings_pixels_captured': False,
              'stages': {}, 'window_observations': [],
              'cleanup_owner': 'launcher; whole App run including cleanup <=300s'}
    evidence = None
    settings_entered = False
    title_checks = 0

    def command(argv, timeout=6, binary=False, allow_empty=False, input_data=None):
        left = deadline - time.monotonic()
        if left < 0.25:
            raise Stop('deadline_no_further_input')
        try:
            return subprocess.run(argv, check=True, capture_output=True,
                                  text=not binary, input=input_data, timeout=min(timeout, left)).stdout
        except subprocess.TimeoutExpired:
            raise Stop('command_timeout_no_retry') from None
        except subprocess.CalledProcessError as exc:
            if allow_empty and exc.returncode == 1:
                return b'' if binary else ''
            raise Stop('public_UI_command_failed_raw_output_withheld') from None

    def pause():
        if deadline - time.monotonic() < 1:
            raise Stop('insufficient_capture_time')
        time.sleep(0.7)

    def snapshot(name, root=False):
        if settings_entered:
            raise Stop('settings_pixels_forbidden')
        safe_capture(name, root)
        path = evidence / (name + '.png')
        if path.exists():
            raise Stop('evidence_path_already_exists')
        if len(report['captures']) >= 39:
            raise Stop('40_PNG_budget_includes_export_no_further_capture')
        command(['import', '-window', 'root' if root else str(window),
                 '-strip', str(path)], timeout=8)
        os.chmod(path, 0o600)
        size = path.stat().st_size
        if size > 2 * 1024 * 1024 or sum(x['bytes'] for x in report['captures']) + size > 14 * 1024 * 1024:
            raise Stop('PNG_budget_exceeded_do_not_publish')
        report['captures'].append({'file': path.name, 'bytes': size,
                                  'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                                  'surface': 'dedicated-private-Xvfb' if root else 'App-window'})
        return path

    def matched(frame, name, region=None):
        spec = ui['guards'][name]
        x, y, width, height = region or spec['region']
        raw = command(['convert', str(frame), '-crop', f'{width}x{height}+{x}+{y}',
                       '+repage', '-alpha', 'off', '-depth', '8', 'rgb:-'], binary=True)
        digest = hashlib.sha256(raw).hexdigest()
        matched = len(raw) == width * height * 3 and digest == spec['rgb_sha256']
        report['guards'].append({'frame': frame.name, 'control_context': name,
                                 'rgb_sha256': digest, 'matched': matched, 'actual_region': [x, y, width, height]})
        return matched

    def fresh_rgb():
        return command(['import', '-window', str(window), '-depth', '8', 'rgb:-'], binary=True)

    def main_focus():
        if visible_windows():
            raise Stop('owned_native_window_before_App_input')
        command(['xdotool', 'windowfocus', '--sync', str(window)])
        if (int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != window):
            raise Stop('App_identity_or_focus_changed_no_input')

    def semantic_title():
        nonlocal title_checks
        if title_checks >= 3:
            raise Stop('finite_saved_title_sample_budget')
        name = 'saved-title-public-' + str(title_checks)
        title_checks += 1
        if hashlib.sha256(read_owned(fixture)).hexdigest() != ui['alpha_semantic_contract']['fixture_png_sha256']:
            raise Stop('controlled_source_fixture_changed')
        saved_title(public_metadata(args.app_pid, name), args.app_pid)
        report['guards'].append({'control_context':'saved-image-title',
            'gate':'exact-closed-caption-unique-safe-public-node','matched':True})

    def guard(frame, name):
        if name == 'saved-image-title':
            semantic_title()
        elif not matched(frame, name):
            raise Stop('current_control_context_differs:' + name)

    def click(key, frame, contexts, direct=False):
        for context in contexts:
            guard(frame, context)
        main_focus()
        raw=fresh_rgb()
        for context in contexts:
            if context != 'saved-image-title' and not rgb_guard_matches(raw,ui['guards'][context]):
                raise Stop('fresh_control_context_changed_no_input:' + context)
        if 'saved-image-title' in contexts:
            semantic_title()
        xy=ui['direct_coordinates' if direct else 'conditional_coordinates'][key]
        command(['xdotool', 'mousemove', '--window', str(window), *map(str,xy)])
        command(['xdotool', 'click', '1'])
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        report['actions'].append({'kind':'click','target':key,'guarded_frame':frame.name,'xy':xy,'fresh_RGB_rechecked':True})
        pause()

    def click_gallery_card(kind, frame):
        if kind not in ('blank','image'):
            raise Stop('unknown_gallery_target')
        main_focus()
        metadata=public_metadata(args.app_pid, 'gallery-public-' + kind)
        target=gallery_target(metadata,args.app_pid,ui['gallery_public_contract'])
        if hashlib.sha256(read_owned(fixture)).hexdigest() != ui['alpha_semantic_contract']['fixture_png_sha256']:
            raise Stop('controlled_source_fixture_changed')
        raw=fresh_rgb()
        alpha_left=target['bounds']['x']
        preview=gallery_preview(raw,alpha_left)
        if kind == 'image':
            left=alpha_left
        elif kind == 'blank':
            # Other card stays bound to both original opaque-era blank RGB gates.
            found=[]
            for left in ui['gallery_slot_lefts']:
                checks=[]
                for key in ('gallery-blank-preview','gallery-blank-title'):
                    spec=dict(ui['guards'][key]);spec['region']=list(spec['region']);spec['region'][0]=left
                    checks.append(rgb_guard_matches(raw,spec))
                if all(checks):found.append(left)
            if len(found)!=1 or abs(found[0]-alpha_left)<2:
                raise Stop('exact_reviewed_blank_card_not_unique')
            left=found[0]
        else:raise Stop('unknown_gallery_target')
        if visible_windows():raise Stop('late_native_before_gallery_input')
        xy=[left+179,240]
        command(['xdotool','mousemove','--window',str(window),*map(str,xy)])
        command(['xdotool','click','1'])
        command(['xdotool','mousemove','--window',str(window),'100','700'])
        report['actions'].append({'kind':'bounded-gallery-card-click','card':kind,'xy':xy,
            'guarded_frame':frame.name,'closed_alpha_node_path':target['path'],'alpha_preview':preview})
        pause()

    def owned_focus(pid):
        visited = set()
        try:
            while pid >= 2 and pid not in visited and len(visited) < 16:
                visited.add(pid)
                proc = Path('/proc') / str(pid)
                if not proc.exists() or proc.stat().st_uid != os.getuid():
                    return False
                if pid == args.app_pid:
                    return True
                lines = (proc / 'status').read_text().splitlines()
                parent = next((line for line in lines if line.startswith('PPid:')), None)
                if parent is None:
                    return False
                pid = int(parent.split()[1])
        except (OSError, ValueError):
            return False
        return False

    def descendants():
        pids, pending = {args.app_pid}, [args.app_pid]
        while pending and len(pids) < 24:
            pid = pending.pop()
            proc = Path('/proc') / str(pid)
            if not proc.exists() or not owned_focus(pid):
                continue
            for task in list((proc / 'task').iterdir())[:64]:
                child_file = task / 'children'
                if not child_file.exists():
                    continue
                try:
                    children = list(map(int, child_file.read_text().split()))
                except FileNotFoundError:
                    continue
                for child in children:
                    if child not in pids and len(pids) < 24 and owned_focus(child):
                        pids.add(child)
                        pending.append(child)
        return pids

    def visible_windows():
        result = []
        for pid in descendants():
            ids = command(['xdotool', 'search', '--onlyvisible', '--pid', str(pid)], allow_empty=True).split()
            for wid in set(map(int, ids)):
                if wid == window:
                    continue
                fields = dict(line.split('=', 1) for line in command(['xdotool', 'getwindowgeometry', '--shell', str(wid)]).splitlines() if '=' in line)
                bounds = [int(fields[key]) for key in ('X', 'Y', 'WIDTH', 'HEIGHT')]
                x, y, width, height = bounds
                if width > 0 and height > 0:
                    result.append({'window': wid, 'pid': pid, 'bounds': bounds, 'mapped_visible': True,
                                   'inside_private_display': x >= 0 and y >= 0 and x + width <= int(dims[0]) and y + height <= int(dims[1])})
        return sorted(result, key=lambda item: item['window'])

    def await_native(phase, allow_absence=False):
        until = min(deadline, time.monotonic() + 12)
        previous, stable_at = None, None
        while time.monotonic() < until:
            found = visible_windows()
            if len(found) > 1:
                snapshot(phase + '-ambiguous-native', root=True)
                raise Stop('multiple_visible_native_windows_no_input:' + phase)
            if found:
                current = found[0]
                if not current['inside_private_display']:
                    snapshot(phase + '-native-outside-display', root=True)
                    raise Stop('mapped_native_not_inside_private_display:' + phase)
                if current == previous and time.monotonic() - stable_at >= 0.8:
                    report['window_observations'].append(dict(current, phase=phase))
                    snapshot(phase + '-native-visible', root=True)
                    return current
                if current != previous:
                    previous, stable_at = current, time.monotonic()
            else:
                previous, stable_at = None, None
            time.sleep(0.25)
        snapshot(phase + '-no-visible-native-after-wait', root=True)
        report['window_observations'].append({'phase': phase, 'mapped_visible': False,
                                               'owned_pids': sorted(descendants()), 'wait_limit_seconds': 12})
        if allow_absence:
            return None
        raise Stop('no_stable_visible_native_window_after_wait:' + phase)

    def public_metadata(pid, name):
        if not args.private_accessibility_bus:
            raise Stop('private_public_accessibility_required_for_native_input')
        completed = subprocess.run([args.probe_python, '-B', str(helper), '--app-pid', str(pid),
                                    '--owned-root-pid', str(args.app_pid), '--output', str(evidence),
                                    '--output-name', name + '.json', '--deadline-monotonic', str(min(deadline, time.monotonic() + 3)),
                                    '--private-accessibility-bus'], capture_output=True,
                                   timeout=min(5, max(0.25, deadline - time.monotonic())))
        path = evidence / (name + '.json')
        if completed.returncode != 0 or not path.is_file() or path.stat().st_size > 128 * 1024:
            raise Stop('public_accessibility_unavailable:' + name)
        data = json.loads(path.read_text())
        if not data.get('coverage_complete'):
            raise Stop('public_metadata_incomplete_no_unique_control_inference:' + name)
        return data

    def safe_capture(name, root=False):
        data=public_metadata(args.app_pid,'capture-' + name.lower())
        validate_public_capture(data,args.app_pid)
        if int(command(['xdotool','getwindowpid',str(window)]).strip()) != args.app_pid:
            raise Stop('capture_App_identity_changed')
        found=visible_windows()
        if found:
            if not root or len(found)!=1 or not found[0]['inside_private_display']:
                raise Stop('unknown_native_metadata_only')
            native=found[0]
            public=public_metadata(native['pid'],'native-proof-' + name.lower())
            if (public.get('toolkit')!='GTK' or not any(n.get('dialog') and n.get('showing') for n in public['nodes'])
                    or not any(n.get('button') and n.get('showing') and n.get('action_interface') and n.get('label') in
                        ('Open','打开','Save','保存','Export','导出','OK','Ok') for n in public['nodes'])):
                raise Stop('unknown_native_chooser_metadata_only')
            raw=command(['import','-window','root','-depth','8','rgb:-'],binary=True)
            footer=ui['native_footer_guard'];x,y,w,h=native['bounds']
            spec={'region':[x+w-footer['right']-footer['width'],y+h-footer['bottom']-footer['height'],footer['width'],footer['height']],
                  'rgb_sha256':footer['rgb_sha256']}
            if not rgb_guard_matches(raw,spec,int(dims[0]),int(dims[1])):
                raise Stop('unknown_native_chrome_metadata_only')

    def choose_export_destination(frame):
        contexts = ('export-destination-modal', 'export-other-folder-control')
        for context in contexts:
            guard(frame, context)
        metadata = public_metadata(args.app_pid, '20-export-public')
        node = validate_export_destination(metadata, ui['export_destination_public_contract'])
        xy = ui['conditional_coordinates']['export_other_folder']
        bounds = node['bounds']
        if not (bounds['x'] < xy[0] < bounds['x'] + bounds['width']
                and bounds['y'] < xy[1] < bounds['y'] + bounds['height']):
            raise Stop('export_destination_point_outside_reviewed_button')
        if visible_windows():
            snapshot('21-export-native-visible', root=True)
            raise Stop('late_native_before_export_destination_input')
        command(['xdotool', 'windowfocus', '--sync', str(window)])
        if (int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != window):
            raise Stop('export_destination_App_identity_or_focus_changed')
        # Public probing/focusing may take time: bind the one click to fresh RGB.
        raw = command(['import', '-window', str(window), '-depth', '8', 'rgb:-'], binary=True)
        for context in contexts:
            if not rgb_guard_matches(raw, ui['guards'][context]):
                snapshot('21-export-unreviewed-destination')
                raise Stop('current_export_destination_pixels_changed_no_input:' + context)
        if visible_windows():
            snapshot('21-export-native-visible', root=True)
            raise Stop('late_native_before_export_destination_input')
        command(['xdotool', 'mousemove', '--window', str(window), *map(str, xy)])
        if visible_windows():
            snapshot('21-export-native-visible', root=True)
            raise Stop('late_native_before_export_destination_input')
        command(['xdotool', 'click', '1'])
        report['actions'].append({'kind': 'click', 'target': 'export_other_folder',
                                  'guarded_frame': frame.name, 'xy': xy,
                                  'public_node_path': node['path'], 'fresh_RGB_rechecked': True})
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        pause()
        return await_native('21-export')

    def inside(node, native):
        bounds = node.get('bounds')
        if not bounds:
            return False
        x, y, width, height = native['bounds']
        return (bounds['width'] > 0 and bounds['height'] > 0 and bounds['x'] >= x and bounds['y'] >= y
                and bounds['x'] + bounds['width'] <= x + width and bounds['y'] + bounds['height'] <= y + height)

    def focus_visible(native):
        if native not in visible_windows():
            raise Stop('native_window_disappeared_no_input')
        command(['xdotool', 'windowfocus', '--sync', str(native['window'])])
        if int(command(['xdotool', 'getwindowfocus']).strip()) != native['window']:
            raise Stop('visible_native_focus_not_confirmed')

    def await_native_gone(native, phase):
        until = min(deadline, time.monotonic() + 12)
        while time.monotonic() < until:
            if not visible_windows():
                main_ids = command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)], allow_empty=True).split()
                if str(window) not in main_ids:
                    raise Stop('main_window_not_mapped_after_native_return')
                command(['xdotool', 'windowfocus', '--sync', str(window)])
                pause()
                frame = snapshot(phase + '-returned-App')
                # Close only a visually proven App menu, never a guessed native window.
                if matched(frame, 'open-menu'):
                    command(['xdotool', 'key', '--clearmodifiers', 'Escape'])
                    report['actions'].append({'kind': 'key', 'key': 'Escape', 'target': 'visibly-matched-App-open-menu'})
                    pause()
                    frame = snapshot(phase + '-menu-closed')
                if visible_windows() or matched(frame, 'open-menu'):
                    raise Stop('return_still_has_native_or_App_menu:' + phase)
                guard(frame, 'editor-tools')
                guard(frame, 'save-control')
                report['window_observations'].append({'phase': phase, 'native_gone': True,
                                                       'main_window_visible': True, 'main_window': window})
                return frame
            time.sleep(0.25)
        snapshot(phase + '-native-not-gone', root=True)
        raise Stop('native_window_not_visibly_gone:' + phase)

    def choose_owned_path(native, path, phase, accept_labels):
        metadata = public_metadata(native['pid'], phase + '-public-before')
        nodes = metadata['nodes']
        accept_labels = set(accept_labels) | {'OK', 'Ok'}
        if (metadata.get('toolkit') != 'GTK' or not any(n.get('dialog') and n.get('showing') for n in nodes)
                or not any(n.get('button') and n.get('showing') and n.get('action_interface')
                           and n.get('label') in accept_labels for n in nodes)):
            raise Stop('native_file_chooser_semantics_not_confirmed:' + phase)
        # GTK4 exposes generic containers and unreliable child extents in this observed runner.
        # The current, owned native window's actual Cancel/OK chrome is the pixel gate.
        footer = ui['native_footer_guard']
        x, y, width, height = native['bounds']
        fx = x + width - footer['right'] - footer['width']
        fy = y + height - footer['bottom'] - footer['height']
        frame = snapshot(phase + '-native-chrome', root=True)
        rgb = command(['convert', str(frame), '-crop', f"{footer['width']}x{footer['height']}+{fx}+{fy}",
                       '+repage', '-alpha', 'off', '-depth', '8', 'rgb:-'], binary=True)
        if hashlib.sha256(rgb).hexdigest() != footer['rgb_sha256']:
            raise Stop('current_native_chooser_chrome_differs_no_location_input:' + phase)
        focus_visible(native)
        before_entries = {tuple(n['path']) for n in nodes if n.get('entry') and n.get('showing') and n.get('editable_text_interface')}
        # GTK's documented location-popup binding, after actual native pixel/toolkit/action proof.
        command(['xdotool', 'key', '--clearmodifiers', 'ctrl+l'])
        pause()
        snapshot(phase + '-location-visible', root=True)
        metadata = public_metadata(native['pid'], phase + '-public-location')
        entries = [n for n in metadata['nodes'] if n.get('entry') and n.get('editable_text_interface')
                   and n.get('showing') and (n.get('enabled') or n.get('sensitive'))
                   and (n.get('focused') or tuple(n['path']) not in before_entries)]
        if len(entries) != 1:
            raise Stop('GTK_location_entry_not_unique_new_or_focused_no_input:' + phase)
        focus_visible(native)
        report['file_input_attempted'] = True
        native_action(native, entries[0], path, phase, 'set-location')
        pause()
        metadata = public_metadata(native['pid'], phase + '-public-before-accept')
        buttons = [n for n in metadata['nodes'] if n.get('button') and n.get('showing') and n.get('action_interface')
                   and (n.get('enabled') or n.get('sensitive'))
                   and n.get('label') in accept_labels and n.get('allowed_actions')]
        if len(buttons) != 1:
            raise Stop('native_accept_action_not_unique:' + phase)
        focus_visible(native)
        native_action(native, buttons[0], path, phase, 'accept')
        return await_native_gone(native, phase)

    def native_action(native, node, path, phase, mode):
        purpose = phase.split('-')[-1]
        if native not in visible_windows():
            raise Stop('native_not_visible_before_public_action')
        try:
            completed = subprocess.run([args.probe_python, '-B', str(action_helper), '--target-pid', str(native['pid']),
                                        '--owned-root-pid', str(args.app_pid), '--node-path', json.dumps(node['path']),
                                        '--purpose', purpose, '--owned-path', str(path), '--input-dir', str(args.input_dir),
                                        '--deliverable-dir', str(deliverables), '--mode', mode, '--ui-approval', str(args.ui_approval),
                                        '--deadline-monotonic', str(min(deadline, time.monotonic() + 3)),
                                        '--private-accessibility-bus'], capture_output=True,
                                       timeout=min(5, max(0.25, deadline - time.monotonic())))
        except subprocess.TimeoutExpired:
            raise Stop('native_public_action_timeout_unknown_result_no_retry:' + phase) from None
        if completed.returncode != 0 or len(completed.stdout) > 4096:
            raise Stop('native_public_action_not_confirmed_no_retry:' + phase)
        result = json.loads(completed.stdout)
        if not result.get('success'):
            raise Stop('native_public_action_returned_false_no_retry:' + phase)
        report['actions'].append({'kind': 'public-native-UI-' + mode, 'purpose': purpose,
                                  'basename': path.name, 'node_path': node['path'], 'native_window': native['window'],
                                  'API_returned_success': True})

    def image_info(frame):
        raw = command(['convert', str(frame), '-alpha', 'off', '-depth', '8', 'rgb:-'], binary=True)
        try:
            return locate_fixture(raw, 1280, 900, (128, 88, 960, 870))
        except ValueError as exc:
            raise Stop(str(exc)) from None

    def await_fixture(frame, phase):
        until = min(deadline, time.monotonic() + 12)
        previous, stable_at = None, None
        while time.monotonic() < until:
            # Read only the verified App's current visible pixels into memory; no repeated PNG dump.
            raw = command(['import', '-window', str(window), '-depth', '8', 'rgb:-'], binary=True)
            try:
                info = locate_fixture(raw, 1280, 900, (128, 88, 960, 870))
                current = (info['bounds'], info['color_pixels'])
                if current == previous and time.monotonic() - stable_at >= 0.5:
                    ready = snapshot(phase + '-fixture-visible-stable')
                    image_info(ready)
                    return ready
                if current != previous:
                    previous, stable_at = current, time.monotonic()
            except ValueError:
                previous, stable_at = None, None
            time.sleep(0.25)
        snapshot(phase + '-fixture-not-confirmed-after-wait')
        raise Stop('owned_fixture_not_visibly_stable_after_wait:' + phase)

    def edit_and_undo(frame, phase):
        before = image_info(frame)
        before_crop = crop_sha(command(['convert',str(frame),'-alpha','off','-depth','8','rgb:-'],binary=True))
        guard(frame, 'editor-tools')
        if visible_windows():
            raise Stop('native_still_visible_before_edit')
        main_focus()
        if crop_sha(fresh_rgb()) != before_crop:
            raise Stop('alpha_canvas_changed_before_drag_no_input')
        x, y = map(round, before['centroids'][0])
        dx, dy = 26, 17
        command(['xdotool', 'mousemove', '--window', str(window), str(x), str(y)])
        command(['xdotool', 'mousedown', '1'])
        try:
            for mx, my in ((13, 8), (26, 17)):
                command(['xdotool', 'mousemove', '--window', str(window), str(x + mx), str(y + my)])
                time.sleep(0.15)
        finally:
            command(['xdotool', 'mouseup', '1'])
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        pause()
        moved_frame = snapshot(phase + '-dragged')
        after = image_info(moved_frame)
        report['stages'][phase] = {'visible_drag_pixels': [dx, dy], 'before': before, 'after': after,
                                  'check': 'inner_markers_and_all_color_rectangles_with_fixed_canvas_clip'}
        if not translated_with_canvas_clip(before, after, dx, dy):
            raise Stop('actual_two_axis_drag_not_visibly_confirmed:' + phase)
        # One conventional Linux Undo; the real result decides whether it worked.
        main_focus()
        if crop_sha(fresh_rgb()) != crop_sha(command(['convert',str(moved_frame),'-alpha','off','-depth','8','rgb:-'],binary=True)):
            raise Stop('drag_result_changed_before_one_Undo_no_input')
        command(['xdotool', 'key', '--clearmodifiers', 'ctrl+z'])
        pause()
        frame = snapshot(phase + '-one-Undo')
        restored = image_info(frame)
        if (not translated(before, restored, 0, 0)
                or crop_sha(command(['convert',str(frame),'-alpha','off','-depth','8','rgb:-'],binary=True)) != before_crop):
            raise Stop('one_native_Undo_did_not_restore_fixture:' + phase)
        report['stages'][phase] = {'visible_drag_pixels': [dx, dy], 'before': before,
                                    'after': after, 'one_Undo_restored': restored,
                                    'full_canvas_RGB_restored': True,
                                    'verdict': 'pending_main_actual_evidence_review'}
        return frame

    def stable_file(path):
        until = min(deadline, time.monotonic() + 10)
        previous, stable_at = None, None
        while time.monotonic() < until:
            if path.is_file() and not path.is_symlink() and path.stat().st_uid == os.getuid():
                current = (path.stat().st_size, path.stat().st_mtime_ns)
                if current[0] > 2 * 1024 * 1024:
                    raise Stop('owned_output_exceeds_2MiB_no_copy')
                if current[0] and current == previous and time.monotonic() - stable_at > 0.8:
                    return {'file': path.name, 'bytes': current[0], 'sha256': hashlib.sha256(read_owned(path)).hexdigest()}
                if current != previous:
                    previous, stable_at = current, time.monotonic()
            time.sleep(0.25)
        raise Stop('owned_output_not_present_and_stable')

    def copy_owned_output(source, name):
        # Preserve the real bounded file before format/pixel checks, including failures.
        before = stable_file(source)
        target = evidence / name
        if (name.endswith('.png') and (len(report['captures']) + 1 > 40
                or sum(item['bytes'] for item in report['captures']) + before['bytes'] > 14 * 1024 * 1024)):
            raise Stop('owned_export_exceeds_combined_PNG_budget')
        if sum(p.stat().st_size for p in evidence.iterdir() if p.is_file()) + before['bytes'] > 15 * 1024 * 1024 - 65536:
            raise Stop('owned_output_copy_exceeds_total_budget')
        with target.open('xb') as stream:
            os.chmod(target, 0o600)
            stream.write(read_owned(source))
        if hashlib.sha256(target.read_bytes()).hexdigest() != before['sha256']:
            raise Stop('owned_output_copy_not_stable')

    try:
        if args.app_pid < 2 or deadline <= start or not args.isolated_display_capture or not os.environ.get('DISPLAY'):
            raise Stop('live_private_Xvfb_and_deadline_required')
        for directory in (args.work_dir, args.output, args.input_dir):
            if not directory.is_absolute() or directory.is_symlink() or not directory.is_dir() or directory.stat().st_uid != os.getuid():
                raise Stop('explicit_owned_isolated_directory_required')
        runtime, runtime_sha = read_json(args.identity_approval)
        ui, ui_sha = read_json(args.ui_approval)
        if (runtime.get('schema') != 2 or runtime.get('reviewed_by') != 'main-reviewer'
                or runtime.get('runtime_head') != HEAD or runtime.get('runtime_app_sha256') != APP_SHA
                or runtime.get('change_scope') not in ('tests-and-runtime-only', 'product-candidate')):
            raise Stop('main_runtime_identity_mismatch')
        if (args.ui_approval.stat().st_size > 16384 or ui.get('schema') != 2 or ui.get('reviewed_by') != 'main-reviewer'
                or ui.get('observed_head') != GUARD_HEAD or ui.get('observed_app_sha256') != GUARD_APP_SHA
                or ui.get('window') != {'width': 1280, 'height': 900}
                or ui.get('workflow_revision') != 7):
            raise Stop('main_r7_UI_identity_mismatch')
        if ui.get('opened_document_contract', {}).get('expected_export_dimensions') != [256, 192]:
            raise Stop('reviewed_opened_document_contract_required')
        alpha = ui.get('alpha_semantic_contract', {})
        if (alpha.get('schema') != 1 or alpha.get('fixture') != 'transparent-markers.png'
                or alpha.get('fixture_png_sha256') != '8201fa2b0c7c94de97433462d40ae97aa43b77e395921769d967c7f168875b75'
                or alpha.get('fixture_rgba_sha256') != '9321b2244ee4bd74dcdaaf52601784addc2245123278c0e604d6291cc5dccd31'
                or alpha.get('canvas_crop') != [176,204,736,552]
                or alpha.get('gallery_bounds_lefts') != [493,874]
                or ui.get('gallery_slot_lefts') != [493,875]):
            raise Stop('fixed_alpha_scope_contract_required')
        helper = Path(__file__).with_name('alpha_public_probe.py')
        checks = Path(__file__).with_name('alpha_checks.py')
        action_helper = Path(__file__).with_name('alpha_native_action.py')
        if (hashlib.sha256(helper.read_bytes()).hexdigest() != ui.get('public_probe_sha256')
                or hashlib.sha256(checks.read_bytes()).hexdigest() != ui.get('workflow_checks_sha256')
                or hashlib.sha256(action_helper.read_bytes()).hexdigest() != ui.get('native_action_sha256')):
            raise Stop('reviewed_helpers_changed_no_UI_input')
        manifest, manifest_sha = read_json(args.input_dir / 'manifest.json')
        if manifest_sha != '8926b97d3370005fa008bcac6cf21fc287d3717448968591b1d705df76a290a7':
            raise Stop('exact_owned_fixture_manifest_required')
        for item in manifest['files']:
            path = args.input_dir / item['file']
            if (path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid()
                    or path.stat().st_size != item['bytes']
                    or hashlib.sha256(path.read_bytes()).hexdigest() != item['sha256']):
                raise Stop('owned_fixture_identity_mismatch')
        fixture = args.input_dir / 'transparent-markers.png'
        report['input_manifest_sha256'] = manifest_sha
        for spec in ui['guards'].values():
            if (len(spec['region']) != 4 or any(type(n) is not int for n in spec['region'])
                    or not re.fullmatch('[0-9a-f]{64}', spec['rgb_sha256'])):
                raise Stop('invalid_visual_guard')
            x, y, width, height = spec['region']
            if min(x, y) < 0 or min(width, height) < 1 or x + width > 1280 or y + height > 900:
                raise Stop('guard_outside_current_window')
        for mapping in ('direct_coordinates', 'conditional_coordinates'):
            for xy in ui[mapping].values():
                if len(xy) != 2 or any(type(n) is not int for n in xy) or not (0 <= xy[0] < 1280 and 32 <= xy[1] < 900):
                    raise Stop('invalid_UI_coordinate')
        for tool in ('xdotool', 'import', 'convert', 'identify'):
            if not shutil.which(tool):
                raise Stop('missing_visible_UI_dependency:' + tool)
        proc = Path('/proc') / str(args.app_pid)
        if proc.stat().st_uid != os.getuid():
            raise Stop('App_not_same_user')
        digest = hashlib.sha256()
        with (proc / 'exe').open('rb') as stream:
            while True:
                if time.monotonic() >= deadline:
                    raise Stop('identity_hash_deadline')
                chunk = stream.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
        if digest.hexdigest() != APP_SHA:
            raise Stop('actual_App_binary_SHA_mismatch_no_input')
        report.update(app_sha256=digest.hexdigest(), source_head=HEAD, guard_source_head=GUARD_HEAD,
                      alpha_UI_observation='current run only; parent image review pending',
                      identity_declaration_sha256=runtime_sha, UI_declaration_sha256=ui_sha)
        dims = command(['xdotool', 'getdisplaygeometry']).split()
        if len(dims) != 2 or not (1280 <= int(dims[0]) <= 1920 and 900 <= int(dims[1]) <= 1200):
            raise Stop('private_display_dimensions_outside_bounds')
        ids = sorted(set(map(int, command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)]).split())))
        if args.window_id is None:
            if len(ids) != 1:
                raise Stop('one_visible_App_window_required')
            window = ids[0]
        else:
            window = args.window_id
            if window not in ids:
                raise Stop('supplied_window_not_visible')
        if int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid:
            raise Stop('window_App_PID_mismatch')
        command(['xdotool', 'windowsize', '--sync', str(window), '1280', '900'])
        command(['xdotool', 'windowfocus', '--sync', str(window)])
        fields = dict(line.split('=', 1) for line in command(['xdotool', 'getwindowgeometry', '--shell', str(window)]).splitlines() if '=' in line)
        if fields.get('WIDTH') != '1280' or fields.get('HEIGHT') != '900':
            raise Stop('actual_window_size_mismatch')
        evidence = args.output / 'main-qa-alpha-workflow'
        evidence.mkdir(mode=0o700)
        deliverables = args.work_dir / 'qa-deliverables'
        deliverables.mkdir(mode=0o700)
        report['window_id'] = window
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        pause()
        frame = snapshot('01-current-initial')
        click('quick_mode', frame, ['first-mode'], direct=True)
        frame = snapshot('02-after-quick')
        click('canvas_navigation', frame, ['canvas-navigation'], direct=True)
        frame = snapshot('03-gallery')
        click('new_project', frame, ['new-project'])
        frame = snapshot('04-create-dialog')
        click('new_create', frame, ['create-dialog'])
        frame = snapshot('05-editor')
        click('editor_folder', frame, ['editor-toolbar', 'editor-tools', 'blank-canvas'])
        frame = snapshot('06-open-menu')
        click('open_local_image', frame, ['open-menu'])
        native = await_native('07-import')
        frame = choose_owned_path(native, fixture, '08-import', {'Open', '打开'})
        frame = await_fixture(frame, '08-import')
        imported = image_info(frame)
        imported_crop = crop_sha(command(['convert',str(frame),'-alpha','off','-depth','8','rgb:-'],binary=True))
        guard(frame, 'opened-image-properties')
        guard(frame, 'opened-image-zoom')
        report['opened_document_contract'] = ui['opened_document_contract']
        report['file_selected'] = True
        report['stages']['import'] = {'visible_fixture': imported, 'source_sha256': manifest['files'][1]['sha256'],
                                      'verdict': 'pending_main_actual_evidence_review'}
        frame = edit_and_undo(frame, '09-edit')
        click('editor_save', frame, ['save-control', 'editor-tools'])
        native = await_native('10-save', allow_absence=True)
        if hashlib.sha256(read_owned(fixture)).hexdigest() != manifest['files'][1]['sha256']:
            raise Stop('fixture_source_changed_after_save')
        report['fixture_source_unchanged_after_save'] = True
        if native is None:
            # Actual rev4 save removed the dirty label without a native picker.
            # This is a UI transition, not proof of a persisted/reopenable file.
            if visible_windows():
                raise Stop('late_native_before_save_gallery_observation')
            frame = snapshot('11-save-returned-App')
            for context in ('saved-image-title', 'editor-tools', 'opened-image-properties', 'opened-image-zoom'):
                guard(frame, context)
            report['stages']['save'] = {'native_picker_observed': False,
                'dirty_label_removed': True, 'actual_file_verified': False,
                'reopen_verified': False, 'verdict': 'visible_transition_only'}
            click('editor_back', frame, ['back-control', 'editor-tools', 'saved-image-title'])
            frame = snapshot('12-save-gallery')
            # rev5 supplies fixed gallery geometry; alpha content is source-based and unobserved.
            click_gallery_card('blank', frame)
            frame = snapshot('13-gallery-other-blank')
            guard(frame, 'blank-canvas')
            guard(frame, 'editor-tools')
            click('editor_back', frame, ['back-control', 'editor-tools'])
            frame = snapshot('14-gallery-after-other-document')
            click_gallery_card('image', frame)
            frame = await_fixture(frame, '15-gallery-reopen')
            reopened = image_info(frame)
            guard(frame, 'opened-image-properties')
            guard(frame, 'opened-image-zoom')
            if not translated(imported, reopened, 0, 0):
                raise Stop('gallery_reopened_fixture_geometry_not_preserved')
            if crop_sha(command(['convert',str(frame),'-alpha','off','-depth','8','rgb:-'],binary=True)) != imported_crop:
                raise Stop('reopened_full_canvas_RGB_not_restored')
            report['stages']['reopen'] = {'visible_fixture': reopened,
                'different_blank_document_visibly_opened_first': True,
                'complete_canvas_RGB_matches_import': True,
                'actual_project_file_verified': False, 'restart_persistence_verified': False,
                'verdict': 'same_App_gallery_reopen_pending_main_actual_evidence_review'}
        else:
            # Ordinary observed save has no native picker; new save UI is unknown.
            raise Stop('unknown_native_save_result_no_path_input')
        click('editor_export', frame, ['export-control', 'editor-tools', 'opened-image-properties', 'opened-image-zoom'])
        snapshot('19-export-destination', root=True)
        native = await_native('20-export', allow_absence=True)
        if native is None:
            frame = snapshot('20-export-App-destination-requires-review')
            native = choose_export_destination(frame)
        export = deliverables / 'qa-export.png'
        frame = choose_owned_path(native, export, '21-export', {'Save', '保存', 'Export', '导出'})
        info = stable_file(export)
        copy_owned_output(export, 'exported-qa.png')
        report['stages']['export'] = info
        export_bytes = read_owned(export)
        dimensions = command(['identify','-format','%w %h','png:-'],binary=True,input_data=export_bytes).decode().split()
        info['actual_dimensions'] = dimensions
        expected_dimensions = ui['opened_document_contract']['expected_export_dimensions']
        if dimensions != [str(v) for v in expected_dimensions] or export_bytes[:8] != b'\x89PNG\r\n\x1a\n':
            raise Stop('export_actual_PNG_or_current_canvas_dimensions_differ')
        rgba = command(['convert','png:-','-alpha','on','-depth','8','rgba:-'],binary=True,input_data=export_bytes)
        if read_owned(export) != export_bytes:
            raise Stop('owned_export_changed_during_RGBA_check')
        export_width, export_height = expected_dimensions
        if len(rgba) != export_width * export_height * 4:
            raise Stop('export_decoded_RGBA_size_mismatch')
        rgb = bytes(value for index, value in enumerate(rgba) if index % 4 != 3)
        try:
            pixels = locate_fixture(rgb, export_width, export_height)
        except ValueError as exc:
            raise Stop('export_pixels:' + str(exc)) from None
        info.update(width=export_width, height=export_height, visible_fixture=pixels,
                    RGBA_sha256=hashlib.sha256(rgba).hexdigest(),
                    nontransparent_pixels=sum(value != 0 for value in rgba[3::4]),
                    verdict='actual_file_checks_recorded_pending_main_actual_evidence_review')
        info['exact_fixture_pixels_preserved'] = exact_rgba(rgba, tuple(expected_dimensions))
        info['comparison_contract'] = 'all256x192_RGBA_bytes; alpha0_RGB_included; no_normalization'
        report['stages']['export'] = info
        if not info['exact_fixture_pixels_preserved']:
            raise Stop('export_original_fixture_RGBA_not_preserved')
        if hashlib.sha256(read_owned(fixture)).hexdigest() != manifest['files'][1]['sha256']:
            raise Stop('fixture_source_changed_during_workflow')
        report['fixture_source_unchanged'] = True
        report['status'] = 'bounded_UI_observation_completed_review_pending'
    except (Stop, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as exc:
        report['blocking_reason'] = str(exc) if isinstance(exc, Stop) else 'input_or_public_runtime_unavailable_raw_error_withheld'
    finally:
        report['elapsed_seconds'] = round(time.monotonic() - start, 3)
        report['settings_entered'] = settings_entered
        raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        if evidence is not None:
            artifact_bytes = sum(p.stat().st_size for p in evidence.rglob('*') if p.is_file())
            if artifact_bytes + len(raw) > 15 * 1024 * 1024:
                report['status'] = 'artifact_budget_exceeded_do_not_publish'
                raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
            with (evidence / 'workflow.json').open('xb') as stream:
                os.chmod(evidence / 'workflow.json', 0o600)
                stream.write(raw)
        print(raw.decode(), end='')
    return 0 if report['status'] == 'bounded_UI_observation_completed_review_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
