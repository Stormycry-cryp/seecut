#!/usr/bin/env python3
"""Fixed configuration captions only; no editable names, values or pixels."""
import hashlib
import importlib.util
from pathlib import Path

BASE_SHA = '20456d60ce8ca14f72941026e0b56e97eebbf552267d991d61bf3be7b867702f'
CONFIG_LABELS = frozenset({'服务地址', '模型', 'API 密钥', '图片支持', '服务支持图片',
    '运行环境', '助手入口', 'MCP 客户端', '尚未选择', '请填写服务地址', '请填写模型名称',
    '请填写本次会话的 API 密钥', '请先允许读取与预览', '请选择运行环境文件',
    '请选择助手入口文件', '请选择 MCP 客户端文件', '连接只初始化助手，不发送问题。'})


def load_probe():
    path = Path(__file__).with_name('agent-native-public-probe.py')
    if not path.is_file() or path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != BASE_SHA:
        raise ValueError('pinned_native_probe_dependency_required')
    spec = importlib.util.spec_from_file_location('config_native_probe', path)
    module = importlib.util.module_from_spec(spec)
    exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
    public = module.load_probe()
    public.SAFE_LABELS |= CONFIG_LABELS
    return public


if __name__ == '__main__':
    raise SystemExit(load_probe().main())
