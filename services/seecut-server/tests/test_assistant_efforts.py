"""Effort selection must freeze one server policy for the original turn."""
from __future__ import annotations

import json
import os
import threading
from concurrent.futures import ThreadPoolExecutor
from copy import deepcopy
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from seecut_server import assistant_contract as contract
from seecut_server.assistant_models import parse_models
from seecut_server.assistant_provider import ProviderFailure
from seecut_server.assistant_worker import AssistantWorker
from seecut_server.config import Config, ConfigError
from seecut_server.db import Database
from seecut_server.efforts import parse_efforts, validate_efforts
from seecut_server.errors import ApiError
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture, FakeAssistant, done
from test_generation_robustness import FakeImage2, FakeXiangxin
from test_service import FakeEmailSender


def specification(identity='lite', model='fast', revision='policy-1', output=100, steps=2, credits=8):
    return dict(id=identity, label=identity, policy_revision=revision,
                agent=dict(model_id=model, max_output_tokens=output, max_steps=steps, max_turn_credits=credits),
                generation=dict(image_model_ids=['gpt-image-2.5-flare'], video_model_ids=[]))


class AssistantEffortTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        self.fast, self.quality = FakeAssistant(), FakeAssistant()
        self.image2 = FakeImage2()
        models = parse_models(json.dumps([
            dict(id='fast', label='快捷', model='synthetic-fast', max_turn_credits=12),
            dict(id='quality', label='深入', model='synthetic-quality', max_turn_credits=20),
        ]))
        self.config = replace(self.config, assistant_models=models,
                              model_prices={'image:gpt-image-2.5-flare': 7},
                              efforts=parse_efforts(json.dumps([
                                  specification(), specification('strong', 'quality', output=200, steps=4, credits=18)])))
        self.reopen()

    def reopen(self):
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
                                    image2=self.image2, clock=lambda: self.clock[0], assistant_provider=self.provider,
                                    assistant_providers={'fast': self.fast, 'quality': self.quality})
        self.service = self.account.assistant
        self.worker = AssistantWorker(self.service)

    def budget_body(self, effort='lite', revision='policy-1', model='fast', turn='turn-1', key=None):
        selected = next((value for value in self.service.capabilities()['efforts'] if value['id'] == effort), None)
        policy = {key: value for key, value in selected.items() if key not in {'available', 'reason_code'}} if selected else {}
        return dict(model_id=model, effort_id=effort, policy_revision=revision,
                    expected_policy_digest=contract.digest(policy),
                    client_request_id=key or 'budget-' + turn, local_session_id='session-1', turn_id=turn)

    def selected_body(self, **kwargs):
        request = self.budget_body(**kwargs)
        budget = self.service.turn_budget(self.user, request, request['client_request_id'])
        body = self.body(budget=budget, turn=request['turn_id'])
        body['request']['maxOutputTokens'] = budget['policy']['agent']['max_output_tokens']
        return body | {key: request[key] for key in ('model_id', 'effort_id', 'policy_revision')}

    def test_catalog_is_shared_verified_and_has_no_provider_configuration(self):
        catalog = self.service.capabilities()
        generation = self.account.generation_models()
        for key in ('effort_policy_version', 'default_effort_id', 'efforts'):
            self.assertEqual(catalog[key], generation[key])
        self.assertEqual(catalog['default_effort_id'], 'lite')
        self.assertTrue(all(effort['available'] for effort in catalog['efforts']))
        policy = catalog['efforts'][0]['agent']
        self.assertEqual(policy['model_id'], 'fast')
        self.assertEqual((policy['max_output_tokens'], policy['max_steps'], policy['max_turn_credits']), (100, 2, 8))
        public = json.dumps(catalog)
        for hidden in ('synthetic-key', 'synthetic-fast', 'assistant_base_url', 'api_key'):
            self.assertNotIn(hidden, public)
        self.fast.token_contract_id = None
        catalog = self.service.capabilities()
        self.assertFalse(catalog['efforts'][0]['available'])
        self.assertEqual(catalog['efforts'][0]['reason_code'], 'ASSISTANT_INPUT_CONTRACT_UNAVAILABLE')
        self.expect_error('EFFORT_POLICY_UNAVAILABLE', lambda: self.selected_body())

    def test_non_executable_credit_budget_is_unavailable(self):
        self.config = replace(self.config, efforts=(specification(credits=1),))
        self.reopen()
        tier = self.service.capabilities()['efforts'][0]
        self.assertFalse(tier['available'])
        self.assertEqual(tier['reason_code'], 'EFFORT_BUDGET_UNAVAILABLE')
        self.expect_error('EFFORT_POLICY_UNAVAILABLE', lambda: self.selected_body())
        self.wallet(100, 0)

    def test_new_budget_requires_paired_current_policy_and_exact_model(self):
        body = self.budget_body()
        for value in ({key: item for key, item in body.items() if key not in {'effort_id', 'policy_revision'}},):
            self.expect_error('EFFORT_POLICY_REQUIRED', lambda: self.service.turn_budget(self.user, value, 'required'))
        for change in ({'effort_id': None}, {'policy_revision': None}, {'policy': {}}, {'effort_id': '../bad'}):
            self.expect_error('EFFORT_POLICY_INVALID', lambda: self.service.turn_budget(self.user, body | change, 'invalid'))
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.service.turn_budget(self.user, body | {'policy_revision': 'old'}, 'stale'))
        self.expect_error('EFFORT_POLICY_MISMATCH', lambda: self.service.turn_budget(self.user, body | {'model_id': 'quality'}, 'mismatch'))
        self.wallet(100, 0)
        with self.db.connect() as connection:
            self.assertEqual(connection.execute('SELECT COUNT(*) FROM assistant_turn_budgets').fetchone()[0], 0)

    def test_new_effort_budget_requires_canonical_digest_without_writing_state(self):
        body = self.budget_body()
        for value in ('', True, 'A' * 64, 'f' * 63):
            with self.subTest(value=value):
                request = body | {'expected_policy_digest': value}
                self.expect_error('EFFORT_POLICY_INVALID', lambda: self.service.turn_budget(
                    self.user, request, 'invalid-digest'))
        missing = {key: value for key, value in body.items() if key != 'expected_policy_digest'}
        self.expect_error('EFFORT_POLICY_DIGEST_REQUIRED', lambda: self.service.turn_budget(self.user, missing, 'missing'))
        self.expect_error('EFFORT_POLICY_DIGEST_REQUIRED', lambda: self.service.turn_budget(
            self.user, body | {'expected_policy_digest': None}, 'null-digest'))
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.service.turn_budget(
            self.user, body | {'expected_policy_digest': 'f' * 64}, 'different-digest'))
        with self.db.connect() as connection:
            for table in ('assistant_turn_budgets', 'assistant_idempotency', 'assistant_operations'):
                self.assertEqual(connection.execute('SELECT COUNT(*) FROM ' + table).fetchone()[0], 0)
        self.wallet(100, 0)

    def test_accepted_policy_drift_creates_no_budget_and_explicit_refresh_can_continue(self):
        original = deepcopy(self.config.efforts)
        original[0]['label'] = '强度／策略'
        original[0]['generation']['video_model_ids'] = ['sd_2.0_mini_special']
        self.config = replace(self.config, efforts=original, model_prices=self.config.model_prices |
                              {'video:sd_2.0_mini_special': 12})
        self.reopen()
        video = FakeXiangxin()
        self.account.xiangxin = video
        accepted = self.budget_body()
        # Availability changes after input acceptance but before first POST.
        video.configured = False
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.service.turn_budget(self.user, accepted, 'accepted'))
        # A same-revision configuration change is equally rejected.
        changed = deepcopy(original)
        changed[0]['agent']['max_steps'] = 1
        self.config = replace(self.config, efforts=changed)
        self.reopen()
        self.account.xiangxin = video
        video.configured = True
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.service.turn_budget(self.user, accepted, 'accepted'))
        with self.db.connect() as connection:
            for table in ('assistant_turn_budgets', 'assistant_idempotency', 'assistant_operations'):
                self.assertEqual(connection.execute('SELECT COUNT(*) FROM ' + table).fetchone()[0], 0)
        self.wallet(100, 0)
        current = self.budget_body()
        budget = self.service.turn_budget(self.user, current, 'refreshed')
        self.assertEqual(budget['policy']['agent']['max_steps'], 1)
        self.assertNotIn('expected_policy_digest', budget)
        self.assertEqual(contract.digest(budget['policy']), current['expected_policy_digest'])
        # Recovery ignores catalog drift and accepts journals predating digest.
        video.configured = False
        replay = self.service.turn_budget(self.user, accepted, 'recovery')
        self.assertEqual(replay, budget)
        legacy_request = {key: value for key, value in accepted.items() if key != 'expected_policy_digest'}
        self.assertEqual(self.service.turn_budget(self.user, legacy_request, 'old-journal'), budget)
        self.assertEqual(self.service.turn_budget(self.user, accepted | {'expected_policy_digest': 'malformed'}, 'read-original'), budget)

    def test_receipts_dispatch_and_replays_keep_complete_policy(self):
        body = self.selected_body()
        quote = self.service.quote(self.user, body, 'quote')
        expected = {key: value for key, value in self.service.capabilities()['efforts'][0].items()
                    if key not in {'available', 'reason_code'}}
        self.assertEqual(quote['policy'], expected)
        self.assertEqual((quote['effort_id'], quote['policy_revision']), ('lite', 'policy-1'))
        operation, created = self.service.create(self.user, body | {'quote_id': quote['quote_id']}, 'create')
        self.assertTrue(created)
        self.assertEqual(operation['budget']['policy'], expected)
        self.worker.process_once()
        self.assertEqual(self.fast.calls, [operation['operation_id']])
        self.assertEqual(self.quality.calls + self.provider.calls, [])
        self.assertEqual(json.loads(self.fast.prepared[operation['operation_id']].payload)['maxOutputTokens'], 100)
        for value in (self.service.get(self.user, operation['operation_id']),
                      self.service.by_request(self.user, body['client_request_id'])):
            self.assertEqual(value['budget']['policy'], expected)
        restored, created = self.service.create(self.user, body | {'quote_id': quote['quote_id']}, 'create')
        self.assertFalse(created)
        self.assertEqual(restored['operation_id'], operation['operation_id'])
        self.assertEqual(self.service.close_budget(self.user, body['budget_id'], {}, 'close')['policy'], expected)

    def test_catalog_changes_cannot_rewrite_existing_budget_limits_or_pricing(self):
        body = self.selected_body()
        original = self.service.turn_budget(self.user, self.budget_body(), 'budget-turn-1')
        self.config = replace(self.config, efforts=(specification(revision='policy-2', output=50, steps=1),),
                              assistant_input_rate=9, assistant_output_rate=12, assistant_pricing_version='fixture-2',
                              assistant_context_window=900)
        self.reopen()
        replay = self.service.turn_budget(self.user, self.budget_body(), 'budget-turn-1')
        self.assertEqual(replay['policy'], original['policy'])
        quote = self.service.quote(self.user, body, 'old-budget-quote')
        self.assertEqual(quote['policy'], original['policy'])
        self.assertEqual(quote['pricing_version'], 'fixture-1')
        self.assertEqual(quote['max_credits'], 3)
        operation, _ = self.service.create(self.user, body | {'quote_id': quote['quote_id']}, 'old-budget-create')
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, operation['operation_id'])['state'], 'succeeded')
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.service.turn_budget(
            self.user, self.budget_body(turn='turn-new'), 'old-policy-new-turn'))

    def test_removed_effort_does_not_redirect_unknown_or_resubmit(self):
        self.fast.failure = ProviderFailure()
        operation = self.create(self.selected_body())
        self.worker.process_once()
        expected = operation['budget']['policy']
        self.config = replace(self.config, efforts=())
        self.reopen()
        status = self.service.get(self.user, operation['operation_id'])
        self.assertEqual(status['state'], 'outcome_unknown')
        self.assertEqual(status['budget']['policy'], expected)
        self.fast.lookup_result = dict(state='succeeded', usage={'input': 100, 'output': 50}, result=done())
        final = self.service.reconcile(self.user, operation['operation_id'], {}, 'reconcile')
        self.assertEqual(final['state'], 'succeeded')
        self.assertEqual(final['budget']['policy'], expected)
        self.assertEqual(self.fast.calls, [operation['operation_id']])
        self.assertEqual(self.fast.lookups, [operation['operation_id']])
        self.assertEqual(self.quality.calls + self.quality.lookups, [])

    def test_model_route_changes_do_not_rebind_frozen_budget_or_queued_operation(self):
        body = self.selected_body()
        operation = self.create(body)
        models = deepcopy(self.config.assistant_models)
        models[0]['model'] = 'new-physical-model'
        self.config = replace(self.config, assistant_models=models)
        self.reopen()
        changed = deepcopy(body)
        changed.update(client_request_id='next-step', step_id='next-step')
        self.expect_error('EFFORT_POLICY_MISMATCH', lambda: self.service.quote(self.user, changed, 'changed-route'))
        self.worker.process_once()
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual(final['error']['code'], 'ASSISTANT_ROUTE_CHANGED')
        self.assertEqual(self.fast.calls + self.quality.calls, [])

    def test_provider_preparation_uses_frozen_context_without_mutating_shared_provider(self):
        class BoundedProvider(FakeAssistant):
            def __init__(self, config):
                super().__init__()
                self.config = config

            def prepare(self, request):
                prepared = super().prepare(request)
                if prepared.input_token_upper_bound > self.config.assistant_context_window:
                    raise ValueError('input exceeds provider configuration')
                return prepared

        self.fast = BoundedProvider(self.config)
        self.reopen()
        body = self.selected_body()
        self.config = replace(self.config, assistant_context_window=250)
        self.fast = BoundedProvider(self.config)
        self.reopen()
        self.assertGreater(len(json.dumps(body['request']).encode()), 250)
        operation = self.create(body)
        self.worker.process_once()
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual(final['state'], 'succeeded')
        self.assertEqual(self.fast.config.assistant_context_window, 250)
        self.assertEqual(final['budget']['policy']['agent']['context_window'], 1000)
        self.assertEqual(self.fast.calls, [operation['operation_id']])

    def test_selection_cannot_reuse_existing_budget_with_other_policy(self):
        body = self.selected_body()
        for change in ({'effort_id': 'strong'}, {'policy_revision': 'policy-2'}, {'model_id': 'quality'}):
            self.expect_error('EFFORT_POLICY_MISMATCH', lambda: self.service.quote(self.user, body | change, 'no-rebind'))
        self.expect_error('ASSISTANT_ACTIVE_TURN', lambda: self.selected_body(effort='strong', model='quality', turn='next'))

    def test_concurrent_first_selections_cannot_claim_the_other_policy(self):
        self.config = replace(self.config, efforts=(specification(), specification('other', 'fast', output=150)))
        self.reopen()
        barrier = threading.Barrier(2)
        original = self.service._selected

        def selected(*args, **kwargs):
            result = original(*args, **kwargs)
            barrier.wait(timeout=5)
            return result

        def create(identity):
            body = self.budget_body(effort=identity, key='budget-' + identity)
            try:
                return self.service.turn_budget(self.user, body, 'header-' + identity)
            except ApiError as error:
                return error.code

        with patch.object(self.service, '_selected', side_effect=selected), ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(create, ('lite', 'other')))
        self.assertEqual(sum(isinstance(value, dict) for value in results), 1)
        self.assertIn('EFFORT_POLICY_MISMATCH', results)
        with self.db.connect() as connection:
            self.assertEqual(connection.execute('SELECT COUNT(*) FROM assistant_turn_budgets').fetchone()[0], 1)

    def test_legacy_budget_and_operation_remain_legacy_after_enabling_efforts(self):
        saved = self.config
        self.config = replace(self.config, efforts=())
        self.reopen()
        body = self.body()
        operation = self.create(body)
        self.config = saved
        self.reopen()
        replay = self.create(body)
        self.assertEqual(replay['operation_id'], operation['operation_id'])
        self.assertIsNone(replay['budget']['effort_id'])
        self.assertIsNone(replay['budget']['policy'])
        self.worker.process_once()
        self.assertEqual(self.provider.calls, [operation['operation_id']])

    def test_configuration_is_bounded_and_requires_supported_references(self):
        valid = specification()
        bad = []
        bad.extend(([valid] * 9, [valid, valid]))
        for key, value in (('id', '../bad'), ('label', 'bad\nlabel'), ('policy_revision', ''), ('extra', 1)):
            bad.append([valid | {key: value}])
        bad.append([valid | {'agent': valid['agent'] | {'max_steps': True}}])
        bad.append([valid | {'generation': {'image_model_ids': [], 'video_model_ids': []}}])
        for value in bad:
            with self.assertRaises(ValueError):
                parse_efforts(json.dumps(value))
        for value in (valid | {'agent': valid['agent'] | {'model_id': 'missing'}},
                      valid | {'agent': valid['agent'] | {'max_output_tokens': 201}},
                      valid | {'generation': {'image_model_ids': ['unknown-image'], 'video_model_ids': []}}):
            with self.assertRaises(ValueError):
                validate_efforts(replace(self.config, efforts=(value,)))
        with patch.dict(os.environ, {'SECUT_ENV': 'test', 'SECUT_EFFORTS_JSON': '[{}]'}, clear=True):
            with self.assertRaises(ConfigError):
                Config.from_env()

    def test_migration_preserves_legacy_money_and_null_policy(self):
        schema = (Path(__file__).parents[1] / 'schema.sql').read_text()
        schema = schema.replace(' effort_id TEXT, policy_revision TEXT, policy_json TEXT, route_fingerprint TEXT,\n', '')
        schema = schema.replace('    effort_policy_fingerprint TEXT,\n', '')
        schema_path = Path(self.temp.name) / 'before-efforts.sql'
        schema_path.write_text(schema)
        old = Database(Path(self.temp.name) / 'old.db', schema_path)
        old.initialize()
        # initialize already migrates; construct the actual historical shape.
        with old.connect() as connection:
            connection.execute('ALTER TABLE generation_quotes DROP COLUMN effort_policy_fingerprint')
            for name in ('effort_id', 'policy_revision', 'policy_json', 'route_fingerprint'):
                connection.execute('ALTER TABLE assistant_turn_budgets DROP COLUMN ' + name)
        with self.db.connect() as connection:
            rows = [(table, dict(connection.execute(f'SELECT * FROM {table} WHERE ' +
                    ('id=?' if table == 'users' else 'user_id=?'), (self.user,)).fetchone())) for table in ('users', 'wallets')]
        with old.transaction(immediate=True) as connection:
            for table, row in rows:
                connection.execute(f"INSERT INTO {table} ({','.join(row)}) VALUES ({','.join('?' for _ in row)})", tuple(row.values()))
            connection.execute('UPDATE wallets SET available_credits=94,held_credits=4 WHERE user_id=?', (self.user,))
            connection.execute("INSERT INTO generation_quotes (id,user_id,kind,model,canonical_json,billing_key,credits,expires_at,created_at) VALUES ('legacy-quote',?,'image','old-model','{}','old-key',7,9999999999,1)", (self.user,))
            connection.execute("INSERT INTO assistant_turn_budgets (id,user_id,local_session_id,turn_id,max_credits,held_credits,spent_credits,operation_count,max_steps,state,created_at,expires_at) VALUES ('legacy',?,'session','turn',12,4,2,1,8,'active',1,9999999999)", (self.user,))
        old.schema_path = Path(__file__).parents[1] / 'schema.sql'
        old.initialize()
        old.initialize()
        with old.connect() as connection:
            quote = connection.execute("SELECT * FROM generation_quotes WHERE id='legacy-quote'").fetchone()
            self.assertIsNone(quote['effort_policy_fingerprint'])
            self.assertEqual((quote['canonical_json'], quote['credits']), ('{}', 7))
            row = connection.execute("SELECT * FROM assistant_turn_budgets WHERE id='legacy'").fetchone()
            self.assertEqual((row['effort_id'], row['policy_revision'], row['policy_json'], row['route_fingerprint']), (None,) * 4)
            self.assertEqual((row['model_id'], row['held_credits'], row['spent_credits']), ('default', 4, 2))
            wallet = connection.execute('SELECT * FROM wallets WHERE user_id=?', (self.user,)).fetchone()
            self.assertEqual((wallet['available_credits'], wallet['held_credits']), (94, 4))
