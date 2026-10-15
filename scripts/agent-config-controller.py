#!/usr/bin/env python3
"""Reuse successful native entry; observe settings metadata; close panel once.

Configuration produces zero PNG. No fields, grants, connection or provider.
One fresh App and one shared 120-second controller deadline, launcher cleanup.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

HEAD = '11ebf203e1a78d3b6a21677c4b17e96223c74b0e'
APP_SHA = '8fe30fc73f4ab79435e79aa582edf4e2b3adeb26a5435315ea32b537c28abf30'
ENTRY_SHA = '739940c925145f49136d7cc618a01f3b7775b4ae84cbb1d2647221032270f3e4'
CODES = frozenset({'private_context_required', 'deadline_no_further_UI', 'owned_window_or_focus_changed',
    'entry_dependency_unconfirmed', 'entry_observation_incomplete', 'entry_report_identity_unconfirmed',
    'configuration_UI_declaration_unconfirmed', 'fresh_owned_output_required', 'restored_geometry_unconfirmed',
    'safe_public_metadata_unavailable', 'same_App_metadata_required', 'fresh_conversation_panel_unconfirmed',
    'configuration_open_Action_unconfirmed', 'configuration_state_unconfirmed_no_retry',
    'configuration_close_Action_unconfirmed', 'panel_disappearance_or_canvas_unconfirmed',
    'combined_evidence_budget_exceeded'})


class Stop(Exception):
    def __init__(self, code):
        self.code = code if code in CODES else 'unknown_guard_code_withheld'


def usable_metadata(data):
    if (data.get('status') != 'public_metadata_observed' or data.get('coverage_complete') is not True
            or data.get('field_values_read') is not False or data.get('ui_actions') != []):
        return False
    showing = [n for n in data.get('nodes', []) if n.get('showing') is True]
    windows = [n for n in showing if n.get('path') == [0] and n.get('role') == 23
               and n.get('bounds') == {'x': 0, 'y': 0, 'width': 1280, 'height': 900}]
    return len(windows) == 1 and not any(n.get('modal') or n.get('dialog') for n in showing)


def healthy_button(node):
    return (node.get('role') == 43 and node.get('allowed_actions') == ['click']
            and all(node.get(k) is True for k in ('showing', 'enabled', 'sensitive', 'action_interface'))
            and not any(node.get(k) for k in ('entry', 'editable', 'editable_text_interface')))


def selected_node(data, spec):
    found = [n for n in data.get('nodes', []) if n.get('path') == spec['path']]
    if len(found) != 1:
        return None
    node = found[0]
    return node if (healthy_button(node) and node.get('label') == spec['label']
                    and node.get('bounds') == spec['bounds']) else None


def panel_state(data, ui, state):
    if not usable_metadata(data) or state not in ('conversation', 'configuration'):
        return False
    showing = [n for n in data['nodes'] if n.get('showing') is True]
    spec = ui['panel']
    panels = [n for n in showing if n.get('path') == spec['path'] and n.get('role') == 39
              and n.get('label') == '助手' and n.get('bounds') == spec['bounds']]
    labels = {n.get('label') for n in showing}
    settings = dict(ui['settings_button'])
    if state == 'configuration':
        settings['label'] = '返回对话'
    expected = settings['label']
    opposite = '助手设置' if state == 'configuration' else '返回对话'
    return (len(panels) == 1 and '画布' in labels and expected in labels and opposite not in labels
            and sum(n.get('label') == expected for n in showing) == 1
            and sum(n.get('label') == '关闭助手' for n in showing) == 1
            and selected_node(data, settings) is not None and selected_node(data, ui['close_button']) is not None)


def action_target(data, ui, intent):
    state = {'open-settings': 'conversation', 'close-from-settings': 'configuration'}.get(intent)
    if state is None or not panel_state(data, ui, state):
        return None
    return selected_node(data, ui['settings_button'] if intent == 'open-settings' else ui['close_button'])


def closed_context(data):
    if not usable_metadata(data):
        return None
    showing = [n for n in data['nodes'] if n.get('showing') is True]
    labels = {n.get('label') for n in showing}
    if (not {'画布', '属性', '图层'}.issubset(labels)
            or labels & {'助手设置', '关闭助手', '返回对话'}
            or any(n.get('role') == 39 and n.get('label') == '助手' for n in showing)):
        return None
    rail = [n for n in showing if n.get('path') == [0, 5] and n.get('role') == 62
            and n.get('label') == '助手' and n.get('bounds') == {'x': 18, 'y': 726, 'width': 44, 'height': 44}
            and all(n.get(k) is True for k in ('enabled', 'sensitive', 'focusable'))]
    all_focused = [n for n in showing if n.get('focused') is True]
    focused = [{k: n.get(k) for k in ('path', 'role', 'label')} for n in all_focused
               if not any(n.get(k) for k in ('entry', 'editable', 'editable_text_interface'))]
    focus_verified = (len(rail) == 1 and rail[0].get('focused') is True
                      and rail[0].get('checked') is False
                      and len(all_focused) == 1 and len(focused) == 1 and focused[0]['path'] == [0, 5])
    return {'panel_absent': True, 'canvas_context_present': True,
            'focus_on_known_trigger': focus_verified, 'focused_safe_nodes': focused}


def read_ui(path):
    if (not path.is_file() or path.is_symlink() or path.stat().st_size > 16384):
        raise Stop('configuration_UI_declaration_unconfirmed')
    ui = json.loads(path.read_bytes())
    expected = {'settings_button': {'path': [0, 32, 1], 'label': '助手设置',
                    'bounds': {'x': 1200, 'y': 88, 'width': 36, 'height': 40}},
                'close_button': {'path': [0, 32, 2], 'label': '关闭助手',
                    'bounds': {'x': 1240, 'y': 88, 'width': 36, 'height': 40}},
                'panel': {'path': [0, 32], 'bounds': {'x': 936, 'y': 88, 'width': 344, 'height': 812}}}
    if (ui.get('schema') != 'seecut-agent-config-ui-v1' or ui.get('source_head') != HEAD
            or ui.get('app_sha256') != APP_SHA
            or any(ui.get(k) != v for k, v in expected.items())):
        raise Stop('configuration_UI_declaration_unconfirmed')
    return ui


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('app-pid', 'window-id'):
        p.add_argument('--' + name, type=int, required=True)
    for name in ('work-dir', 'output', 'identity-approval', 'ui-approval'):
        p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--deadline-monotonic', type=float, required=True)
    p.add_argument('--next-stage', choices=['canvas-entry'], required=True)
    p.add_argument('--expected-sha', default=HEAD)
    p.add_argument('--probe-python', default='/usr/bin/python3')
    p.add_argument('--isolated-display-capture', action='store_true')
    p.add_argument('--private-accessibility-bus', action='store_true')
    args = p.parse_args()
    end = min(args.deadline_monotonic - 15, time.monotonic() + 120)
    report = {'schema': 'seecut-agent-config-observation-v1', 'status': 'blocked', 'phase': 'private_context',
        'source_head': HEAD, 'app_pid': args.app_pid, 'product_verdict': 'not_tested', 'actions': [],
        'screenshots': [], 'configuration_pixels_captured': False, 'field_values_read': False,
        'permissions_granted': False, 'provider_started': False, 'MCP_started': False,
        'cleanup_owner': 'launcher'}
    directory = None

    def command(argv):
        left = end - time.monotonic()
        if left < 0.25:
            raise Stop('deadline_no_further_UI')
        return subprocess.run(argv, check=True, capture_output=True, text=True, timeout=min(8, left)).stdout

    def owned_window():
        if (int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id):
            raise Stop('owned_window_or_focus_changed')

    def probe(name):
        completed = subprocess.run([args.probe_python, '-B', str(Path(__file__).with_name('agent-config-probe.py')),
            '--app-pid', str(args.app_pid), '--owned-root-pid', str(args.app_pid), '--output', str(directory),
            '--output-name', name + '.json', '--deadline-monotonic', str(min(end, time.monotonic() + 3)),
            '--private-accessibility-bus'], capture_output=True, timeout=min(5, max(0.25, end - time.monotonic())))
        path = directory / (name + '.json')
        if completed.returncode != 0 or not path.is_file() or path.is_symlink() or path.stat().st_size > 131072:
            raise Stop('safe_public_metadata_unavailable')
        data = json.loads(path.read_bytes())
        if data.get('app_pid') != args.app_pid:
            raise Stop('same_App_metadata_required')
        return data

    def action(intent, filename):
        owned_window()
        report['actions'].append({'intent': intent, 'kind': 'one_public_Action_attempt'})
        result = subprocess.run([args.probe_python, '-B', str(Path(__file__).with_name('agent-config-action.py')),
            '--app-pid', str(args.app_pid), '--window-id', str(args.window_id), '--output', str(directory),
            '--intent', intent, '--output-name', filename, '--deadline-monotonic', str(min(end, time.monotonic() + 8)),
            '--private-accessibility-bus'], capture_output=True, timeout=min(9, max(0.25, end - time.monotonic())))
        if result.returncode != 0:
            raise Stop('configuration_open_Action_unconfirmed' if intent == 'open-settings' else 'configuration_close_Action_unconfirmed')
        if end - time.monotonic() < 1:
            raise Stop('deadline_no_further_UI')
        time.sleep(0.7)

    try:
        if (args.expected_sha != HEAD or not args.private_accessibility_bus or not args.isolated_display_capture
                or not os.environ.get('DISPLAY') or end <= time.monotonic()):
            raise Stop('private_context_required')
        entry = Path(__file__).with_name('agent-native-entry-controller.py')
        if not entry.is_file() or entry.is_symlink() or hashlib.sha256(entry.read_bytes()).hexdigest() != ENTRY_SHA:
            raise Stop('entry_dependency_unconfirmed')
        report['phase'] = 'reuse_entry'
        argv = sys.argv[1:].copy()
        argv[argv.index('--deadline-monotonic') + 1] = str(end + 15)
        result = subprocess.run([sys.executable, '-B', str(entry), *argv], capture_output=True, timeout=max(0.25, end - time.monotonic()))
        prior = args.output / 'main-qa-agent-native-entry' / 'agent-native-entry.json'
        if result.returncode != 0 or not prior.is_file() or prior.is_symlink() or prior.stat().st_size > 16384:
            raise Stop('entry_observation_incomplete')
        previous = json.loads(prior.read_bytes())
        if (previous.get('status') != 'assistant_panel_observed_main_review_required'
                or previous.get('app_pid') != args.app_pid or previous.get('source_head') != HEAD
                or len(previous.get('captures', [])) != 5):
            raise Stop('entry_report_identity_unconfirmed')
        if not args.output.is_absolute() or args.output.is_symlink() or args.output.stat().st_uid != os.getuid():
            raise Stop('fresh_owned_output_required')
        directory = args.output / 'main-qa-agent-config'
        directory.mkdir(mode=0o700)
        ui = read_ui(Path(__file__).with_name('agent-config-ui.json'))
        report['phase'] = 'restore_1280'
        owned_window()
        command(['xdotool', 'windowsize', '--sync', str(args.window_id), '1280', '900'])
        time.sleep(0.7)
        owned_window()
        fields = dict(line.split('=', 1) for line in command(['xdotool', 'getwindowgeometry', '--shell', str(args.window_id)]).splitlines() if '=' in line)
        if fields.get('WIDTH') != '1280' or fields.get('HEIGHT') != '900':
            raise Stop('restored_geometry_unconfirmed')
        report['phase'] = 'fresh_conversation'
        if action_target(probe('01-restored-panel-public'), ui, 'open-settings') is None:
            raise Stop('fresh_conversation_panel_unconfirmed')
        report['phase'] = 'open_settings'
        action('open-settings', '02-open-settings-action.json')
        report['phase'] = 'configuration_metadata_only'
        data = probe('03-config-public')
        if not panel_state(data, ui, 'configuration'):
            raise Stop('configuration_state_unconfirmed_no_retry')
        report['configuration_state_observed'] = True
        report['configuration_editable_nodes'] = [
            {k: n.get(k) for k in ('path', 'role', 'bounds', 'showing', 'enabled', 'sensitive', 'editable', 'editable_text_interface')}
            for n in data['nodes'] if n.get('showing') and (n.get('entry') or n.get('editable') or n.get('editable_text_interface'))]
        report['phase'] = 'close_settings_panel'
        action('close-from-settings', '04-close-assistant-action.json')
        report['phase'] = 'closed_context'
        closed = closed_context(probe('05-after-close-public'))
        if closed is None:
            raise Stop('panel_disappearance_or_canvas_unconfirmed')
        report.update(closed)
        report['status'] = ('config_closed_focus_verified_main_review_required' if closed['focus_on_known_trigger']
                            else 'config_closed_focus_unconfirmed_main_review_required')
        report['phase'] = 'complete'
    except Stop as exc:
        report['status'] = 'blocked'; report['blocking_reason'] = exc.code
    except subprocess.TimeoutExpired:
        report['status'] = 'blocked'; report['blocking_reason'] = 'owned_subprocess_deadline_no_retry'
    except subprocess.CalledProcessError:
        report['status'] = 'blocked'; report['blocking_reason'] = 'owned_UI_command_failed_no_retry'
    except Exception:
        report['status'] = 'blocked'; report['blocking_reason'] = 'unexpected_OS_or_UI_error_raw_withheld'
    finally:
        if directory is not None:
            raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
            files = [f for folder in (args.output / 'main-qa-agent-native-bootstrap', args.output / 'main-qa-agent-native-entry', directory)
                     for f in folder.iterdir() if f.is_file()]
            pngs = [f for f in files if f.suffix == '.png']
            if (len(raw) <= 16384 and len(pngs) <= 10 and sum(f.stat().st_size for f in pngs) <= 14 * 1024 * 1024
                    and sum(f.stat().st_size for f in files) + len(raw) <= 15 * 1024 * 1024):
                path = directory / 'agent-config.json'
                with path.open('xb') as stream:
                    os.chmod(path, 0o600); stream.write(raw)
            else:
                report['status'] = 'blocked'; report['blocking_reason'] = 'combined_evidence_budget_exceeded'
        print(json.dumps({k: report[k] for k in ('status', 'phase', 'blocking_reason') if k in report}))
    return 0 if report['status'] in ('config_closed_focus_verified_main_review_required',
                                   'config_closed_focus_unconfirmed_main_review_required') else 2


if __name__ == '__main__':
    raise SystemExit(main())
