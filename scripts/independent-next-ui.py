#!/usr/bin/env python3
"""One reviewed mode click, three workbench sizes and five sidebar hovers.

No App/client launch, Settings entry, hidden control clicks or project mutation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

HEAD = 'cecc8fdf9578675051dae58bda25f0ff805ce235'
APP_SHA = '8859b10e7b8a58785d6a60454995f33d56efea554011825287917d06064aff55'


class Stop(Exception):
    pass


def read_declaration(path):
    if (not path.is_absolute() or path.is_symlink() or not path.is_file()
            or path.stat().st_size > 16384):
        raise Stop('explicit_regular_declaration_required')
    raw = path.read_bytes()
    return json.loads(raw), hashlib.sha256(raw).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-pid', type=int, required=True)
    parser.add_argument('--work-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--identity-approval', type=Path, required=True)
    parser.add_argument('--ui-approval', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--window-id', type=int)
    parser.add_argument('--expected-sha', default=HEAD)
    parser.add_argument('--client-binary', type=Path)  # Launcher compatibility; never accessed/launched.
    parser.add_argument('--next-stage', choices=['navigation'], default='navigation')
    parser.add_argument('--isolated-display-capture', action='store_true')
    args = parser.parse_args()
    start = time.monotonic()
    deadline = min(args.deadline_monotonic - 15, start + 45)
    evidence = None
    report = {'phase': 'cecc8fd-fresh-workbench-observation', 'status': 'blocked',
              'source_head': HEAD, 'product_verdict': 'pending_independent_actual_image_review',
              'actions': [], 'captures': [], 'client_started': False,
              'permissions_granted': False, 'project_created': False,
              'paid_action_requested': False, 'settings_entered': False,
              'cleanup_owner': 'launcher; whole App run including cleanup <=300 seconds'}

    def command(argv, binary=False):
        left = deadline - time.monotonic()
        if left < 0.25:
            raise Stop('deadline_no_further_input')
        try:
            return subprocess.run(argv, check=True, capture_output=True,
                                  text=not binary, timeout=min(8, left)).stdout
        except (subprocess.TimeoutExpired, subprocess.CalledProcessError):
            raise Stop('public_UI_command_unavailable_no_retry_raw_output_withheld') from None

    def pause():
        if deadline - time.monotonic() < 1:
            raise Stop('insufficient_capture_time')
        time.sleep(0.7)

    def snapshot(name):
        path = evidence / (name + '.png')
        if path.exists():
            raise Stop('capture_path_already_exists')
        command(['import', '-window', str(window), '-strip', str(path)])
        os.chmod(path, 0o600)
        raw = path.read_bytes()
        if len(raw) > 2 * 1024 * 1024 or sum(x['bytes'] for x in report['captures']) + len(raw) > 14 * 1024 * 1024:
            raise Stop('PNG_budget_exceeded_do_not_publish')
        report['captures'].append({'file': path.name, 'bytes': len(raw),
                                   'sha256': hashlib.sha256(raw).hexdigest(), 'surface': 'App-window'})
        return path

    def geometry(width):
        if int(command(['xdotool', 'getwindowpid', str(window)]).strip()) != args.app_pid:
            raise Stop('App_window_identity_changed')
        command(['xdotool', 'windowsize', '--sync', str(window), str(width), '900'])
        pause()
        fields = dict(line.split('=', 1) for line in command(
            ['xdotool', 'getwindowgeometry', '--shell', str(window)]).splitlines() if '=' in line)
        if fields.get('WIDTH') != str(width) or fields.get('HEIGHT') != '900':
            raise Stop('actual_window_size_differs')
        x, y = int(fields['X']), int(fields['Y'])
        if x < 0 or y < 0 or x + width > int(dims[0]) or y + 900 > int(dims[1]):
            raise Stop('App_window_outside_private_display')
        report['actions'].append({'kind': 'resize', 'width': width, 'height': 900})

    def modal_digest(frame):
        x, y, width, height = ui['mode_guard']['region']
        raw = command(['convert', str(frame), '-crop', f'{width}x{height}+{x}+{y}',
                       '+repage', '-alpha', 'off', '-depth', '8', 'rgb:-'], binary=True)
        if len(raw) != width * height * 3:
            raise Stop('mode_guard_RGB_size_differs')
        return hashlib.sha256(raw).hexdigest()

    try:
        if args.expected_sha != HEAD or args.app_pid < 2 or deadline <= start:
            raise Stop('exact_candidate_and_live_deadline_required')
        if not args.isolated_display_capture or not os.environ.get('DISPLAY'):
            raise Stop('private_QA_display_attestation_required')
        runtime, runtime_sha = read_declaration(args.identity_approval)
        ui, ui_sha = read_declaration(args.ui_approval)
        if (runtime.get('schema') != 2 or runtime.get('reviewed_by') != 'main-reviewer'
                or runtime.get('runtime_head') != HEAD or runtime.get('runtime_app_sha256') != APP_SHA
                or runtime.get('change_scope') != 'product-candidate'):
            raise Stop('main_exact_runtime_identity_required')
        if (ui.get('schema') != 1 or ui.get('reviewed_by') != 'independent-qa'
                or ui.get('observed_head') != HEAD or ui.get('observed_app_sha256') != APP_SHA
                or ui.get('stage') != 'fresh-workbench-observation'
                or ui.get('window') != {'width': 1280, 'height': 900}
                or ui.get('quick_mode') != [558, 500]
                or ui.get('hover_targets') != [[40, 170], [40, 227], [40, 285], [40, 748], [40, 805]]
                or ui.get('mode_guard', {}).get('region') != [420, 388, 520, 155]):
            raise Stop('independent_current_UI_declaration_required')
        for path in (args.work_dir, args.output):
            if not path.is_absolute() or path.is_symlink() or not path.is_dir() or path.stat().st_uid != os.getuid():
                raise Stop('explicit_owned_isolated_directory_required')
        proc = Path('/proc') / str(args.app_pid)
        if proc.stat().st_uid != os.getuid():
            raise Stop('App_same_OS_user_required')
        digest = hashlib.sha256()
        with (proc / 'exe').open('rb') as stream:
            while True:
                if time.monotonic() >= deadline:
                    raise Stop('deadline_during_binary_verification')
                data = stream.read(1024 * 1024)
                if not data:
                    break
                digest.update(data)
        if digest.hexdigest() != APP_SHA:
            raise Stop('actual_App_SHA_differs')
        report.update(app_sha256=digest.hexdigest(), identity_declaration_sha256=runtime_sha,
                      UI_declaration_sha256=ui_sha)
        dims = command(['xdotool', 'getdisplaygeometry']).split()
        if len(dims) != 2 or not (1440 <= int(dims[0]) <= 1920 and 900 <= int(dims[1]) <= 1200):
            raise Stop('private_display_dimensions_outside_finite_bounds')
        if args.window_id is None:
            ids = set(command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)]).split())
            if len(ids) != 1:
                raise Stop('one_visible_App_window_required')
            window = int(ids.pop())
        else:
            window = args.window_id
        if str(window) not in command(['xdotool', 'search', '--onlyvisible', '--pid', str(args.app_pid)]).split():
            raise Stop('explicit_App_window_not_visible')
        new_evidence = args.output / 'independent-qa-cecc8fd-ui1'
        new_evidence.mkdir(mode=0o700)
        evidence = new_evidence
        geometry(1280)
        command(['xdotool', 'windowfocus', '--sync', str(window)])
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        before = snapshot('01-before-quick')
        if modal_digest(before) != ui['mode_guard']['rgb_sha256']:
            raise Stop('current_mode_dialog_differs_no_click')
        command(['xdotool', 'mousemove', '--window', str(window), '558', '500'])
        command(['xdotool', 'click', '1'])
        report['actions'].append({'kind': 'click', 'target': 'visible_quick_mode', 'xy': [558, 500]})
        command(['xdotool', 'mousemove', '--window', str(window), '100', '700'])
        pause()
        after = snapshot('02-after-quick-1280x900')
        if modal_digest(after) == ui['mode_guard']['rgb_sha256']:
            raise Stop('reviewed_mode_dialog_still_visible')
        for width in (1024, 1440):
            geometry(width)
            snapshot('03-workbench-' + str(width) + 'x900')
        geometry(1280)
        for index, (x, y) in enumerate(ui['hover_targets'], 4):
            command(['xdotool', 'mousemove', '--window', str(window), str(x), str(y)])
            report['actions'].append({'kind': 'hover', 'xy': [x, y], 'label': 'unknown_until_actual_review'})
            pause()
            snapshot(f'{index:02d}-sidebar-hover-{y}')
        report['status'] = 'finite_observation_completed_review_pending'
    except (Stop, OSError, ValueError, KeyError, TypeError) as exc:
        report['blocking_reason'] = str(exc) if isinstance(exc, Stop) else 'public_runtime_unavailable_raw_error_withheld'
    finally:
        report['elapsed_seconds'] = round(time.monotonic() - start, 3)
        raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
        if evidence is not None:
            if sum(p.stat().st_size for p in evidence.iterdir() if p.is_file()) + len(raw) > 15 * 1024 * 1024:
                report['status'] = 'artifact_budget_exceeded_do_not_publish'
                raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
            with (evidence / 'navigation.json').open('xb') as stream:
                os.chmod(evidence / 'navigation.json', 0o600)
                stream.write(raw)
        print(raw.decode(), end='')
    return 0 if report['status'] == 'finite_observation_completed_review_pending' else 2


if __name__ == '__main__':
    raise SystemExit(main())
