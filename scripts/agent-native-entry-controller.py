#!/usr/bin/env python3
"""Five-frame A3-derived bootstrap, one guarded assistant open, panel size review.

Preparation only. The launcher must explicitly register this controller/helpers
and both bounded output directories. This does not fit the old A3 allowlist.
One public assistant-entry Action only; no settings, credentials, grants or provider.
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
BOOTSTRAP_SHA = '4dfd9d275a5639e85a3ebce71fbaa620e1e3ca3f9982199887d04a65f6933439'
ICON_RGB_SHA = 'c2023327de09beb6145452019a446793537154cfb10572a2928c90e014e8862b'
ICON_BOUNDS = {'x': 18, 'y': 726, 'width': 44, 'height': 44}
PANEL_CONFIG_LABELS = frozenset({'返回对话', '本地助手组件', '收起本地助手组件',
                               '选择运行环境', '选择助手入口', '选择MCP 客户端'})


GUARD_CODES = frozenset(['actual_panel_window_size_differs_no_pixels', 'combined_evidence_budget_exceeded', 'PNG_limit_exceeded', 'actual_editor_or_assistant_icon_unknown_metadata_only_stop', 'assistant_target_changed_no_hover', 'current_assistant_icon_RGB_differs_no_hover', 'deadline_no_capture', 'deadline_no_further_input', 'entry_action_unconfirmed_no_retry', 'entry_changed_after_hover_no_open', 'exact_private_QA_context_required', 'fresh_bootstrap_creation_identity_required', 'fresh_bootstrap_same_App_path_incomplete_no_retry', 'fresh_capture_required', 'hover_context_changed_no_pixels', 'public_metadata_unavailable_no_retry', 'reviewed_bootstrap_SHA_required', 'same_App_metadata_required', 'same_live_App_window_and_focus_required'])


class GuardStop(Exception):
    def __init__(self, code):
        self.code = code if code in GUARD_CODES else 'unknown_guard_code_withheld'


BOOTSTRAP_FAILURE_CODES = frozenset(['App_same_OS_user_required', 'App_window_identity_changed', 'App_window_outside_private_display', 'PNG_budget_exceeded_do_not_publish', 'Settings_pixels_forbidden_until_complete_public_close_proof', 'actual_App_SHA_differs', 'actual_blank_editor_requires_review_no_further_input', 'actual_window_size_differs', 'capture_path_already_exists', 'current_create_dialog_public_context_changed_no_create', 'current_mode_dialog_differs_no_click', 'deadline_during_binary_verification', 'deadline_no_further_input', 'exact_A2_control_scope_required', 'exact_candidate_and_live_deadline_required', 'exact_reviewed_create_point_required', 'explicit_App_window_not_visible', 'explicit_owned_isolated_directory_required', 'explicit_regular_declaration_required', 'focus_outside_reviewed_App_no_key_or_click', 'independent_current_UI_declaration_required', 'insufficient_capture_time', 'main_exact_runtime_identity_required', 'mode_guard_RGB_size_differs', 'one_visible_App_window_required', 'private_QA_display_attestation_required', 'private_display_dimensions_outside_finite_bounds', 'public_UI_command_unavailable_no_retry_raw_output_withheld', 'public_probe_timeout_no_retry', 'public_settings_metadata_incomplete_no_further_input', 'reviewed_mode_dialog_still_visible', 'reviewed_public_action_SHA_required', 'reviewed_public_probe_SHA_required', 'current_navigation_control_differs:nav-canvas', 'current_navigation_control_differs:new-project', 'current_navigation_control_differs:current-create-dialog'])


def bootstrap_code(value):
    return value if isinstance(value, str) and value in BOOTSTRAP_FAILURE_CODES else 'bootstrap_unexpected_error_raw_withheld'


def hover_candidate(data):
    """Only currently observed editor metadata; historical PID/path never authorizes."""
    if (data.get('status') != 'public_metadata_observed' or data.get('coverage_complete') is not True
            or data.get('field_values_read') is not False or data.get('ui_actions') != []):
        return None
    showing = [n for n in data.get('nodes', []) if n.get('showing') is True]
    labels = {n.get('label') for n in showing}
    if (not {'画布', '属性', '图层'}.issubset(labels)
            or labels & {'新建画布', '工作模式', '外观', '助手设置', '关闭助手', '返回对话'}
            or any(n.get('dialog') or n.get('modal') for n in showing)):
        return None
    windows = [n for n in showing if n.get('role') == 23 and n.get('path') == [0]
               and n.get('bounds') == {'x': 0, 'y': 0, 'width': 1280, 'height': 900}]
    targets = [n for n in showing if n.get('bounds') == ICON_BOUNDS and n.get('role') == 62
               and n.get('label') in (None, '助手')
               and all(n.get(k) is True for k in ('enabled', 'sensitive', 'focusable', 'action_interface'))
               and not any(n.get(k) for k in ('entry', 'editable', 'editable_text_interface'))
               and isinstance(n.get('path'), list) and len(n['path']) == 2
               and n['path'][0] == 0 and type(n['path'][1]) is int and 0 <= n['path'][1] < 128]
    return targets[0] if len(windows) == 1 and len(targets) == 1 else None


def entry_candidate(data):
    target = hover_candidate(data)
    return target if (target and target.get('path') == [0, 5] and target.get('label') == '助手'
                      and target.get('allowed_actions') == ['click']) else None


def safe_panel_context(data, width=1280, height=900):
    if (data.get('status') != 'public_metadata_observed' or data.get('coverage_complete') is not True
            or data.get('field_values_read') is not False):
        return False
    showing = [n for n in data.get('nodes', []) if n.get('showing') is True]
    labels = {n.get('label') for n in showing}
    windows = [n for n in showing if n.get('path') == [0] and n.get('role') == 23
               and n.get('bounds') == {'x': 0, 'y': 0, 'width': width, 'height': height}]
    # Main's fresh synthetic App has never received credentials/components/provider.
    # Positive conversation-side labels and no modal/entry/config-surface indicators.
    return (len(windows) == 1 and {'助手设置', '关闭助手', '画布'}.issubset(labels)
            and not labels & PANEL_CONFIG_LABELS
            and sum(n.get('label') == '助手设置' for n in showing) == 1
            and sum(n.get('label') == '关闭助手' for n in showing) == 1
            and not any(n.get('dialog') or n.get('modal') for n in showing))


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
    report = {'schema': 'seecut-agent-native-entry-v1', 'status': 'blocked',
              'product_verdict': 'not_tested', 'source_head': HEAD, 'app_pid': args.app_pid,
              'assistant_actions': [], 'hover_attempted': False, 'captures': [],
              'field_values_read': False, 'provider_started': False, 'MCP_started': False,
              'settings_opened': False, 'permissions_granted': False, 'cleanup_owner': 'launcher'}
    directory = None

    def command(argv, binary=False):
        left = end - time.monotonic()
        if left < 0.25:
            raise GuardStop('deadline_no_further_input')
        return subprocess.run(argv, check=True, capture_output=True, text=not binary, timeout=min(8, left)).stdout

    def pause():
        if end - time.monotonic() < 1:
            raise GuardStop('deadline_no_capture')
        time.sleep(0.7)

    def owned_window():
        if (int(command(['xdotool', 'getwindowpid', str(args.window_id)]).strip()) != args.app_pid
                or int(command(['xdotool', 'getwindowfocus']).strip()) != args.window_id):
            raise GuardStop('same_live_App_window_and_focus_required')

    def probe(name):
        result = subprocess.run([args.probe_python, '-B', str(Path(__file__).with_name('agent-native-public-probe.py')),
            '--app-pid', str(args.app_pid), '--owned-root-pid', str(args.app_pid), '--output', str(directory),
            '--output-name', name + '.json', '--deadline-monotonic', str(min(end, time.monotonic() + 3)),
            '--private-accessibility-bus'], capture_output=True, timeout=min(5, max(0.25, end - time.monotonic())))
        path = directory / (name + '.json')
        if result.returncode != 0 or not path.is_file() or path.is_symlink() or path.stat().st_size > 131072:
            raise GuardStop('public_metadata_unavailable_no_retry')
        data = json.loads(path.read_bytes())
        if data.get('app_pid') != args.app_pid:
            raise GuardStop('same_App_metadata_required')
        return data

    def snapshot(name):
        if len(report['captures']) >= 5:
            raise GuardStop('PNG_limit_exceeded')
        path = directory / (name + '.png')
        if path.exists():
            raise GuardStop('fresh_capture_required')
        command(['import', '-window', str(args.window_id), '-strip', str(path)])
        path.chmod(0o600)
        if not 0 < path.stat().st_size <= 2 * 1024 * 1024:
            raise GuardStop('PNG_limit_exceeded')
        report['captures'].append({'file': path.name, 'bytes': path.stat().st_size,
                                  'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
        files = [f for folder in (args.output / 'main-qa-agent-native-bootstrap', directory)
                 for f in folder.iterdir() if f.is_file()]
        pngs = [f for f in files if f.name.endswith('.png')]
        if (len(pngs) > 10 or sum(f.stat().st_size for f in pngs) > 14 * 1024 * 1024
                or sum(f.stat().st_size for f in files) > 15 * 1024 * 1024):
            raise GuardStop('combined_evidence_budget_exceeded')
        return path

    try:
        if (args.expected_sha != HEAD or not args.isolated_display_capture or not args.private_accessibility_bus
                or not os.environ.get('DISPLAY') or end <= time.monotonic()):
            raise GuardStop('exact_private_QA_context_required')
        a3 = Path(__file__).with_name('agent-native-bootstrap.py')
        if not a3.is_file() or a3.is_symlink() or hashlib.sha256(a3.read_bytes()).hexdigest() != BOOTSTRAP_SHA:
            raise GuardStop('reviewed_bootstrap_SHA_required')
        # Same launcher App/PID/arguments. The child is the derived QA, not an App.
        bootstrap_args = sys.argv[1:].copy()
        bootstrap_args[bootstrap_args.index('--deadline-monotonic') + 1] = str(end + 15)
        bootstrap = subprocess.run([sys.executable, '-B', str(a3), *bootstrap_args], capture_output=True,
                                   timeout=max(0.25, end - time.monotonic()))
        a3_report = args.output / 'main-qa-agent-native-bootstrap' / 'bootstrap.json'
        if not a3_report.is_file() or a3_report.is_symlink() or a3_report.stat().st_size > 131072:
            raise GuardStop('fresh_bootstrap_same_App_path_incomplete_no_retry')
        result = json.loads(a3_report.read_bytes())
        if bootstrap.returncode != 0:
            report['bootstrap_failure_code'] = bootstrap_code(result.get('blocking_reason'))
            raise GuardStop('fresh_bootstrap_same_App_path_incomplete_no_retry')
        if (result.get('status') != 'finite_observation_completed_review_pending'
                or result.get('project_created') != 'visible_editor_context_pending_main_pixels_review'
                or result.get('app_sha256') != APP_SHA or result.get('scope') != 'canvas-entry'):
            raise GuardStop('fresh_bootstrap_creation_identity_required')
        directory = args.output / 'main-qa-agent-native-entry'
        directory.mkdir(mode=0o700)
        owned_window()
        command(['xdotool', 'windowsize', '--sync', str(args.window_id), '1280', '900'])
        pause()
        owned_window()
        data = probe('01-before-hover-public')
        target = hover_candidate(data)
        if target is None:
            raise GuardStop('actual_editor_or_assistant_icon_unknown_metadata_only_stop')
        before = snapshot('01-before-hover')
        raw = command(['convert', str(before), '-crop', '44x44+18+726', '+repage',
                       '-alpha', 'off', '-depth', '8', 'rgb:-'], binary=True)
        if len(raw) != 5808 or hashlib.sha256(raw).hexdigest() != ICON_RGB_SHA:
            raise GuardStop('current_assistant_icon_RGB_differs_no_hover')
        owned_window()
        # Recollect immediately before hover, then require exact path/bounds/state.
        current = hover_candidate(probe('02-recheck-public'))
        if current != target:
            raise GuardStop('assistant_target_changed_no_hover')
        report['hover_attempted'] = True
        command(['xdotool', 'mousemove', '--window', str(args.window_id), '40', '748'])
        pause()
        after = probe('03-after-hover-public')
        if hover_candidate(after) is None:
            raise GuardStop('hover_context_changed_no_pixels')
        if entry_candidate(after) is None:
            snapshot('03-assistant-tooltip-review')
            report['status'] = 'assistant_hover_observed_main_review_required'
        else:
            # The reviewed icon RGB is the unhovered state. Clear only the pointer
            # hover, then recollect; never compare hovered pixels with that baseline.
            owned_window()
            command(['xdotool', 'mousemove', '--window', str(args.window_id), '100', '700'])
            pause()
            if entry_candidate(probe('03-before-open-public')) != entry_candidate(after):
                raise GuardStop('entry_changed_after_hover_no_open')
            report['assistant_actions'].append({'kind': 'one_public_Action_attempt', 'label': '助手', 'path': [0, 5]})
            action = subprocess.run([args.probe_python, '-B', str(Path(__file__).with_name('agent-native-entry-action.py')),
                '--app-pid', str(args.app_pid), '--window-id', str(args.window_id), '--output', str(directory),
                '--deadline-monotonic', str(min(end, time.monotonic() + 8)), '--private-accessibility-bus'],
                capture_output=True, timeout=min(9, max(0.25, end - time.monotonic())))
            if action.returncode != 0:
                raise GuardStop('entry_action_unconfirmed_no_retry')
            pause()
            data = probe('04-panel-public')
            if not safe_panel_context(data):
                report['status'] = 'panel_metadata_only_main_review_required'
            else:
                snapshot('04-assistant-panel-1280x900')
                report['status'] = 'assistant_panel_observed_main_review_required'
                for index, (width, height) in enumerate(((1024, 900), (1440, 900), (1280, 720)), 5):
                    owned_window()
                    command(['xdotool', 'windowsize', '--sync', str(args.window_id), str(width), str(height)])
                    pause()
                    owned_window()
                    fields = dict(line.split('=', 1) for line in command(
                        ['xdotool', 'getwindowgeometry', '--shell', str(args.window_id)]).splitlines() if '=' in line)
                    if fields.get('WIDTH') != str(width) or fields.get('HEIGHT') != str(height):
                        raise GuardStop('actual_panel_window_size_differs_no_pixels')
                    name = f'{index:02d}-panel-{width}x{height}'
                    current = probe(name + '-public')
                    if not safe_panel_context(current, width, height):
                        report['status'] = 'panel_metadata_only_main_review_required'
                        report['blocked_panel_size'] = [width, height]
                        break
                    snapshot(f'{index:02d}-assistant-panel-{width}x{height}')
    except GuardStop as exc:
        report['status'] = 'blocked'
        report['blocking_reason'] = exc.code
    except subprocess.TimeoutExpired:
        report['status'] = 'blocked'
        report['blocking_reason'] = 'owned_UI_subprocess_deadline_no_retry'
    except subprocess.CalledProcessError:
        report['status'] = 'blocked'
        report['blocking_reason'] = 'owned_UI_command_failed_no_retry'
    except Exception:
        report['status'] = 'blocked'
        report['blocking_reason'] = 'unexpected_OS_or_UI_error_raw_withheld'
    finally:
        if directory is not None:
            raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
            files = [f for folder in (args.output / 'main-qa-agent-native-bootstrap', directory)
                     for f in folder.iterdir() if f.is_file()]
            if sum(f.stat().st_size for f in files) + len(raw) <= 15 * 1024 * 1024:
                path = directory / 'agent-native-entry.json'
                with path.open('xb') as stream:
                    os.chmod(path, 0o600)
                    stream.write(raw)
        print(json.dumps({k: report[k] for k in ('status', 'product_verdict', 'hover_attempted', 'blocking_reason') if k in report}))
    return 0 if report['status'] in ('assistant_hover_observed_main_review_required',
        'panel_metadata_only_main_review_required', 'assistant_panel_observed_main_review_required') else 2


if __name__ == '__main__':
    raise SystemExit(main())
