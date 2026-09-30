#!/usr/bin/env python3
"""Capture candidate observations of the normal Linux App, without granting access.

Requires Xvfb (1920x1200x24), xdotool, ImageMagick import/convert, and CJK fonts.
The caller builds concat and supplies LD_LIBRARY_PATH for its dynamic libraries.
No preview mode, editor API, callback, or Slint property injection is used.

Source coordinate basis (all client pixels, SLINT_SCALE_FACTOR=1):
  app.slint: title Seecut; minimum 900x560; Linux title strip 32 high.
  seecut/components.slint AppRail: 80 wide; bottom padding 14; account 44;
    spacing 14; settings 44 => settings centre (40, window_height - 94).
  dialogs/settings.slint: page x=80; header=128; preference column x=144,
    width=640 at both requested widths; rows=74 + 1 divider; theme y=240,
    options width=94 => light x=643, dark x=737. Advanced button y~=385.
    advanced rail x=80..244; padding 8; PageTab=30; spacing=2;
    General centre y=183, Remote centre y=279.
  modal.slint DialogSection: header 30; body top padding 4; Select=28;
    General language dropdown centre y=176. select.slint supports Home,
    Down and Return. i18n.rs built-in en index=0, zh-Hans index=11.

Page selection is checked using the real captured PageTab highlight pixel.
Language/theme are read back from portable settings.json after UI changes.
These checks do not establish permission text, layout quality or acceptance:
every Remote image remains a candidate requiring visual review. No canvas is
opened; the owned 16px PNG is prepared only, not imported in this first stage.
"""

import argparse
import collections
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib


MAX_OUTPUT = 15 * 1024 * 1024
LOG_LIMIT = 256 * 1024
RGB_TOLERANCE = 2  # 8-bit rendering quantization only; not a layout tolerance.


def png16():
    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
    rows = b"".join(b"\0" + bytes((32, 96, 192)) * 16 for _ in range(16))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 16, 16, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def redact(text):
    # Drop complete token/authorization/credential lines, including fragmented
    # stderr chunks once assembled. UUID instance identifiers are not secrets.
    return "\n".join("[sensitive line omitted]" if re.search(
        r"token|authorization|credential|password|secret|bearer", line, re.I)
        else line for line in text.splitlines())


def rgb_matches(observed, expected):
    return all(abs(int(observed[i:i + 2], 16) - int(expected[i:i + 2], 16))
               <= RGB_TOLERANCE for i in (0, 2, 4))


def self_test():
    blob = png16()
    offset, packed = 8, b""
    while offset < len(blob):
        size = struct.unpack(">I", blob[offset:offset + 4])[0]
        kind, data = blob[offset + 4:offset + 8], blob[offset + 8:offset + 8 + size]
        crc = struct.unpack(">I", blob[offset + 8 + size:offset + 12 + size])[0]
        assert crc == zlib.crc32(kind + data) & 0xFFFFFFFF
        if kind == b"IDAT":
            packed += data
        offset += size + 12
    assert len(zlib.decompress(packed)) == 16 * (1 + 16 * 3)
    assert "abc" not in redact("client token=abc\nnormal line")
    assert rgb_matches("808080", "7e8280")
    assert not rgb_matches("808080", "7d8080")
    assert not rgb_matches("808080", "808380")
    assert not rgb_matches("808080", "80807d")
    print("stdlib PNG, log redaction and RGB tolerance boundary self-test passed")


class Probe:
    def __init__(self, args):
        self.args = args
        self.output = Path(args.output).absolute()
        self.owns_output = False
        self.process = None
        self.work = None
        self.window = None
        self.deadline = time.monotonic() + 280
        self.events = []
        self.log_chunks = collections.deque()
        self.log_bytes = 0
        self.log_lock = threading.Lock()
        self.locale, self.dark = "en", False
        self.tab = None
        self.reader = None
        self.last_observation = None

    def event(self, action, **fields):
        self.events.append(dict(elapsed_s=round(280 - (self.deadline - time.monotonic()), 3),
                                action=action, **fields))

    def remaining(self):
        left = self.deadline - time.monotonic()
        if left <= 0:
            raise RuntimeError("280 second work deadline reached")
        return left

    def command(self, *argv, timeout=8):
        result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=min(timeout, self.remaining()), check=False)
        if result.returncode:
            raise RuntimeError(redact(f"{argv[0]} exit {result.returncode}: "
                                     + result.stderr.decode(errors="replace")[-3000:]))
        if len(result.stdout) > LOG_LIMIT:
            raise RuntimeError(f"{argv[0]} unexpectedly large command output")
        return result.stdout.decode(errors="replace").strip()

    def alive(self):
        self.remaining()
        if self.process.poll() is not None:
            raise RuntimeError(f"App exited with code {self.process.returncode}")

    def settle(self, seconds=0.35):
        self.alive()
        time.sleep(min(seconds, self.remaining()))
        self.alive()

    def windows(self):
        # xdotool search returns 1 when no window matches; this alone is a
        # retryable startup observation. Other errors remain fatal.
        result = subprocess.run(
            ["xdotool", "search", "--all", "--onlyvisible", "--pid",
             str(self.process.pid), "--name", "^Seecut$"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            timeout=min(5, self.remaining()), check=False)
        if result.returncode not in (0, 1) or result.stderr:
            raise RuntimeError("window search: " + redact(result.stderr.decode(errors="replace")))
        return result.stdout.decode().split()

    def check_window(self):
        self.alive()
        matches = self.windows()
        if matches != [self.window]:
            raise RuntimeError(f"Expected one owned Seecut window; found {matches}")
        pid = self.command("xdotool", "getwindowpid", self.window)
        if pid != str(self.process.pid):
            raise RuntimeError("Window PID mismatch")

    def geometry(self):
        self.check_window()
        values = self.command("xdotool", "getwindowgeometry", "--shell", self.window)
        return {key.lower(): int(value) for key, value in
                (line.split("=", 1) for line in values.splitlines())
                if key in ("X", "Y", "WIDTH", "HEIGHT", "SCREEN")}

    def click(self, x, y, name):
        self.check_window()
        self.command("xdotool", "windowfocus", "--sync", self.window)
        self.command("xdotool", "mousemove", "--sync", "--window", self.window,
                     str(x), str(y))
        self.command("xdotool", "click", "1")
        self.event("x11_click", target=name, x=x, y=y, window=self.window)
        self.settle()

    def key(self, *keys):
        self.check_window()
        # Language popup owns focus; sending to the main X window explicitly
        # would bypass its real popup. Check the focused window's owning PID.
        focused = self.command("xdotool", "getwindowfocus")
        if self.command("xdotool", "getwindowpid", focused) != str(self.process.pid):
            raise RuntimeError("Keyboard focus is not owned by QA App")
        self.command("xdotool", "key", "--clearmodifiers", *keys)
        self.event("x11_key", keys=list(keys), focused_window=focused)
        self.settle()

    def capture(self, filename=None):
        geom = self.geometry()
        target = self.output / filename if filename else self.work / "pending-probe.png"
        self.command("import", "-window", self.window, "-depth", "8",
                     "-define", "png:compression-level=9", "PNG24:" + str(target), timeout=12)
        self.check_window()
        header = target.read_bytes()[:24]
        if header[:8] != b"\x89PNG\r\n\x1a\n":
            raise RuntimeError("Capture did not produce a PNG")
        size = struct.unpack(">II", header[16:24])
        if size != (geom["width"], geom["height"]):
            raise RuntimeError(f"PNG {size} differs from real window geometry {geom}")
        if filename:
            total = sum(p.stat().st_size for p in self.output.iterdir())
            if total > MAX_OUTPUT - 1024 * 1024:
                target.unlink()
                raise RuntimeError("PNG output reached reserved 14 MiB limit")
            self.event("screenshot", file=filename, geometry=geom,
                       locale=self.locale, theme="dark" if self.dark else "light",
                       locale_theme_source="portable configuration + UI prefs readback",
                       page_index=self.tab,
                       page_evidence="selected tab pixel or preference selector verified; content visual review pending",
                       permissions="fresh process; no grant actions; text visual review pending",
                       sha256=hashlib.sha256(target.read_bytes()).hexdigest(),
                       bytes=target.stat().st_size)
        else:
            # Keep the last complete observation if a later import fails.
            target = target.replace(self.work / "probe.png")
            self.last_observation = dict(geometry=geom, locale=self.locale,
                                         theme="dark" if self.dark else "light",
                                         captured_elapsed_s=round(
                                             280 - (self.deadline - time.monotonic()), 3))
        return target

    def pixel(self, image, x, y):
        text = self.command("convert", str(image), "-crop", f"1x1+{x}+{y}",
                            "-depth", "8", "txt:-")
        match = re.search(r"#([0-9a-fA-F]{6})", text)
        if not match:
            raise RuntimeError("Could not read capture pixel")
        return match.group(1).lower()

    def require_rgb(self, action, observed, expected, **fields):
        matches = rgb_matches(observed, expected)
        self.event(action, observed_rgb=observed, expected_rgb=expected,
                   per_channel_tolerance=RGB_TOLERANCE, matches=matches, **fields)
        if not matches:
            raise RuntimeError(f"{action}: observed {observed}; expected {expected}; "
                               f"maximum per-channel deviation {RGB_TOLERANCE}; "
                               "navigation failed or coordinate assumptions changed")

    def verify_tab(self, index):
        shot = self.capture()
        expected = "3e3e43" if self.dark else "e2e5eb"
        # A point within the left margin, away from rounded corners/icons.
        value = self.pixel(shot, 92, 128 + 8 + 32 * index + 15)
        self.require_rgb("selected_tab_pixel", value, expected, index=index,
                         interpretation="selected navigation tab; page content needs visual review")
        self.tab = index

    def read_prefs(self):
        prefs = json.loads((self.work / "portable/settings.json").read_text())
        return prefs.get("locale", "en"), prefs.get("dark", False)

    def verify_prefs(self):
        actual = self.read_prefs()
        if actual != (self.locale, self.dark):
            raise RuntimeError(f"UI preference readback differs: {actual}")
        self.event("prefs_readback", locale=actual[0], dark=actual[1],
                   source="owned portable/settings.json")

    def preferences(self):
        height = self.geometry()["height"]
        self.click(40, height - 94, "AppRail Settings")
        # Header area uses Theme.panel, distinct from the default generate page.
        shot = self.capture()
        self.require_rgb("settings_header_pixel", self.pixel(shot, 100, 100),
                         "232326" if self.dark else "f8f9fb")
        # A header alone also exists on Remote. Require the actual preference
        # radio thumb before any clicks at row coordinates; otherwise a missed
        # rail click could accidentally target controls on an advanced page.
        thumb_x = 698 if self.dark else 604
        thumb_rgb = "33221f" if self.dark else "e8eef8"
        self.require_rgb("preferences_selector_pixel", self.pixel(shot, thumb_x, 232), thumb_rgb)
        self.tab = 0

    def advanced(self):
        self.click(735, 385, "Preferences Advanced settings")
        self.verify_tab(1)

    def resize(self, width, height):
        self.check_window()
        self.command("xdotool", "windowsize", "--sync", self.window, str(width), str(height))
        self.settle()
        actual = self.geometry()
        if (actual["width"], actual["height"]) != (width, height):
            raise RuntimeError(f"Requested {width}x{height}, received {actual}")
        self.event("resize", geometry=actual)

    def collect_log(self):
        while True:
            data = self.process.stdout.read(4096)
            if not data:
                return
            with self.log_lock:
                self.log_chunks.append(data)
                self.log_bytes += len(data)
                while self.log_bytes > LOG_LIMIT:
                    self.log_bytes -= len(self.log_chunks.popleft())

    def run(self):
        if platform.system() != "Linux":
            raise RuntimeError("Window probe runs only on remote Linux; use --self-test locally")
        if not re.fullmatch(r"[0-9a-f]{40}", self.args.expected_sha):
            raise RuntimeError("--expected-sha must be a full lowercase 40 hex commit")
        root = Path(__file__).resolve().parents[1]
        actual = self.command("git", "-C", str(root), "rev-parse", "HEAD")
        if actual != self.args.expected_sha:
            raise RuntimeError(f"Checkout HEAD {actual} differs from expected {self.args.expected_sha}")
        for tool in ("xdotool", "import", "convert"):
            if not shutil.which(tool):
                raise RuntimeError(f"Required tool unavailable: {tool}")
        if not os.environ.get("DISPLAY"):
            raise RuntimeError("DISPLAY is required")
        if "SEECUT_UI_PREVIEW_DIR" in os.environ:
            raise RuntimeError("SEECUT_UI_PREVIEW_DIR must be absent")
        if self.output.is_symlink() or self.output.exists():
            raise RuntimeError("--output must name a fresh, absent directory")
        self.output.mkdir(parents=True)
        self.owns_output = True
        self.work = Path(tempfile.mkdtemp(prefix="seecut-window-", dir=self.output.parent)).resolve()
        (self.work / "portable").mkdir()
        # current_exe resolves symlinks on Linux. Copying makes portable truly
        # adjacent to the executed binary, without writing under target/debug.
        binary = Path(self.args.binary).resolve(strict=True)
        copied = self.work / "concat"
        shutil.copy2(binary, copied)
        copied.chmod(copied.stat().st_mode | 0o700)
        (self.work / "owned-fixture-16.png").write_bytes(png16())
        (self.work / "portable/settings.json").write_text(json.dumps(
            {"locale": "en", "dark": False, "server": {"enabled": False, "token": ""}}))
        env = os.environ.copy()
        env["SLINT_SCALE_FACTOR"] = "1"
        env["TMPDIR"] = str(self.work)
        # Preserve HOME. Any fallback state/cache is confined to owned paths.
        for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"):
            env[key] = str(self.work / key.lower())
        env.pop("SEECUT_MCP_CLIENT_TOKEN", None)
        env.pop("SEECUT_MCP_WRITE_TOKEN", None)
        self.event("launch", head=actual, expected_head=self.args.expected_sha,
                   platform=platform.platform(), binary_sha256=hashlib.sha256(copied.read_bytes()).hexdigest(),
                   display=env["DISPLAY"], portable_strategy="copied executable beside fresh portable",
                   startup_preferences={"locale": "en", "dark": False, "server_enabled": False},
                   preference_source="explicit isolated configuration, not UI operation",
                   fixture="owned 16x16 PNG prepared only; not imported",
                   default_home="SeeCut.page=1 generation workbench; source expectation")
        self.process = subprocess.Popen([str(copied)], cwd=self.work, env=env,
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.reader = threading.Thread(target=self.collect_log, daemon=True)
        self.reader.start()
        # Popen has no timeout argument. This watchdog bounds the App itself;
        # all synchronous child commands additionally have explicit timeouts.
        watchdog = threading.Timer(self.remaining(), self.process.terminate)
        watchdog.daemon = True
        watchdog.start()
        try:
            startup_deadline = min(self.deadline, time.monotonic() + 60)
            while time.monotonic() < startup_deadline:
                self.alive()
                matches = self.windows()
                if len(matches) > 1:
                    raise RuntimeError(f"Multiple owned Seecut windows: {matches}")
                if matches:
                    self.window = matches[0]
                    break
                self.settle(0.4)
            if not self.window:
                raise RuntimeError("No owned visible Seecut window within 60 seconds")
            self.event("window_found", pid=self.process.pid, window=self.window,
                       geometry=self.geometry())
            self.resize(1280, 900)
            self.preferences()
            self.capture("01-settings-preferences-en-light.png")
            self.advanced()
            self.capture("02-settings-general-en-light.png")
            for locale in ("en", "zh-Hans"):
                if locale != self.locale:
                    self.click(140, 183, "General tab")
                    self.verify_tab(1)
                    self.click(400, 176, "Language dropdown")
                    self.key("Home", *("Down",) * 11, "Return")
                    self.locale = locale
                    self.verify_prefs()
                for dark in (False, True):
                    if dark != self.dark:
                        self.preferences()
                        self.click(737 if dark else 643, 240, "Appearance option")
                        self.dark = dark
                        self.verify_prefs()
                        self.advanced()
                    self.click(140, 279, "Remote tab, no permission button")
                    self.verify_tab(4)
                    for label, width, height in (("wide", 1280, 900), ("narrow", 900, 700)):
                        self.resize(width, height)
                        self.verify_tab(4)
                        self.verify_prefs()
                        self.capture(f"remote-{locale}-{'dark' if dark else 'light'}-{label}.png")
                    self.resize(1280, 900)
            self.event("candidate_capture_complete", remote_images=8,
                       acceptance="not established; visual review required",
                       limitations="Linux X11 cannot establish macOS native focus/clipboard behavior")
        finally:
            watchdog.cancel()

    def preserve_failure_observation(self):
        # Copy only the already captured image. No X11 command or new App
        # interaction occurs after a failure. It may predate the failed step.
        if not self.owns_output or self.work is None:
            return
        source = self.work / "probe.png"
        if source.is_symlink() or not source.is_file():
            self.event("failure_observation_skipped", reason="no existing regular probe.png")
            return
        size = source.stat().st_size
        total = sum(path.stat().st_size for path in self.output.iterdir())
        remaining = MAX_OUTPUT - 1024 * 1024 - total
        if size > remaining:
            self.event("failure_observation_skipped", reason="14 MiB PNG reserve limit",
                       source_bytes=size, remaining_bytes=max(0, remaining))
            return
        target = self.output / "last-observation-before-failure.png"
        shutil.copyfile(source, target)
        self.event("failure_observation_preserved", file=target.name, bytes=size,
                   sha256=hashlib.sha256(target.read_bytes()).hexdigest(),
                   status="candidate; captured before failure; visual pending",
                   observation=self.last_observation,
                   interpretation="last existing probe image; may predate failed navigation; no new capture")

    def cleanup(self):
        if self.process is not None:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=6)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=4)
            self.event("app_stopped", pid=self.process.pid, exit_code=self.process.returncode)
            if self.reader is not None:
                self.reader.join(timeout=1)
        if self.owns_output:
            with self.log_lock:
                log = b"".join(self.log_chunks).decode(errors="replace")
            (self.output / "app-log.txt").write_text(redact(log)[-LOG_LIMIT:])
            (self.output / "events.jsonl").write_text("".join(
                json.dumps(event, ensure_ascii=False) + "\n" for event in self.events))
        if self.work is not None:
            shutil.rmtree(self.work)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--output")
    parser.add_argument("--expected-sha")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    if not all((args.binary, args.output, args.expected_sha)):
        parser.error("--binary, --output and --expected-sha are required")
    probe = Probe(args)
    def stopped(signum, frame):
        raise RuntimeError(f"Interrupted by signal {signum}")
    signal.signal(signal.SIGTERM, stopped)
    signal.signal(signal.SIGINT, stopped)
    code = 0
    try:
        probe.run()
    except Exception as error:
        message = redact(str(error))[:3000]
        probe.event("failed", error=message, later_navigation="stopped")
        try:
            probe.preserve_failure_observation()
        except Exception as copy_error:
            probe.event("failure_observation_skipped", reason=redact(str(copy_error))[:1000])
        print(message, file=sys.stderr)
        code = 1
    finally:
        probe.cleanup()
    if code == 0:
        print("10 candidate PNGs captured; permission text and layout await visual review")
    return code


if __name__ == "__main__":
    sys.exit(main())
