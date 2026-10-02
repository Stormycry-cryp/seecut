#!/usr/bin/env python3
"""Seal private runner evidence; publish only AES-256-GCM CMS ciphertext."""

import argparse
import hashlib
import os
from pathlib import Path
import stat
import subprocess
import tarfile
import tempfile


LIMIT = 15 * 1024 * 1024


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "recipient", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    if not args.source.exists():
        print("No QA evidence directory; nothing to publish")
        return 0
    assert args.source.is_absolute() and not args.source.is_symlink()
    assert args.source.is_dir() and args.source.stat().st_uid == os.getuid()
    assert args.output.is_absolute() and not args.output.exists()
    assert stat.S_ISREG(args.recipient.lstat().st_mode)
    assert b"PRIVATE KEY" not in args.recipient.read_bytes()
    files, size = [], 0
    for root, dirs, names in os.walk(args.source, followlinks=False):
        for name in dirs:
            info = (Path(root) / name).lstat()
            assert stat.S_ISDIR(info.st_mode), "Unsafe evidence directory"
        for name in names:
            path = Path(root) / name
            info = path.lstat()
            assert stat.S_ISREG(info.st_mode) and info.st_nlink == 1, "Unsafe evidence file"
            size += info.st_size
            assert size <= LIMIT, "Private evidence exceeds 15 MiB"
            files.append(path)
    if not files:
        print("No QA evidence files; nothing to publish")
        return 0
    args.output.mkdir(mode=0o700)
    sealed = args.output / "private-evidence.cms"
    with tempfile.TemporaryDirectory(prefix="seecut-seal-", dir=args.source.parent) as temporary:
        archive = Path(temporary) / "private-evidence.tar.gz"
        with tarfile.open(archive, "w:gz") as package:
            for path in sorted(files):
                package.add(path, arcname=str(path.relative_to(args.source)), recursive=False)
        command = ["openssl", "cms", "-encrypt", "-binary", "-aes-256-gcm",
                   "-outform", "DER", "-in", str(archive), "-out", str(sealed),
                   str(args.recipient)]
        try:
            result = subprocess.run(command, capture_output=True, timeout=20)
        except BaseException:
            sealed.unlink(missing_ok=True)
            raise
        if result.returncode:
            sealed.unlink(missing_ok=True)
            raise RuntimeError("Private evidence encryption failed; plaintext upload forbidden")
    sealed.chmod(0o600)
    if sealed.stat().st_size > LIMIT:
        sealed.unlink()
        raise RuntimeError("Encrypted evidence exceeds 15 MiB; upload forbidden")
    digest = hashlib.sha256(sealed.read_bytes()).hexdigest()
    print(f"Encrypted evidence: {sealed.stat().st_size} bytes; SHA256 {digest}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
