"""Versioned prices and task rounding against disposable wallets only."""
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
import json
import os
import sqlite3
import unittest
from unittest.mock import patch

from seecut_server.assistant_billing import capture_for_usage, credit_cost
from seecut_server.assistant_contract import canonical, digest
from seecut_server.assistant_local_resolution import LocalResolution
from seecut_server.assistant_models import model_config, parse_models
from seecut_server.config import Config, ConfigError
from seecut_server.errors import ApiError
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture, done
from test_assistant_efforts import specification
from test_service import FakeEmailSender
from test_generation_robustness import FakeImage2


class PriceUnitTest(unittest.TestCase):
    def test_integer_ceil_and_legacy_defaults(self):
        for numerator, expected in [(0, 0), (320, 1), (1000, 1), (1001, 2)]:
            self.assertEqual(credit_cost(numerator, 0, 1, 0), expected)
            self.assertEqual(credit_cost(numerator * 1000, 0, 1, 0, 1_000_000), expected)
        self.assertEqual(credit_cost(100_000, 50_000, 20, 60, 1_000_000), 5)
        old = dict(input_rate=2, output_rate=5)
        self.assertEqual(capture_for_usage(old, {'spent_credits': 2}, {'input': 100, 'output': 50}).credits, 1)
        for unit in (True, '1000', 1000.0, 0, 999, 1_000_001):
            with self.assertRaises(ValueError):
                credit_cost(1, 1, 1, 1, unit)

    def test_config_units_must_be_explicit_and_unmixed(self):
        env = {'SECUT_ENV': 'test', 'SECUT_ASSISTANT_RATE_DENOMINATOR': '1000000',
               'SECUT_ASSISTANT_INPUT_CREDITS_PER_1000000': '20',
               'SECUT_ASSISTANT_OUTPUT_CREDITS_PER_1000000': '60'}
        with patch.dict(os.environ, env, clear=True):
            config = Config.from_env()
        self.assertEqual((config.assistant_rate_denominator, config.assistant_input_rate, config.assistant_output_rate),
                         (1_000_000, 20, 60))
        alias = dict(id='old', label='old', model='synthetic', input_rate=2, output_rate=6)
        self.assertEqual(model_config(config, parse_models(json.dumps([alias]))[0]).assistant_rate_denominator, 1000)
        with self.assertRaises(ValueError):
            model_config(config, alias | {'input_rate': 2, 'output_rate': 6, 'rate_denominator': False})
        for missing in ('input_rate', 'output_rate'):
            with self.assertRaises(ValueError):
                model_config(config, {k: v for k, v in alias.items() if k != missing})
        for invalid in (env | {'SECUT_ASSISTANT_INPUT_CREDITS_PER_1000': '2'},
                        env | {'SECUT_ASSISTANT_RATE_DENOMINATOR': '1000'},
                        env | {'SECUT_ASSISTANT_RATE_DENOMINATOR': '100'},
                        {k: v for k, v in env.items() if k != 'SECUT_ASSISTANT_OUTPUT_CREDITS_PER_1000000'}):
            with patch.dict(os.environ, invalid, clear=True), self.assertRaises(ConfigError):
                Config.from_env()
        with patch.dict(os.environ, {'SECUT_ENV': 'test'}, clear=True):
            self.assertEqual(Config.from_env().assistant_rate_denominator, 1000)


class TaskRoundingTest(AssistantFixture):
    def price(self, rate=1000, unit=1_000_000):
        self.config = replace(self.config, assistant_input_rate=rate, assistant_output_rate=rate,
                              assistant_rate_denominator=unit)
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
                                    clock=lambda: self.clock[0], assistant_provider=self.provider,
                                    image2=getattr(self, 'fixture_image2', None))
        self.service = self.account.assistant

    def settle(self, operation, usage):
        result = done()
        result['usage'] = usage
        self.service.finish(operation['operation_id'], 'succeeded', usage, result)
        return self.service.get(self.user, operation['operation_id'])

    def step(self, first, name):
        return self.create(self.body(step=name, budget={'budget_id': first['budget_id']}))

    def rows(self):
        with self.db.connect() as connection:
            tables = [r[0] for r in connection.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
            return {t: [dict(r) for r in connection.execute(f'SELECT * FROM {t} ORDER BY rowid')] for t in tables}

    def test_point_two_plus_point_two_is_one_and_zero_delta_completes(self):
        self.price()
        first = self.create()
        first = self.settle(first, {'input': 100, 'output': 100})
        self.assertEqual((first['captured_credits'], first['budget']['spent_credits'],
                          first['budget']['accrued_microcredits']), (1, 1, 200_000))
        second = self.step(first, 'step-2')
        second = self.settle(second, {'input': 100, 'output': 100})
        self.assertEqual((second['state'], second['billing_state'], second['captured_credits'],
                          second['released_credits']), ('succeeded', 'released', 0, second['reserved_credits']))
        self.assertEqual((second['budget']['spent_credits'], second['budget']['accrued_microcredits']), (1, 400_000))
        self.wallet(99, 0)
        before = self.rows()
        self.settle(second, {'input': 100, 'output': 100})
        self.assertEqual(self.rows(), before)

    def test_point_six_plus_point_six_crosses_one_only_once(self):
        self.price(3000)
        first = self.settle(self.create(), {'input': 100, 'output': 100})
        second = self.step(first, 'step-2')
        second = self.settle(second, {'input': 100, 'output': 100})
        self.assertEqual((second['captured_credits'], second['budget']['spent_credits'],
                          second['budget']['accrued_microcredits']), (1, 2, 1_200_000))
        self.wallet(98, 0)

    def test_zero_and_integral_total_do_not_add_extra_credit(self):
        self.price(5000)
        first = self.settle(self.create(), {'input': 0, 'output': 0})
        self.assertEqual((first['captured_credits'], first['budget']['spent_credits']), (0, 0))
        second = self.settle(self.step(first, 'step-2'), {'input': 100, 'output': 100})
        self.assertEqual((second['captured_credits'], second['budget']['spent_credits'],
                          second['budget']['accrued_microcredits']), (1, 1, 1_000_000))
        self.wallet(99, 0)

    def test_unknown_missing_and_over_limit_usage_do_not_accrue(self):
        self.price()
        operation = self.create()
        self.service.unknown(operation['operation_id'])
        with self.assertRaises(ApiError):
            self.service.finish(operation['operation_id'], 'failed', {'input': -1, 'output': 0})
        for usage in (None, {'input': 100, 'output': 201}):
            self.service.finish(operation['operation_id'], 'failed', usage)
            current = self.service.get(self.user, operation['operation_id'])
            self.assertEqual((current['budget']['spent_credits'], current['budget']['accrued_microcredits'],
                              current['captured_credits']), (0, 0, 0))
            self.wallet(100 - operation['reserved_credits'], operation['reserved_credits'])

    def test_failed_and_cancelled_verified_usage_are_charged_once(self):
        self.price()
        operation = self.create()
        self.service.finish(operation['operation_id'], 'cancelled', {'input': 100, 'output': 100})
        current = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((current['budget']['spent_credits'], current['budget']['accrued_microcredits'],
                          current['budget']['state']), (1, 200_000, 'closed'))
        before = self.rows()
        self.service.finish(operation['operation_id'], 'failed', {'input': 100, 'output': 100})
        self.assertEqual(self.rows(), before)
        self.wallet(99, 0)

    def test_concurrent_finish_and_injected_crash_are_atomic(self):
        self.price()
        operation = self.create()
        before = self.rows()
        with patch.object(self.service, '_ledger', side_effect=RuntimeError('synthetic crash')):
            with self.assertRaises(RuntimeError):
                self.settle(operation, {'input': 100, 'output': 100})
        self.assertEqual(self.rows(), before)
        with ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(lambda _: self.settle(operation, {'input': 100, 'output': 100}), range(8)))
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((final['budget']['spent_credits'], final['budget']['accrued_microcredits']), (1, 200_000))
        self.assertEqual(sum(r['kind'] == 'capture' for r in self.wallet(99, 0)), 1)

    def test_d1_preview_and_apply_use_same_delta_and_replay_once(self):
        self.price()
        first = self.settle(self.create(), {'input': 100, 'output': 100})
        operation = self.step(first, 'step-2')
        self.service.unknown(operation['operation_id'])
        operation = self.service.get(self.user, operation['operation_id'])
        usage = {'input': 100, 'output': 100}
        result = done(); result['usage'] = usage
        body = dict(user_id=self.user, operation_id=operation['operation_id'], expected_version=operation['version'],
                    resolution_id='task:step2', operator='synthetic-reviewer', reason='Synthetic verified usage',
                    outcome='confirmed_usage', terminal_state='succeeded', usage=usage, result=result,
                    evidence_confirmed=True, evidence_reference={'id': 'fixture:step2', 'sha256': 'a' * 64})
        resolver = LocalResolution(self.db, clock=lambda: self.clock[0])
        before = self.rows()
        preview = resolver.resolve(body)
        self.assertEqual((preview['status'], preview['proposed']['cost']), ('ready', 0))
        self.assertEqual(self.rows(), before)
        # Failure after settlement but before audit commit must roll everything back.
        with self.db.connect() as connection:
            connection.execute("CREATE TRIGGER synthetic_audit_crash BEFORE INSERT ON assistant_resolutions BEGIN SELECT RAISE(ABORT, 'synthetic crash'); END")
        with self.assertRaises(sqlite3.IntegrityError):
            resolver.resolve(body, apply=True)
        self.assertEqual(self.rows(), before)
        with self.db.connect() as connection:
            connection.execute('DROP TRIGGER synthetic_audit_crash')
        def attempt(_):
            try:
                return resolver.resolve(body, apply=True)
            except ApiError as error:
                self.assertEqual(error.code, 'ASSISTANT_EXECUTOR_ACTIVE')
                return None
        with ThreadPoolExecutor(max_workers=4) as pool:
            resolutions = list(pool.map(attempt, range(8)))
        applied = next(r for r in resolutions if r is not None)
        self.assertTrue(all(r is None or r == applied for r in resolutions))
        self.assertEqual(resolver.resolve(body, apply=True), applied)
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((final['captured_credits'], final['budget']['spent_credits'],
                          final['budget']['accrued_microcredits']), (0, 1, 400_000))
        before = self.rows()
        self.assertEqual(resolver.resolve(body, apply=True), applied)
        self.settle(final, usage)
        self.assertEqual(self.rows(), before)
        self.wallet(99, 0)

    def test_migration_keeps_old_json_units_and_operation_rounding(self):
        first = self.create()
        with self.db.transaction(immediate=True) as connection:
            for table, columns in [('assistant_quotes', ['rate_denominator']),
                                   ('assistant_operations', ['rate_denominator']),
                                   ('assistant_turn_budgets', ['rounding_mode', 'accrued_microcredits'])]:
                for name in columns:
                    connection.execute(f'ALTER TABLE {table} DROP COLUMN {name}')
        self.db.initialize(); self.db.initialize()
        with self.db.connect() as connection:
            row = connection.execute('SELECT * FROM assistant_operations WHERE id=?', (first['operation_id'],)).fetchone()
            self.assertEqual((row['input_rate'], row['output_rate'], row['rate_denominator']), (2, 5, 1000))
        first = self.settle(first, {'input': 100, 'output': 50})
        second = self.settle(self.step(first, 'step-2'), {'input': 100, 'output': 50})
        self.assertEqual((second['budget']['rounding_mode'], second['budget']['spent_credits'],
                          second['budget']['accrued_microcredits']), ('legacy_operation', 2, 0))
        self.wallet(98, 0)

    def test_quote_freezes_unit_and_rejects_mismatch(self):
        body = self.body()
        quote = self.service.quote(self.user, body, 'quote-unit')
        with self.db.transaction(immediate=True) as connection:
            connection.execute('UPDATE assistant_quotes SET rate_denominator=1000000 WHERE id=?', (quote['quote_id'],))
        self.expect_error('ASSISTANT_QUOTE_MISMATCH', lambda: self.service.create(
            self.user, body | {'quote_id': quote['quote_id']}, 'create-unit'))
        self.wallet(100, 0)

    def test_v1_policy_json_is_unchanged_and_v2_is_explicit(self):
        effort = specification(model='default', credits=10)
        self.fixture_image2 = FakeImage2()
        self.config = replace(self.config, efforts=(effort,), model_prices={'image:gpt-image-2.5-flare': 7})
        self.price(rate=2, unit=1000)
        policy = {k: v for k, v in self.service.capabilities()['efforts'][0].items()
                  if k not in {'available', 'reason_code'}}
        expected = effort | {'agent': effort['agent'] | dict(pricing_version='fixture-1', context_window=1000,
                                                            input_rate=2, output_rate=2)}
        self.assertEqual(canonical(policy), canonical(expected))
        self.assertEqual(digest(policy), digest(expected))
        self.assertNotIn('rate_denominator', policy['agent'])
        self.assertEqual(self.service.capabilities()['effort_policy_version'], 1)
        self.price(rate=20)
        current = self.service.capabilities()
        self.assertEqual(current['protocol_version'], 1)
        self.assertEqual(current['effort_policy_version'], 2)
        self.assertEqual(current['efforts'][0]['agent']['rate_denominator'], 1_000_000)
