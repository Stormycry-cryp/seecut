"""Migration of a genuine v1 effort artifact from fixed old public service code."""
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

from seecut_server.assistant_contract import digest
from seecut_server.assistant_models import parse_models
from seecut_server.config import Config
from seecut_server.db import Database
from seecut_server.service import SeeCutService
from test_assistant_contract import FakeAssistant, done
from test_assistant_efforts import specification
from test_generation_robustness import FakeImage2
from test_service import FakeEmailSender


class LegacyEffortMigrationTest(unittest.TestCase):
    def test_old_spent_budget_and_accepted_operation_keep_frozen_v1_policy(self):
        fixtures = Path(__file__).parent / 'fixtures'
        metadata = json.loads((fixtures / 'legacy_effort_3562.json').read_text())
        sql = (fixtures / 'legacy_effort_3562.sql').read_bytes()
        self.assertEqual(metadata['source_commit'], '3562c6df0cab266e931e94a6da251c2c3227e874')
        self.assertEqual(hashlib.sha256(sql).hexdigest(), metadata['sql_sha256'])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with sqlite3.connect(root / 'test.db') as connection:
                connection.executescript(sql.decode())
                columns = {row[0]: [column[1] for column in connection.execute('PRAGMA table_info(' + row[0] + ')')]
                           for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")}
                def snapshot():
                    return {table: connection.execute('SELECT ' + ','.join(fields) + ' FROM ' + table + ' ORDER BY rowid').fetchall()
                            for table, fields in columns.items()}
                before = snapshot()
                self.assertNotIn('rate_denominator', columns['assistant_operations'])
                self.assertNotIn('rounding_mode', columns['assistant_turn_budgets'])
                self.assertNotIn('billing_mode', columns['assistant_turn_budgets'])
                with patch.dict(os.environ, {'SECUT_ENV': 'test'}, clear=True):
                    config = Config.from_env()
                config = replace(config, database_path=root / 'test.db', storage_path=root / 'storage',
                    assistant_base_url='http://127.0.0.1:1/v1', assistant_model='synthetic-model',
                    assistant_api_key='synthetic-key', assistant_pricing_version='million-v2',
                    assistant_input_rate=20, assistant_output_rate=60, assistant_rate_denominator=1_000_000,
                    assistant_context_window=1000, assistant_max_output_tokens=200, assistant_max_turn_credits=10,
                    assistant_models=parse_models(json.dumps([
                        dict(id='fast', label='快捷', model='synthetic-fast', max_turn_credits=12,
                             input_rate=20, output_rate=60, rate_denominator=1_000_000),
                        dict(id='quality', label='深入', model='synthetic-quality', max_turn_credits=20,
                             input_rate=20, output_rate=60, rate_denominator=1_000_000)])),
                    model_prices={'image:gpt-image-2.5-flare': 7},
                    efforts=(specification(revision='policy-2', output=128, steps=4, credits=10),))
                db = Database(config.database_path, Path(__file__).resolve().parents[1] / 'schema.sql')
                db.initialize()
                db.initialize()
                self.assertEqual(snapshot(), before)
                for table in ('assistant_turn_budgets', 'assistant_quotes', 'assistant_operations'):
                    migrated = connection.execute('SELECT billing_mode,billing_contract_json,billing_contract_digest FROM ' + table).fetchall()
                    self.assertTrue(migrated)
                    self.assertTrue(all(row == ('verified_input_v1', None, None) for row in migrated))
                    with self.assertRaises(sqlite3.IntegrityError):
                        connection.execute("UPDATE " + table + " SET billing_mode='unknown_mode'")
                self.assertTrue(all(row == ('0',) for row in connection.execute('SELECT accrued_numerator FROM assistant_turn_budgets')))
                self.assertTrue(all(row == (None, None, None) for row in connection.execute('SELECT provider_usage_json,reported_numerator,uncollected_numerator FROM assistant_operations')))
                self.assertEqual(connection.execute('PRAGMA integrity_check').fetchone()[0], 'ok')
                self.assertEqual(connection.execute('PRAGMA foreign_key_check').fetchall(), [])
            provider = FakeAssistant()
            account = SeeCutService(config, db, emailer=FakeEmailSender(config), image2=FakeImage2(),
                clock=lambda: 1_700_000_000, assistant_provider=provider,
                assistant_providers={'fast': provider, 'quality': FakeAssistant()})
            service, user = account.assistant, metadata['user_id']

            def frozen():
                with db.connect() as connection:
                    row = dict(connection.execute('SELECT * FROM assistant_turn_budgets WHERE id=?',
                               (metadata['budget_id'],)).fetchone())
                self.assertEqual(row['policy_json'], metadata['policy_json'])
                self.assertEqual(digest(json.loads(row['policy_json'])), metadata['expected_policy_digest'])
                self.assertNotIn('rate_denominator', json.loads(row['policy_json'])['agent'])
                self.assertEqual((row['rounding_mode'], row['accrued_microcredits']), ('legacy_operation', 0))
                return row

            def settle(operation):
                service.finish(operation['operation_id'], 'succeeded', {'input': 100, 'output': 50}, done())
                return service.get(user, operation['operation_id'])

            def quoted_prices(quote):
                with db.connect() as connection:
                    row = connection.execute('SELECT * FROM assistant_quotes WHERE id=?', (quote['quote_id'],)).fetchone()
                return row['input_rate'], row['output_rate'], row['rate_denominator'], row['pricing_version']

            self.assertEqual(account.wallet(user), metadata['initial_wallet'])
            self.assertEqual(frozen()['spent_credits'], 1)
            restored = service.turn_budget(user, metadata['budget_request'], metadata['budget_request']['client_request_id'])
            self.assertEqual(restored['budget_id'], metadata['budget_id'])
            self.assertEqual(restored['policy'], json.loads(metadata['policy_json']))
            accepted = service.create(user, metadata['create_body'], 'create-step-2')[0]
            self.assertEqual(accepted['operation_id'], metadata['accepted_operation_id'])
            self.assertEqual(account.wallet(user), metadata['initial_wallet'])
            with db.connect() as connection:
                rows = list(connection.execute('SELECT * FROM assistant_operations'))
                self.assertEqual(len(rows), 2)
                self.assertTrue(all((row['input_rate'], row['output_rate'], row['rate_denominator']) == (2, 5, 1000) for row in rows))
            second = settle(accepted)
            self.assertEqual((second['captured_credits'], frozen()['spent_credits']), (1, 2))
            wallet = account.wallet(user)
            settle(second)
            self.assertEqual(account.wallet(user), wallet)

            third_body = {key: value for key, value in metadata['create_body'].items() if key != 'quote_id'}
            third_body.update(client_request_id='step-3', step_id='step-3')
            quote = service.quote(user, third_body, 'quote-step-3')
            self.assertEqual(quoted_prices(quote), (2, 5, 1000, 'fixture-1'))
            third = service.create(user, third_body | {'quote_id': quote['quote_id']}, 'create-step-3')[0]
            self.assertEqual(settle(third)['captured_credits'], 1)
            self.assertEqual(frozen()['spent_credits'], 3)
            self.assertEqual((account.wallet(user)['available_credits'], account.wallet(user)['held_credits']), (97, 0))
            service.close_budget(user, metadata['budget_id'], {}, 'close-old')
            frozen()

            catalog = service.capabilities()
            self.assertEqual(catalog['effort_policy_version'], 2)
            policy = {key: value for key, value in catalog['efforts'][0].items() if key not in {'available', 'reason_code'}}
            self.assertEqual(policy['agent']['rate_denominator'], 1_000_000)
            self.assertNotEqual(digest(policy), metadata['expected_policy_digest'])
            new_request = metadata['budget_request'] | dict(client_request_id='budget-new', turn_id='turn-new',
                policy_revision='policy-2', expected_policy_digest=digest(policy))
            budget = service.turn_budget(user, new_request, 'budget-new')
            self.assertNotEqual(budget['budget_id'], metadata['budget_id'])
            self.assertEqual((budget['rounding_mode'], budget['accrued_microcredits']), ('task_cumulative_v1', 0))
            body = third_body | dict(client_request_id='new-step', step_id='new-step', turn_id='turn-new',
                budget_id=budget['budget_id'], policy_revision='policy-2')
            quote = service.quote(user, body, 'quote-new-step')
            self.assertEqual(quoted_prices(quote), (20, 60, 1_000_000, 'million-v2'))
            new = service.create(user, body | {'quote_id': quote['quote_id']}, 'create-new-step')[0]
            settled = settle(new)
            self.assertEqual((settled['budget']['spent_credits'], settled['budget']['accrued_microcredits']), (1, 5000))
            frozen()
