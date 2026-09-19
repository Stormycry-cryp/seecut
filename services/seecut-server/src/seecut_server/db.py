from __future__ import annotations

import sqlite3
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator


class Database:
    def __init__(self, path: Path, schema_path: Path):
        self.path = path
        self.schema_path = schema_path

    def initialize(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.connect() as connection:
            connection.executescript(self.schema_path.read_text(encoding="utf-8"))
            columns = {row["name"] for row in connection.execute("PRAGMA table_info(generation_tasks)")}
            migrations = {
                "provider": "ALTER TABLE generation_tasks ADD COLUMN provider TEXT NOT NULL DEFAULT 'xiangxin'",
                "operation": "ALTER TABLE generation_tasks ADD COLUMN operation TEXT NOT NULL DEFAULT 'generate'",
                "next_attempt_at": "ALTER TABLE generation_tasks ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0",
                "attempt_count": "ALTER TABLE generation_tasks ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0",
                "lease_owner": "ALTER TABLE generation_tasks ADD COLUMN lease_owner TEXT",
                "lease_expires_at": "ALTER TABLE generation_tasks ADD COLUMN lease_expires_at INTEGER",
            }
            for name, statement in migrations.items():
                if name not in columns:
                    connection.execute(statement)
            upload_columns = {row["name"] for row in connection.execute("PRAGMA table_info(uploads)")}
            if "write_token" not in upload_columns:
                connection.execute("ALTER TABLE uploads ADD COLUMN write_token TEXT")
            output_columns = {
                row["name"] for row in connection.execute("PRAGMA table_info(generation_outputs)")
            }
            if "expires_at" not in output_columns:
                connection.execute(
                    "ALTER TABLE generation_outputs ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0"
                )

    @contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        connection = sqlite3.connect(self.path, timeout=10, isolation_level=None)
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA foreign_keys = ON")
        connection.execute("PRAGMA busy_timeout = 10000")
        try:
            yield connection
        finally:
            connection.close()

    @contextmanager
    def transaction(self, immediate: bool = False) -> Iterator[sqlite3.Connection]:
        with self.connect() as connection:
            try:
                connection.execute("BEGIN IMMEDIATE" if immediate else "BEGIN")
                yield connection
                connection.execute("COMMIT")
            except Exception:
                connection.execute("ROLLBACK")
                raise
