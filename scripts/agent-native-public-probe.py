#!/usr/bin/env python3
"""Reuse the reviewed no-values probe with a closed assistant label allowlist.

Labels originate in frozen 11ebf20 source; this is discovery, never UI approval.
The underlying probe excludes editable/ENTRY/TEXT names before name access.
"""
import hashlib
import importlib.util
from pathlib import Path

PROBE_SHA = 'ef194ed6b55e545c922308f875aed184d76490530c8f2a88a27459ce3f1994bd'
ASSISTANT_LABELS = frozenset({
    '助手', '打开工程后使用助手', '助手设置', '关闭助手', '返回对话',
    '连接', '断开', '正在连接…', '正在断开…', '尚未连接',
    '允许读取与预览', '撤销读取与预览', '允许移动 · 5分钟',
    '续期 · 5分钟', '撤销移动权限', '拒绝', '允许', '发送', '停止',
    '本地助手组件', '收起本地助手组件', '选择运行环境', '选择助手入口',
    '选择MCP 客户端', '查看详情', '收起详情',
})


def load_probe():
    path = Path(__file__).with_name('public_probe_11ebf20_ui4.py')
    if not path.is_file() or path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != PROBE_SHA:
        raise ValueError('reviewed_public_probe_SHA_required')
    spec = importlib.util.spec_from_file_location('agent_native_safe_probe', path)
    module = importlib.util.module_from_spec(spec)
    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
    module.SAFE_LABELS |= ASSISTANT_LABELS
    original_collect = module.collect
    def collect(pid, deadline):
        data = original_collect(pid, deadline)
        root, Atspi = module.target_root(pid, deadline)
        for record in data['nodes']:
            if (record.get('role') != 62 or record.get('label') != '助手'
                    or record.get('bounds') != {'x': 18, 'y': 726, 'width': 44, 'height': 44}
                    or record.get('editable') or record.get('editable_text_interface')
                    or not record.get('action_interface')):
                continue
            node = root
            for index in record['path']:
                node = node.get_child_at_index(index)
            node.clear_cache_single()
            if (node.get_role() != Atspi.Role.TOGGLE_BUTTON or node.get_name() != '助手'
                    or node.get_state_set().contains(Atspi.StateType.EDITABLE)
                    or 'EditableText' in set(node.get_interfaces())):
                continue
            action = node.get_action_iface()
            record['allowed_actions'] = [action.get_action_name(i) for i in range(min(action.get_n_actions(), 8))
                                         if action.get_action_name(i) in ('click', 'activate', 'press')]
        return data
    module.collect = collect
    return module


if __name__ == '__main__':
    raise SystemExit(load_probe().main())
