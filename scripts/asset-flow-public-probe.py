#!/usr/bin/env python3
"""Read bounded public AT-SPI metadata; never reads field values or captures pixels.

Only the App or its verified same-user descendant is queried on a launcher-attested
private accessibility bus. Unknown widget names and file-list children are omitted.
This is a capability probe, not a product test or a UI action executor.
"""
import argparse
import json
import os
from pathlib import Path
import time


SAFE_LABELS = frozenset({
    '批量管理', '完成批量管理', '退出批量管理', '选择素材', '取消选择',
    '加入画布', '0 项', '1 项', '已选 0 项', '已选 1 项', '没有找到相关素材',
    '导入素材', '快速模式', '专业模式', '画布', '新建项目', '新建画布', '创建',
    '设置', '权限', '客户端', '实例', '实例 ID', '读取', '编辑',
    '允许读取', '读取与预览', '撤销读取授权', '移动已选图像层',
    '允许移动 · 5 分钟', '续期 5 分钟', '撤销编辑授权',
    '保存画布工程', '打开画布', '打开本机图片', '打开画布工程',
    '从资产库选择图片', '导出', '取消', '确定', '保存', '打开', '复制',
    'Open', 'Save', 'Cancel', 'Copy', 'Location', 'Name', 'File name',
    'Filename', '文件名', '路径', '位置',
    'Open Image', 'Open File', 'Select a File', 'Save As', 'Export',
    '导出至资产库', '导出至其他文件夹', '导出到其他文件夹', '撤销', 'Undo',
    'OK', 'Ok', 'Images',
    '生成', '剪辑', '资产库', '图片', '视频', '主题', '外观', '通用', '常规',
    '浅色', '深色', '跟随系统', '系统', '工作模式', '快速', '专业',
    '本地 Agent 权限', 'MCP', 'MCP Server', 'MCP 服务',
    'Settings', 'General', 'Appearance', 'Theme', 'Light', 'Dark', 'System',
    'Quick mode', 'Professional mode', 'Local Agent Permissions',
    '浅色主题', '深色主题', '浅色模式', '深色模式', 'Light theme', 'Dark theme',
    'Quick', 'Professional', '关闭', '关闭设置', 'Close', 'Close settings',
    'Close Settings', '未命名画布', '图层', '属性', '调整',
    'Untitled Canvas', 'Layers', 'Properties', 'Adjustments',
})


# Safe composite names are built only from the already allowed setting labels.
# Unknown names and every editable field remain excluded before name access.
SAFE_LABELS = SAFE_LABELS | frozenset(
    parent + separator + option
    for parent, options in (
        ('工作模式', ('快速', '快速模式', '专业', '专业模式')),
        ('外观', ('浅色', '浅色主题', '浅色模式', '深色', '深色主题', '深色模式')),
        ('Work mode', ('Quick', 'Quick mode', 'Professional', 'Professional mode')),
        ('Appearance', ('Light', 'Light theme', 'Dark', 'Dark theme')))
    for separator in ('，', ', ', ': ', '：')
    for option in options)


def safe_label(value):
    return value if isinstance(value, str) and value in SAFE_LABELS else None


def target_root(app_pid, deadline):
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
    return root, Atspi


def collect(app_pid, deadline):
    root, Atspi = target_root(app_pid, deadline)
    toolkit = root.get_toolkit_name()
    toolkit = 'GTK' if isinstance(toolkit, str) and toolkit.lower().startswith('gtk') else 'unknown'
    pending = [(root, [], 0)]
    nodes = []
    complete = True
    while pending:
        if time.monotonic() >= deadline or len(nodes) >= 512:
            complete = False
            break
        node, path, depth = pending.pop()
        if depth > 24:
            complete = False
            continue
        node.clear_cache_single()
        states = node.get_state_set()
        role = node.get_role()
        skip_children = role in (Atspi.Role.TABLE, Atspi.Role.TREE, Atspi.Role.TREE_TABLE,
                                 Atspi.Role.LIST, Atspi.Role.DIRECTORY_PANE)
        interfaces = set(node.get_interfaces())
        editable = bool(states.contains(Atspi.StateType.EDITABLE))
        no_name = (skip_children or role in (Atspi.Role.ENTRY, Atspi.Role.TEXT)
                   or editable or 'EditableText' in interfaces)
        record = {'path': path, 'role': int(role), 'label': None if no_name else safe_label(node.get_name()),
                  'showing': bool(states.contains(Atspi.StateType.SHOWING)),
                  'enabled': bool(states.contains(Atspi.StateType.ENABLED)),
                  'sensitive': bool(states.contains(Atspi.StateType.SENSITIVE)),
                  'focused': bool(states.contains(Atspi.StateType.FOCUSED)),
                  'focusable': bool(states.contains(Atspi.StateType.FOCUSABLE)),
                  'selected': bool(states.contains(Atspi.StateType.SELECTED)),
                  'checked': bool(states.contains(Atspi.StateType.CHECKED)),
                  'modal': bool(states.contains(Atspi.StateType.MODAL)),
                  'file_chooser': role == Atspi.Role.FILE_CHOOSER,
                  'dialog': role == Atspi.Role.DIALOG,
                  'button': role == Atspi.Role.PUSH_BUTTON,
                  'radio': role == Atspi.Role.RADIO_BUTTON,
                  'panel': role == Atspi.Role.PANEL,
                  'entry': role in (Atspi.Role.ENTRY, Atspi.Role.TEXT),
                  'editable': editable}
        record['editable_text_interface'] = 'EditableText' in interfaces
        record['action_interface'] = 'Action' in interfaces
        if (record['button'] or record['radio']) and record['action_interface']:
            action = node.get_action_iface()
            record['allowed_actions'] = [action.get_action_name(index) for index in range(min(action.get_n_actions(), 8))
                                         if action.get_action_name(index) in ('click', 'activate', 'press')]
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
        if skip_children:
            # Never traverse native file lists or read their item names/values.
            continue
        count = node.get_child_count()
        if count > 128:
            complete = False
        for index in reversed(range(min(count, 128))):
            child = node.get_child_at_index(index)
            if child is not None:
                pending.append((child, path + [index], depth + 1))
    return {'nodes': nodes, 'toolkit': toolkit, 'coverage_complete': complete,
            'scope': 'one explicitly verified App or owned descendant; file-list children omitted'}


def owned_by_app(target, owner):
    visited = set()
    while target >= 2 and target not in visited and len(visited) < 16:
        visited.add(target)
        proc = Path('/proc') / str(target)
        if proc.stat().st_uid != os.getuid():
            return False
        if target == owner:
            return True
        parent = next((line for line in (proc / 'status').read_text().splitlines() if line.startswith('PPid:')), None)
        if parent is None:
            return False
        target = int(parent.split()[1])
    return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-pid', type=int, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--deadline-monotonic', type=float, required=True)
    parser.add_argument('--private-accessibility-bus', action='store_true')
    parser.add_argument('--owned-root-pid', type=int, help='App parent PID; target must be this App or its verified descendant')
    parser.add_argument('--output-name', default='public-accessibility.json',
                        help='One bounded flat metadata filename, never a path')
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
        if args.owned_root_pid is not None and not owned_by_app(args.app_pid, args.owned_root_pid):
            raise RuntimeError('target_not_owned_by_App')
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
        if (not args.output_name.endswith('.json') or len(args.output_name) > 80
                or any(c not in 'abcdefghijklmnopqrstuvwxyz0123456789-.' for c in args.output_name)
                or '..' in args.output_name):
            raise RuntimeError('flat_metadata_filename_required')
        path = args.output / args.output_name
        with path.open('xb') as stream:
            os.chmod(path, 0o600)
            stream.write(raw)
    print(raw.decode(), end='')
    return 0 if report['status'] == 'public_metadata_observed' else 2


if __name__ == '__main__':
    raise SystemExit(main())
