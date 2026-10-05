#!/usr/bin/env python3
"""Read bounded public AT-SPI metadata; never reads field values or captures pixels.

Only the explicitly supplied same-user App is queried on a launcher-attested
private accessibility bus. Unknown widget names are omitted, not printed.
This is a capability probe, not a product test or a UI action executor.
"""
import argparse
import json
import os
from pathlib import Path
import time


SAFE_LABELS = frozenset({
    '快速模式', '专业模式', '画布', '新建项目', '新建画布', '创建',
    '设置', '权限', '客户端', '实例', '实例 ID', '读取', '编辑',
    '允许读取', '读取与预览', '撤销读取授权', '移动已选图像层',
    '允许移动 · 5 分钟', '续期 5 分钟', '撤销编辑授权',
    '保存画布工程', '打开画布', '打开本机图片', '打开画布工程',
    '从资产库选择图片', '导出', '取消', '确定', '保存', '打开', '复制',
    'Open', 'Save', 'Cancel', 'Copy', 'Location', 'Name', 'File name',
    'Filename', '文件名', '路径', '位置',
})


def safe_label(value):
    return value if isinstance(value, str) and value in SAFE_LABELS else None


def collect(app_pid, deadline):
    import gi
    gi.require_version('Atspi', '2.0')
    from gi.repository import Atspi
    desktop = Atspi.get_desktop(0)
    if desktop is None:
        raise RuntimeError('public_accessibility_unavailable')
    root = None
    for index in range(min(desktop.get_child_count(), 64)):
        if time.monotonic() >= deadline:
            raise RuntimeError('public_accessibility_deadline')
        candidate = desktop.get_child_at_index(index)
        # Do not read another App's names, values, or widget tree.
        if candidate is not None and candidate.get_process_id() == app_pid:
            if root is not None:
                raise RuntimeError('ambiguous_public_app_root')
            root = candidate
    if root is None:
        raise RuntimeError('target_not_exposed_by_public_accessibility')
    pending = [(root, [], 0)]
    nodes = []
    complete = True
    while pending:
        if time.monotonic() >= deadline or len(nodes) >= 512:
            complete = False
            break
        node, path, depth = pending.pop()
        if depth > 12:
            complete = False
            continue
        states = node.get_state_set()
        role = node.get_role()
        record = {'path': path, 'role': int(role), 'label': safe_label(node.get_name()),
                  'showing': bool(states.contains(Atspi.StateType.SHOWING)),
                  'enabled': bool(states.contains(Atspi.StateType.ENABLED)),
                  'focused': bool(states.contains(Atspi.StateType.FOCUSED)),
                  'modal': bool(states.contains(Atspi.StateType.MODAL)),
                  'file_chooser': role == Atspi.Role.FILE_CHOOSER,
                  'dialog': role == Atspi.Role.DIALOG}
        try:
            component = node.get_component_iface()
            extent = component.get_extents(Atspi.CoordType.SCREEN) if component else None
            if extent:
                record['bounds'] = {'x': int(extent.x), 'y': int(extent.y),
                                    'width': int(extent.width), 'height': int(extent.height)}
        except Exception:
            record['bounds_unavailable'] = True
        # No get_text/get_value, action execution, clipboard read or raw screenshot.
        nodes.append(record)
        count = node.get_child_count()
        if count > 128:
            complete = False
        for index in reversed(range(min(count, 128))):
            child = node.get_child_at_index(index)
            if child is not None:
                pending.append((child, path + [index], depth + 1))
    return {'nodes': nodes, 'coverage_complete': complete,
            'scope': 'target-App-only; native picker in another process is not covered'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-pid', type=int, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--private-accessibility-bus', action='store_true')
    args = parser.parse_args()
    started = time.monotonic()
    report = {'status': 'blocked', 'app_pid': args.app_pid,
              'field_values_read': False, 'ui_actions': [], 'screenshots': [],
              'product_verdict': 'not_tested'}
    try:
        if not args.private_accessibility_bus or not os.environ.get('DISPLAY'):
            raise RuntimeError('private_accessibility_bus_attestation_required')
        if args.app_pid < 2 or (Path('/proc') / str(args.app_pid)).stat().st_uid != os.getuid():
            raise RuntimeError('target_App_not_same_user')
        if (not args.output.is_absolute() or args.output.is_symlink()
                or not args.output.is_dir() or args.output.stat().st_uid != os.getuid()):
            raise RuntimeError('explicit_owned_output_directory_required')
        data = collect(args.app_pid, min(args.deadline_monotonic, started + 3))
        report.update(data, status='public_metadata_observed')
    except Exception:
        # Suppress exception text: IPC errors may contain widget names or field values.
        report['status'] = 'public_accessibility_unavailable_or_input_blocked'
    report['elapsed_seconds'] = round(time.monotonic() - started, 3)
    raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
    while len(raw) > 128 * 1024 and report.get('nodes'):
        report['nodes'].pop()
        report['coverage_complete'] = False
        raw = (json.dumps(report, ensure_ascii=False, separators=(',', ':')) + '\n').encode()
    if (args.output.is_absolute() and args.output.is_dir() and not args.output.is_symlink()
            and args.output.stat().st_uid == os.getuid()):
        path = args.output / 'public-accessibility.json'
        with path.open('xb') as stream:
            os.chmod(path, 0o600)
            stream.write(raw)
    print(raw.decode(), end='')
    return 0 if report['status'] == 'public_metadata_observed' else 2


if __name__ == '__main__':
    raise SystemExit(main())
