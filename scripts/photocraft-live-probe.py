#!/usr/bin/env python3
"""Candidate: explicitly disposable complete-editor smoke run on private Linux Xvfb.

The controller owns a new OS session. Its guardian remains alive until cleanup,
so editor children with separate process groups remain attributable to this run.
No existing DISPLAY, user profile, outside PID or raw editor stderr is consumed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import stat
import struct
import subprocess
import sys
import time
import zlib


def require(value, message):
    if not value:
        raise ValueError(message)


def digest(path):
    require(path.is_file() and not path.is_symlink(), 'expected regular non-symlink file')
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(65536), b''):
            result.update(block)
    return result.hexdigest()


def private_new(path):
    path.mkdir(mode=0o700)
    require(stat.S_IMODE(path.stat().st_mode) == 0o700, 'new directory is not private')


def validate_generated_png(path, expected_alpha):
    """Bounded RGB/RGBA 8-bit PNG decoding with all five PNG row filters, stdlib only."""
    require(path.is_file() and not path.is_symlink(), 'PNG must be a regular generated file')
    require(0 < path.stat().st_size <= 1 << 20, 'PNG file budget exceeded')
    data = path.read_bytes()
    require(data.startswith(b'\x89PNG\r\n\x1a\n'), 'not a PNG')
    offset, compressed, dimensions = 8, bytearray(), None
    ended = False
    while offset < len(data):
        require(offset + 12 <= len(data), 'truncated PNG chunk')
        length = struct.unpack('>I', data[offset:offset + 4])[0]
        kind = data[offset + 4:offset + 8]
        require(length <= 1 << 20 and offset + length + 12 <= len(data), 'PNG chunk budget')
        payload = data[offset + 8:offset + 8 + length]
        crc = struct.unpack('>I', data[offset + 8 + length:offset + 12 + length])[0]
        require(zlib.crc32(kind + payload) & 0xffffffff == crc, 'PNG CRC mismatch')
        if kind == b'IHDR':
            require(dimensions is None and length == 13, 'bad PNG IHDR')
            width, height, depth, color, compression, filtering, interlace = struct.unpack('>IIBBBBB', payload)
            require((width, height, depth) == (16, 16, 8) and color in (2, 6), 'unexpected PNG shape')
            require((compression, filtering, interlace) == (0, 0, 0), 'unsupported PNG coding')
            dimensions = width, height, 3 if color == 2 else 4
        elif kind == b'IDAT':
            require(dimensions is not None, 'PNG data precedes header')
            compressed.extend(payload)
            require(len(compressed) <= 1 << 20, 'PNG compressed budget')
        elif kind == b'IEND':
            require(length == 0, 'bad PNG end chunk')
            ended = True
            offset += length + 12
            break
        elif kind == b'tRNS':
            raise ValueError('PNG transparency key is outside this RGB/RGBA smoke decoder')
        offset += length + 12
    require(ended and offset == len(data) and dimensions is not None, 'incomplete PNG')
    width, height, channels = dimensions
    limit = (width * channels + 1) * height
    decoder = zlib.decompressobj()
    raw = decoder.decompress(compressed, limit + 1)
    require(len(raw) == limit and decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail, 'PNG inflate budget or framing mismatch')
    previous = bytearray(width * channels)
    pixels, cursor = [], 0
    for _ in range(height):
        mode = raw[cursor]
        require(mode <= 4, 'unsupported PNG filter')
        row = bytearray(raw[cursor + 1:cursor + 1 + width * channels])
        for i in range(len(row)):
            left = row[i - channels] if i >= channels else 0
            up = previous[i]
            upper_left = previous[i - channels] if i >= channels else 0
            predictor = 0
            if mode == 1:
                predictor = left
            elif mode == 2:
                predictor = up
            elif mode == 3:
                predictor = (left + up) // 2
            elif mode == 4:
                p = left + up - upper_left
                distances = abs(p - left), abs(p - up), abs(p - upper_left)
                predictor = (left, up, upper_left)[distances.index(min(distances))]
            row[i] = (row[i] + predictor) & 255
        for i in range(0, len(row), channels):
            rgba = list(row[i:i + channels]) + ([255] if channels == 3 else [])
            require(all(abs(a - b) <= 2 for a, b in zip(rgba, [255, 128, 0, expected_alpha])), 'generated orange pixel readback mismatch')
            pixels.append(rgba)
        previous = row
        cursor += width * channels + 1
    return {'file': path.name, 'sha256': digest(path), 'bytes': len(data), 'size': [width, height],
            'pixelsChecked': len(pixels), 'expectedRgba': [255, 128, 0, expected_alpha], 'tolerance': 2}


def session_groups(session_id, deadline):
    require(time.monotonic() < deadline, 'owned-session cleanup deadline exceeded')
    result = subprocess.run(['ps', '-eo', 'sid=,pgid=,stat='], check=True, capture_output=True, text=True,
                            timeout=min(1, max(0.01, deadline - time.monotonic())))
    return {int(fields[1]) for row in result.stdout.splitlines()
            if len(fields := row.split()) == 3 and fields[0] == str(session_id) and not fields[2].startswith('Z')}


def cleanup_session(guardian):
    # guardian is the actual start_new_session Child retained by this controller.
    # Never accept a SID/PID from user input, an editor log or a stale PID file.
    absolute_deadline = time.monotonic() + 8
    for sig, grace in ((signal.SIGTERM, 2.0), (signal.SIGKILL, 2.0)):
        deadline = time.monotonic() + grace
        while True:
            groups = session_groups(guardian.pid, absolute_deadline)
            if not groups:
                guardian.wait(timeout=1)
                return
            for group in groups:
                # Recheck session membership immediately before acting on this owned group.
                if group not in session_groups(guardian.pid, absolute_deadline):
                    continue
                try:
                    os.killpg(group, sig)
                except ProcessLookupError:
                    pass
            if time.monotonic() >= deadline:
                break
            time.sleep(0.04)
    require(not session_groups(guardian.pid, absolute_deadline), 'owned display/probe session cleanup incomplete')
    guardian.wait(timeout=1)


def session_guardian(arguments):
    # Called only as this controller's direct child in its freshly created OS session.
    command = ['xvfb-run', '--auto-servernum', '--server-args=-screen 0 1440x1000x24 -nolisten tcp', *arguments]
    child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    code = child.wait()
    print(json.dumps({'exitCode': code}), flush=True)
    # Keep the SID leader alive until the parent cleans all process groups, including
    # NativeHost's separate groups. Do not orphan this session by returning early.
    sys.stdin.buffer.read(1)
    return 0


def run(args):
    require(sys.platform == 'linux' and os.name == 'posix', 'isolated Linux runner required')
    require(args.allow_disposable_editor, 'explicit disposable synthetic-editor opt-in required')
    require(30 <= args.timeout <= 300, 'absolute timeout outside 30..300 seconds')
    deadline = time.monotonic() + args.timeout
    workspace = args.workspace.resolve(strict=True)
    managed = workspace / '.toonflow'
    for path in (args.provenance, args.probe_binary, args.output):
        require(path.is_absolute() and '..' not in path.parts and path.is_relative_to(managed), 'paths must be inside this workspace .toonflow')
        require(not any(parent.is_symlink() for parent in (path, *path.parents)), 'managed path contains a symlink')
    require(args.output.parent.is_dir(), 'output parent must already exist')
    private_new(args.output)
    artifact = args.output / 'artifacts'
    private_new(artifact)
    for name in ('xdg-runtime', 'xdg-config', 'xdg-data', 'xdg-cache', 'cli-profile'):
        private_new(args.output / name)
    receipt = json.loads(args.provenance.read_text())
    require(receipt['status'] == 'built', 'successful complete build provenance required')
    for name in ('photocraft', 'photocraft-cli'):
        entry = receipt['binaries'][name]
        binary = Path(entry['path'])
        require(binary.is_absolute() and binary.is_relative_to(managed), 'build output outside this managed workspace')
        require(not any(parent.is_symlink() for parent in (binary, *binary.parents)), 'build binary path contains a symlink')
        require(digest(binary) == entry['sha256'] and binary.stat().st_size == entry['bytes'], 'actual build binary identity mismatch')
    probe_hash = digest(args.probe_binary)
    environment = os.environ.copy()
    for key in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY', 'PHOTOCRAFT_CONTROL_TOKEN', 'PHOTOCRAFT_CONTROL_TOKEN_FILE',
                'PHOTOCRAFT_CONTROL_PORT', 'SEECUT_PHOTO_HOST_INSTANCE_ID', 'PHOTOCRAFT_AUTOMATION_READ_ROOT',
                'PHOTOCRAFT_AUTOMATION_WRITE_ROOT', 'PHOTOCRAFT_GPU_ADAPTER', 'CRAFT_FONTS_DIR', 'CRAFT_FONTS_REQUIRED'):
        environment.pop(key, None)
    icds = sorted(Path('/usr/share/vulkan/icd.d').glob('lvp_icd*.json'))
    require(icds, 'Mesa software Vulkan ICD missing')
    environment.update({'SEECUT_LIVE_PROBE_ISOLATED': '1', 'VK_ICD_FILENAMES': str(icds[0]), 'VK_DRIVER_FILES': str(icds[0]),
        'WGPU_BACKEND': 'vulkan', 'LIBGL_ALWAYS_SOFTWARE': '1', 'XDG_SESSION_TYPE': 'x11',
        'XDG_RUNTIME_DIR': str(args.output / 'xdg-runtime'), 'XDG_CONFIG_HOME': str(args.output / 'xdg-config'),
        'XDG_DATA_HOME': str(args.output / 'xdg-data'), 'XDG_CACHE_HOME': str(args.output / 'xdg-cache'),
        'PHOTOCRAFT_CONFIG_DIR': str(args.output / 'cli-profile'), 'PHOTOCRAFT_NO_PRINT': '1',
        'LPDEST': 'photocraft-no-such-printer', 'PULSE_SERVER': 'unix:' + str(args.output / 'no-audio.socket')})
    commands = args.output / 'commands.json'
    with commands.open('xb') as output:
        subprocess.run([receipt['binaries']['photocraft-cli']['path'], 'commands', '--json'], env=environment,
                       stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.DEVNULL, check=True,
                       timeout=max(0.01, min(15, deadline - time.monotonic())))
    require(commands.stat().st_size <= 8 << 20, 'compiled command dump budget exceeded')
    require(isinstance(json.loads(commands.read_text()), list), 'invalid compiled command dump')
    probe_root = args.output / 'synthetic-editor-run'  # Must not exist; Rust creates it exclusively.
    command = [sys.executable, str(Path(__file__).resolve()), '--internal-session', str(args.probe_binary),
               str(args.provenance), str(probe_root), str(commands)]
    guardian = subprocess.Popen(command, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, start_new_session=True, text=True)
    exit_code, controller_error = None, None
    try:
        selector = selectors.DefaultSelector()
        selector.register(guardian.stdout, selectors.EVENT_READ)
        require(selector.select(max(0, deadline - time.monotonic())), 'absolute live-probe deadline exceeded')
        line = guardian.stdout.readline(1025)
        require(len(line) <= 1024, 'guardian event budget exceeded')
        exit_code = json.loads(line)['exitCode']
    except BaseException as error:
        controller_error = type(error).__name__  # Never serialize arbitrary errors/paths/environment.
    finally:
        # Finish this bounded owned-session cleanup even if the job sends a second cancel signal.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        cleanup_ok = True
        try:
            cleanup_session(guardian)
        except BaseException as error:
            cleanup_ok = False
            controller_error = 'cleanup' + type(error).__name__
    report_path = probe_root / 'report.json'
    report = json.loads(report_path.read_text()) if report_path.is_file() and not report_path.is_symlink() else {'status': 'missingProbeReport'}
    require(not report.get('model', False) and not report.get('parentSeeCutMcp', False), 'probe scope mismatch')
    report.update({'model': False, 'parentSeeCutMcp': False, 'probeExecutableSha256': probe_hash,
                   'controller': {'exitCode': exit_code, 'failureClass': controller_error, 'ownedSessionCleaned': cleanup_ok,
                                  'privateXvfb': True, 'softwareVulkanOnlyConfigured': True},
                   'compiledCommandsSha256': digest(commands)})
    if exit_code == 0 and report['status'] == 'passed' and controller_error is None:
        try:
            images = []
            for filename, alpha in [('orange-full.png', 255), ('orange-opacity-040.png', 102), ('orange-after-undo.png', 255)]:
                path = probe_root / 'write' / filename
                images.append(validate_generated_png(path, alpha))
                (artifact / filename).write_bytes(path.read_bytes())
            report['pixelEvidence'] = {'status': 'actualPngReadbackPassed', 'images': images,
                                       'uiScreenshotOrAtomicSnapshot': False}
        except BaseException as error:
            report['status'] = 'failed'
            report['pixelEvidence'] = {'status': 'pngReadbackFailed', 'failureClass': type(error).__name__}
    else:
        report['status'] = 'failed'
    (artifact / 'live-probe-report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    require(report['status'] == 'passed', 'complete-editor smoke probe did not pass')


def main():
    require(sys.platform == 'linux', 'Linux only; never use the local macOS foreground')
    if len(sys.argv) > 1 and sys.argv[1] == '--internal-session':
        require(os.environ.get('SEECUT_LIVE_PROBE_ISOLATED') == '1' and len(sys.argv) == 6, 'private guardian invocation required')
        return session_guardian(sys.argv[2:])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--workspace', type=Path, required=True)
    parser.add_argument('--provenance', type=Path, required=True)
    parser.add_argument('--probe-binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--timeout', type=int, default=240)
    parser.add_argument('--allow-disposable-editor', action='store_true')
    args = parser.parse_args()
    def interrupted(signum, _frame):
        raise InterruptedError(f'signal {signum}')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        run(args)
        return 0
    except BaseException as error:
        print('isolated live probe failed: ' + type(error).__name__, file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
