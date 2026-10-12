from __future__ import annotations

import sqlite3
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator


class Database:
    def __init__(self, path: Path, schema_path: Path, *, read_only: bool = False):
        self.path = path
        self.schema_path = schema_path
        self.read_only = read_only

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
                "batch_id": "ALTER TABLE generation_tasks ADD COLUMN batch_id TEXT",
                "batch_index": "ALTER TABLE generation_tasks ADD COLUMN batch_index INTEGER NOT NULL DEFAULT 0",
                "batch_size": "ALTER TABLE generation_tasks ADD COLUMN batch_size INTEGER NOT NULL DEFAULT 1",
                "trashed_at": "ALTER TABLE generation_tasks ADD COLUMN trashed_at INTEGER",
            }
            for name, statement in migrations.items():
                if name not in columns:
                    connection.execute(statement)
            upload_columns = {row["name"] for row in connection.execute("PRAGMA table_info(uploads)")}
            if "write_token" not in upload_columns:
                connection.execute("ALTER TABLE uploads ADD COLUMN write_token TEXT")
            if "media_kind" not in upload_columns:
                connection.execute("ALTER TABLE uploads ADD COLUMN media_kind TEXT")
            if "duration_ms" not in upload_columns:
                connection.execute("ALTER TABLE uploads ADD COLUMN duration_ms INTEGER")
            output_columns = {
                row["name"] for row in connection.execute("PRAGMA table_info(generation_outputs)")
            }
            if "expires_at" not in output_columns:
                connection.execute(
                    "ALTER TABLE generation_outputs ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0"
                )
            quote_columns = {row["name"] for row in connection.execute("PRAGMA table_info(generation_quotes)")}
            if "effort_policy_fingerprint" not in quote_columns:
                connection.execute("ALTER TABLE generation_quotes ADD COLUMN effort_policy_fingerprint TEXT")
            budget_columns = {row["name"] for row in connection.execute("PRAGMA table_info(assistant_turn_budgets)")}
            if budget_columns and "model_id" not in budget_columns:
                connection.execute("ALTER TABLE assistant_turn_budgets ADD COLUMN model_id TEXT NOT NULL DEFAULT 'default'")
            for name in ('effort_id', 'policy_revision', 'policy_json', 'route_fingerprint'):
                if budget_columns and name not in budget_columns:
                    connection.execute(f'ALTER TABLE assistant_turn_budgets ADD COLUMN {name} TEXT')
            for table in ('assistant_quotes', 'assistant_operations'):
                columns = {row['name'] for row in connection.execute(f'PRAGMA table_info({table})')}
                if columns and 'rate_denominator' not in columns:
                    connection.execute(f'ALTER TABLE {table} ADD COLUMN rate_denominator INTEGER NOT NULL DEFAULT 1000 CHECK(rate_denominator IN (1000,1000000))')
            if budget_columns and 'rounding_mode' not in budget_columns:
                connection.execute("ALTER TABLE assistant_turn_budgets ADD COLUMN rounding_mode TEXT NOT NULL DEFAULT 'legacy_operation' CHECK(rounding_mode IN ('legacy_operation','task_cumulative_v1'))")
            if budget_columns and 'accrued_microcredits' not in budget_columns:
                connection.execute('ALTER TABLE assistant_turn_budgets ADD COLUMN accrued_microcredits INTEGER NOT NULL DEFAULT 0 CHECK(accrued_microcredits >= 0)')
            # Add opt-in usage contracts without rewriting any historical row,
            # policy JSON, price, route or operation-level rounding behavior.
            for table in ('assistant_turn_budgets', 'assistant_quotes', 'assistant_operations'):
                columns = {row['name'] for row in connection.execute(f'PRAGMA table_info({table})')}
                additions = {
                    'billing_mode': "TEXT NOT NULL DEFAULT 'verified_input_v1' CHECK(billing_mode IN ('verified_input_v1','provider_usage_capped_v1'))",
                    'billing_contract_json': 'TEXT',
                    'billing_contract_digest': 'TEXT',
                }
                if table == 'assistant_turn_budgets':
                    # Exact numerator over the frozen pricing denominator;
                    # TEXT avoids SQLite's signed-64-bit arithmetic coercion.
                    additions['accrued_numerator'] = "TEXT NOT NULL DEFAULT '0'"
                if table == 'assistant_operations':
                    additions.update(provider_usage_json='TEXT', reported_numerator='TEXT', uncollected_numerator='TEXT')
                if table in ('assistant_quotes', 'assistant_operations'):
                    additions['prepared_payload'] = 'BLOB'
                for name, declaration in additions.items():
                    if columns and name not in columns:
                        connection.execute(f'ALTER TABLE {table} ADD COLUMN {name} {declaration}')
            email_token_columns = {
                row["name"] for row in connection.execute("PRAGMA table_info(email_tokens)")
            }
            if "failed_attempts" not in email_token_columns:
                connection.execute(
                    "ALTER TABLE email_tokens ADD COLUMN failed_attempts INTEGER NOT NULL DEFAULT 0"
                )
            order_columns = {
                row["name"] for row in connection.execute("PRAGMA table_info(orders)")
            }
            if "expires_at" not in order_columns:
                # Earlier payment links did not impose a provider deadline.
                # NULL explicitly preserves that unknown state; do not invent
                # a deadline or close an order solely from its local age.
                connection.execute("ALTER TABLE orders ADD COLUMN expires_at INTEGER")

    @contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        target = self.path.resolve().as_uri() + "?mode=ro" if self.read_only else self.path
        connection = sqlite3.connect(target, uri=self.read_only, timeout=10, isolation_level=None)
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
                if connection.in_transaction:
                    connection.execute("ROLLBACK")
                raise
