#!/usr/bin/env python3
"""Assemble and seal an optional local Agent bundle; never fetch private code."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import stat

ENTRIES = {
    'runtime': 'bin/node',
    'entrypoint': 'runtime/src/cli.mjs',
    'mcpClient': 'bin/seecut-editor-mcp',
}
REQUIRED = set(ENTRIES.values()) | {
    'runtime/package.json', 'runtime/LICENSE', 'build-identity.json',
    'runtime/node_modules/@coto/agent/package.json',
    'runtime/node_modules/@coto/agent/dist/core/index.js',
    'licenses/Node-LICENSE', 'licenses/THIRD_PARTY_NOTICES.json',
}


def inventory(root):
    root = Path(root)
    if not root.is_absolute() or root.is_symlink() or not root.is_dir():
        raise ValueError('absolute_regular_bundle_directory_required')
    files = []
    total = 0
    entries = 0
    for path in sorted(root.rglob('*')):
        entries += 1
        mode = path.lstat().st_mode
        if entries > 32768 or not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
            raise ValueError('bundle_entry_not_allowed')
        if path.is_dir() or path == root / 'components.json':
            continue
        name = path.relative_to(root).as_posix()
        if len(name) > 1024 or '\\' in name:
            raise ValueError('invalid_bundle_path')
        total += path.stat().st_size
        if len(files) >= 16384 or total > 512 * 1024 * 1024:
            raise ValueError('bundle_budget_exceeded')
        digest = hashlib.sha256()
        with path.open('rb') as stream:
            for block in iter(lambda: stream.read(65536), b''):
                digest.update(block)
        files.append({'path': name, 'sha256': digest.hexdigest()})
    if not REQUIRED.issubset({item['path'] for item in files}):
        raise ValueError('complete_runtime_mcp_and_licenses_required')
    for name in ('runtime', 'mcpClient'):
        if not (root / ENTRIES[name]).stat().st_mode & 0o111:
            raise ValueError('executable_component_required')
    return {'version': 1, **ENTRIES, 'files': files}


def seal(root):
    root = Path(root)
    manifest = inventory(root)
    encoded = (json.dumps(manifest, separators=(',', ':'), ensure_ascii=False) + '\n').encode()
    if len(encoded) > 2 * 1024 * 1024:
        raise ValueError('manifest_budget_exceeded')
    (root / 'components.json').write_bytes(encoded)
    return manifest


def verify(root):
    root = Path(root)
    manifest = root / 'components.json'
    if manifest.is_symlink() or not manifest.is_file() or manifest.stat().st_size > 2 * 1024 * 1024:
        raise ValueError('regular_manifest_required')
    saved = json.loads(manifest.read_bytes())
    if saved != inventory(root):
        raise ValueError('component_digest_mismatch')
    return saved


def prepare(source, destination):
    source, destination = Path(source), Path(destination)
    verify(source)
    if not destination.is_absolute() or destination.exists() or destination.is_symlink():
        raise ValueError('fresh_absolute_destination_required')
    shutil.copytree(source, destination, symlinks=False)
    return verify(destination)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('prepare', 'seal', 'verify'))
    parser.add_argument('root', type=Path)
    parser.add_argument('destination', type=Path, nargs='?')
    args = parser.parse_args()
    if args.action == 'prepare':
        if args.destination is None:
            parser.error('prepare requires a destination')
        result = prepare(args.root, args.destination)
    else:
        result = {'seal': seal, 'verify': verify}[args.action](args.root)
    print(json.dumps({'action': args.action, 'verified_files': len(result['files'])}))
