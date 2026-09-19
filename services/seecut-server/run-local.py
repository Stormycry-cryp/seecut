#!/usr/bin/env python3
"""Run the local relay using explicitly selected, existing skill credentials."""

import argparse
import importlib.util
import os
from pathlib import Path
import secrets
import sys


def load_helper(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Cannot load helper: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skill-credentials", action="store_true")
    parser.add_argument("--port", type=int, default=8787)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    os.chdir(root)
    sys.path.insert(0, str(root / "src"))
    os.environ["SECUT_HOST"] = "127.0.0.1"
    os.environ["SECUT_PORT"] = str(args.port)
    os.environ.setdefault("SECUT_PUBLIC_BASE_URL", f"http://127.0.0.1:{args.port}")
    os.environ.setdefault("SECUT_SIGNING_SECRET", secrets.token_urlsafe(48))
    if args.skill_credentials:
        skills = Path.home() / ".codex" / "skills"
        if not os.environ.get("SECUT_IMAGE2_API_KEY"):
            helper = load_helper(skills / "image2-gen/scripts/image2_gen.py", "image2_config")
            os.environ["SECUT_IMAGE2_API_KEY"] = helper.resolve_api_key()
        if not os.environ.get("SECUT_XIANGXIN_API_KEY"):
            helper = load_helper(skills / "xiangxin-video/scripts/video.py", "video_config")
            os.environ["SECUT_XIANGXIN_API_KEY"] = helper.load_api_key()
    from seecut_server.app import main as serve
    serve()


if __name__ == "__main__":
    main()
