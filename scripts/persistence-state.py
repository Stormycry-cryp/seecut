#!/usr/bin/env python3
"""Finite, read-only verifier for this batch's two synthetic canvas packages.

No App API, state repair, path rewriting, database traversal or state copying.
Called by the launcher only after A has exited and immediately before B starts.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time

APP_SHA = 'b7ec5fc769d09e3685f212cd96484dfbb0858e9edb649d1314877a5f0a86f852'
HEAD = 'a6cf09ad935bbbc6cf792c4ac6a4b506a47aebd3'
FIXTURE_RGBA = '26eedbcf118b1e9f52dc410a543a05cb867467b51a046224f86f119ec8dd56d5'
MARKER = 'persistence-batch.json'
MAX_FILE = 2 * 1024 * 1024
MAX_STATE = 6 * 1024 * 1024


def real_directory(path, private=False):
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('absolute_owned_state_path_required')
    for ancestor in (*reversed(path.parents), path):
        info = ancestor.lstat()
        if not stat.S_ISDIR(info.st_mode):
            raise ValueError('state_ancestor_not_real_directory')
    info = path.lstat()
    if info.st_uid != os.getuid() or (private and stat.S_IMODE(info.st_mode) != 0o700):
        raise ValueError('owned_private_state_directory_required')
    return info


def read_owned(path, maximum=MAX_FILE):
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
            or info.st_uid != os.getuid() or info.st_size > maximum):
        raise ValueError('bounded_owned_regular_state_file_required')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        actual = os.fstat(stream.fileno())
        if (actual.st_dev, actual.st_ino, actual.st_size, actual.st_mtime_ns) != (
                info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns):
            raise ValueError('state_file_changed_before_read')
        data = stream.read(maximum + 1)
        after = os.fstat(stream.fileno())
    final = path.lstat()
    identity = lambda i: (i.st_dev, i.st_ino, i.st_size, i.st_mtime_ns)
    if len(data) != info.st_size or identity(after) != identity(info) or identity(final) != identity(info):
        raise ValueError('state_file_changed_during_read')
    return data


def validate_batch(root, token):
    if not re.fullmatch('[0-9a-f]{32}', token) or root.name != 'seecut-persistence-' + token:
        raise ValueError('this_batch_random_state_name_required')
    info = real_directory(root, private=True)
    marker = json.loads(read_owned(root / MARKER, 4096))
    expected = {'schema': 1, 'batch': token, 'uid': os.getuid(),
                'source_head': HEAD, 'app_sha256': APP_SHA,
                'root_device': info.st_dev, 'root_inode': info.st_ino}
    if marker != expected:
        raise ValueError('state_batch_marker_or_identity_changed')
    if set(p.name for p in root.iterdir()) != {MARKER, 'concat', 'portable'}:
        raise ValueError('unknown_state_root_entry')
    real_directory(root / 'portable', private=True)
    return marker


def portable_entry_diagnostic(root, token, deadline):
    """Shallow metadata only, from the validated owned batch portable root.

    At most 32 safe ASCII basenames; unsafe/overflow names are counts only.
    Never reads file contents, resolves a link, or visits any child directory.
    This is failure evidence and does not change the state acceptance policy.
    """
    validate_batch(root, token)
    required = {'settings.json', 'canvas-projects.json', 'canvas-projects'}
    present = set()
    records = []
    total = safe = 0
    with os.scandir(root / 'portable') as entries:
        for entry in entries:
            if time.monotonic() >= deadline:
                raise ValueError('portable_diagnostic_deadline')
            total += 1
            if entry.name in required:
                present.add(entry.name)
            if not 1 <= len(entry.name) <= 80 or not re.fullmatch('[A-Za-z0-9._-]+', entry.name):
                continue
            safe += 1
            if len(records) >= 32:
                continue
            try:
                info = entry.stat(follow_symlinks=False)
                kind = ('directory' if stat.S_ISDIR(info.st_mode) else
                        'regular_file' if stat.S_ISREG(info.st_mode) else
                        'symlink' if stat.S_ISLNK(info.st_mode) else 'other')
                size = info.st_size  # Directory lstat size, not recursive content size.
            except OSError:
                kind, size = 'stat_unavailable', None
            records.append({'name': entry.name, 'type': kind, 'lstat_size_bytes': size})
    return {'schema': 1, 'scope': 'owned_batch_portable_top_level',
            'entry_count': total, 'safe_name_count': safe,
            'omitted_name_count': total - len(records),
            'required_present': {name: name in present for name in sorted(required)},
            'entries': sorted(records, key=lambda item: item['name']),
            'contents_read': False, 'directories_traversed': False}


def png_record(path, expected_dimensions, deadline):
    """Decode only validated owned synthetic PNGs; no raw pixels enter output."""
    data = read_owned(path)
    if data[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError('state_bitmap_not_PNG')
    def command(argv):
        left = deadline - time.monotonic()
        if left <= 0:
            raise ValueError('state_inspection_deadline')
        return subprocess.run(argv, check=True, capture_output=True,
                              timeout=min(5, left)).stdout
    dimensions = command(['identify', '-format', '%w %h', str(path)]).decode().split()
    if dimensions != [str(n) for n in expected_dimensions]:
        raise ValueError('state_bitmap_dimension_mismatch')
    rgba = command(['convert', str(path), '-alpha', 'on', '-depth', '8', 'rgba:-'])
    if len(rgba) != expected_dimensions[0] * expected_dimensions[1] * 4:
        raise ValueError('state_bitmap_RGBA_size_mismatch')
    if read_owned(path) != data:
        raise ValueError('state_bitmap_changed_during_decode')
    return {'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
            'dimensions': list(expected_dimensions), 'rgba_sha256': hashlib.sha256(rgba).hexdigest()}


def pixels_used(document):
    """Exactly root + one ordinary image layer; this test creates nothing else."""
    if set(document) != {'width', 'height', 'root', 'next_id'}:
        raise ValueError('unexpected_synthetic_document_shape')
    root = document['root']
    if (not isinstance(root, dict) or root.get('mask') is not None
            or root.get('hidden') is not False or root.get('opacity') != 1
            or root.get('name') != 'Root' or root.get('blend') != 'Normal'
            or len(root.get('children', [])) != 1):
        raise ValueError('unexpected_synthetic_root')
    node = root['children'][0]
    if set(node) != {'Layer'}:
        raise ValueError('unexpected_synthetic_layer_kind')
    layer = node['Layer']
    if (layer.get('mask') is not None or layer.get('clips_to') is not None
            or layer.get('hidden') is not False or layer.get('opacity') != 1
            or layer.get('blend') != 'Normal' or layer.get('sampling') != 'Smooth'
            or layer.get('transform') != {'x': 0.0, 'y': 0.0, 'scale_x': 1.0, 'scale_y': 1.0,
                                         'rotation': 0.0, 'flip_h': False, 'flip_v': False}):
        raise ValueError('synthetic_layer_not_saved_after_Undo')
    if any(type(n) is not int or n <= 0 for n in (root.get('id'), layer.get('id'), layer.get('pixels'), document.get('next_id'))):
        raise ValueError('synthetic_ids_invalid')
    if root['id'] == layer['id'] or document['next_id'] <= max(root['id'], layer['id']):
        raise ValueError('synthetic_layer_ids_collide')
    return [layer['pixels']]


def snapshot_state(root, token, deadline):
    """Two complete read passes are compared by caller across the launch gap."""
    validate_batch(root, token)
    portable = root / 'portable'
    entries = set(p.name for p in portable.iterdir())
    required = {'settings.json', 'canvas-projects.json', 'canvas-projects'}
    startup_directories = {'logs', 'whisper-models', 'tts-models'}
    if not required <= entries or not entries <= required | {'seecut.json'} | startup_directories:
        raise ValueError('unexpected_portable_state_entry')
    # Desktop logging and Settings Restore create these observed directories.
    # Their contents are unrelated to the saved canvas and never read or uploaded.
    for name in startup_directories & entries:
        real_directory(portable / name)
    files = []
    def file_record(path, maximum=MAX_FILE):
        raw = read_owned(path, maximum)
        record = {'relative_path': str(path.relative_to(root)), 'bytes': len(raw),
                  'sha256': hashlib.sha256(raw).hexdigest()}
        files.append(record)
        return raw
    prefs = json.loads(file_record(portable / 'settings.json', 16384))
    if prefs != {'locale': 'en', 'dark': False, 'server': {'enabled': False}}:
        raise ValueError('startup_preferences_changed')
    if 'seecut.json' in entries:
        file_record(portable / 'seecut.json', 32768)  # Hash only; never emit configuration values.
    registry = json.loads(file_record(portable / 'canvas-projects.json', 4096))
    if (not isinstance(registry, list) or len(registry) != 2
            or any(not isinstance(p, str) for p in registry) or len(set(registry)) != 2):
        raise ValueError('exact_two_synthetic_projects_required')
    folder = portable / 'canvas-projects'
    real_directory(folder)
    names = []
    for raw in registry:
        path = Path(raw)
        if (path.parent != folder or not re.fullmatch(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.comp', path.name)):
            raise ValueError('registry_path_outside_this_batch')
        names.append(path.name)
    folder_names = set(p.name for p in folder.iterdir())
    if folder_names != set(names) | {'.seecut-canvas-locks'}:
        raise ValueError('unknown_canvas_project_entry_or_incomplete_save')
    locks = folder / '.seecut-canvas-locks'
    real_directory(locks, private=True)
    if set(p.name for p in locks.iterdir()) != set(names):
        raise ValueError('unknown_canvas_lock_entry')
    for name in names:
        if file_record(locks / name, 0) != b'':
            raise ValueError('unexpected_canvas_lock_content')
    packages = []
    for name in names:
        path = folder / name
        real_directory(path)
        if set(p.name for p in path.iterdir()) != {'manifest.json', 'preview.png', 'images'}:
            raise ValueError('unexpected_canvas_package_file')
        raw = file_record(path / 'manifest.json', 32768)
        manifest = json.loads(raw)
        if set(manifest) != {'concat-project', 'name', 'width', 'height', 'document'} or manifest['concat-project'] != 1:
            raise ValueError('unexpected_canvas_manifest')
        dimensions = (manifest['width'], manifest['height'])
        if manifest['name'] == 'opaque-quadrants.png' and dimensions == (256, 192):
            kind = 'fixture'
        elif manifest['name'] in ('未命名画布', 'Untitled Canvas') and dimensions == (1920, 1080):
            kind = 'blank'
        else:
            raise ValueError('non_synthetic_canvas_package')
        document = manifest['document']
        if (document['width'], document['height']) != dimensions:
            raise ValueError('manifest_document_dimensions_differ')
        ids = pixels_used(document)
        images = path / 'images'
        real_directory(images)
        if set(p.name for p in images.iterdir()) != {str(i) + '.png' for i in ids}:
            raise ValueError('bitmap_references_not_exactly_satisfied')
        bitmap_records = []
        for pixel_id in ids:
            bitmap = images / (str(pixel_id) + '.png')
            png = png_record(bitmap, dimensions, deadline)
            file_record(bitmap)
            expected = FIXTURE_RGBA if kind == 'fixture' else hashlib.sha256(bytes(1920 * 1080 * 4)).hexdigest()
            if png['rgba_sha256'] != expected:
                raise ValueError('saved_bitmap_not_exact_synthetic_fixture')
            bitmap_records.append(dict(png, pixel_id=pixel_id))
        # image::imageops::thumbnail is called with exact 480x320, including upscaling.
        preview_dimensions = (480, 320)
        preview = png_record(path / 'preview.png', preview_dimensions, deadline)
        file_record(path / 'preview.png')
        if kind == 'blank' and preview['rgba_sha256'] != hashlib.sha256(bytes(480 * 320 * 4)).hexdigest():
            raise ValueError('saved_preview_not_exact_synthetic_fixture')
        packages.append({'kind': kind, 'relative_path': str(path.relative_to(root)),
                         'dimensions': list(dimensions), 'referenced_pixel_ids': ids,
                         'document_sha256': hashlib.sha256(json.dumps(document, sort_keys=True, separators=(',', ':')).encode()).hexdigest(),
                         'bitmaps': bitmap_records, 'preview': preview})
    if sorted(p['kind'] for p in packages) != ['blank', 'fixture'] or sum(f['bytes'] for f in files) > MAX_STATE:
        raise ValueError('finite_synthetic_state_contract_failed')
    # Registry order is retained through its raw digest; JSON values stay on disk.
    return {'schema': 1, 'batch': token, 'root': str(root),
            'files': sorted(files, key=lambda f: f['relative_path']),
            'packages': sorted(packages, key=lambda p: p['kind'])}


def assert_process_gone(pid):
    if type(pid) is not int or pid < 2 or (Path('/proc') / str(pid)).exists():
        raise ValueError('previous_App_PID_not_proven_gone')
    try:
        os.killpg(pid, 0)
    except ProcessLookupError:
        return True
    raise ValueError('previous_App_process_group_not_proven_gone')


def validate_public_capture(data, app_pid):
    """Complete public App metadata must exclude Settings before saved pixels."""
    nodes = data.get('nodes')
    if (data.get('status') != 'public_metadata_observed' or data.get('coverage_complete') is not True
            or data.get('field_values_read') is not False or data.get('app_pid') != app_pid
            or not isinstance(nodes, list) or not 1 <= len(nodes) <= 512
            or any(not isinstance(n, dict) or not isinstance(n.get('path'), list)
                   or any(type(i) is not int or i < 0 for i in n['path']) for n in nodes)):
        raise ValueError('complete_public_App_metadata_required_before_pixels')
    if len({tuple(n['path']) for n in nodes}) != len(nodes):
        raise ValueError('duplicate_public_App_path_before_pixels')
    visible = [n for n in nodes if n.get('showing')]
    settings_bounds = ({'x': 596, 'y': 147, 'width': 188, 'height': 36},
                       {'x': 596, 'y': 222, 'width': 188, 'height': 36},
                       {'x': 144, 'y': 68, 'width': 48, 'height': 24},
                       {'x': 1216, 'y': 68, 'width': 26, 'height': 128})
    setting_labels = ('工作模式', '外观', '主题', '通用', '常规', 'General', 'Appearance',
                      'Theme', 'Work mode', '本地 Agent 权限', 'Local Agent Permissions')
    if any(n.get('label') in setting_labels
           or (isinstance(n.get('label'), str) and any(n['label'].startswith(label + separator)
               for label in ('工作模式', '外观', 'Work mode', 'Appearance')
               for separator in ('，', ', ', ': ', '：')))
           or n.get('bounds') in settings_bounds for n in visible):
        raise ValueError('Settings_context_forbids_saved_pixels')
    windows = [n for n in visible if n.get('role') == 23]
    if (len(windows) != 1 or windows[0].get('path') != [0]
            or windows[0].get('bounds') != {'x': 0, 'y': 0, 'width': 1280, 'height': 900}
            or not all(windows[0].get(k) is True for k in ('enabled', 'sensitive'))):
        raise ValueError('public_App_window_changed_before_pixels')
    if any(n.get('dialog') or n.get('modal') or n.get('file_chooser') for n in visible):
        raise ValueError('unknown_App_modal_forbids_saved_pixels')
    return True
