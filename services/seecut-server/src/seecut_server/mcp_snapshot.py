from __future__ import annotations

import argparse
import os
import sqlite3
import tempfile
import time
from pathlib import Path


def create_snapshot(source: Path, destination: Path) -> None:
    """Create a consistent SQLite backup and atomically publish it."""
    if not source.is_file():
        raise FileNotFoundError("source database does not exist")
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(
        prefix=f".{destination.name}.", suffix=".tmp", dir=destination.parent
    )
    os.close(fd)
    temporary = Path(temporary_name)
    try:
        source_uri = f"file:{source.resolve().as_posix()}?mode=ro"
        with sqlite3.connect(source_uri, uri=True, timeout=2) as source_db:
            with sqlite3.connect(temporary) as snapshot_db:
                source_db.backup(snapshot_db, pages=256, sleep=0.025)
                snapshot_db.execute(
                    "CREATE TABLE IF NOT EXISTS mcp_snapshot_metadata "
                    "(created_at INTEGER NOT NULL, source_schema_version INTEGER NOT NULL)"
                )
                snapshot_db.execute("DELETE FROM mcp_snapshot_metadata")
                schema_version = source_db.execute("PRAGMA schema_version").fetchone()[0]
                snapshot_db.execute(
                    "INSERT INTO mcp_snapshot_metadata(created_at, source_schema_version) VALUES(?,?)",
                    (int(time.time()), schema_version),
                )
                snapshot_db.commit()
                if snapshot_db.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                    raise RuntimeError("snapshot integrity check failed")
                snapshot_db.execute("PRAGMA journal_mode=DELETE")
        os.chmod(temporary, 0o440)
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser(description="Create the SeeCut read-only MCP snapshot")
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    create_snapshot(args.source, args.destination)


if __name__ == "__main__":
    main()
