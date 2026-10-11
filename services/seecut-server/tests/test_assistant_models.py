from __future__ import annotations

import json
import os
import threading
from http.server import ThreadingHTTPServer

import test_assistant_http as _http
from seecut_server.http import SeeCutHandler
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from seecut_server.assistant_models import parse_models, model_config
from seecut_server.assistant_provider import ProviderFailure
from seecut_server.assistant_worker import AssistantWorker
from seecut_server.config import Config, ConfigError
from seecut_server.db import Database
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture, FakeAssistant, done
from test_service import FakeEmailSender


class AssistantModelTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        self.configure_catalog()

    def configure_catalog(self):
        catalog = [
            dict(id='fast', label='快捷模型', model='synthetic-fast', input_rate=2,
                 output_rate=3, max_turn_credits=12),
            dict(id='quality', label='质量模型', model='synthetic-quality', input_rate=9,
                 output_rate=12, max_turn_credits=20),
        ]
        self.config = replace(self.config, assistant_models=parse_models(json.dumps(catalog)))
        self.fast, self.quality = FakeAssistant(), FakeAssistant()
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
                                    clock=lambda: self.clock[0], assistant_provider=self.provider,
                                    assistant_providers={'fast': self.fast, 'quality': self.quality})
        self.service = self.account.assistant
        self.worker = AssistantWorker(self.service)

    def selected_body(self, model='fast', turn='turn-1', step='step-1'):
        budget = self.service.turn_budget(self.user, dict(model_id=model,
            client_request_id='budget-' + turn, local_session_id='session-1', turn_id=turn),
            'budget-' + turn)
        return self.body(step, budget, turn=turn) | {'model_id': model}

    def test_selected_model_owns_its_budget_dispatch_and_shared_wallet(self):
        body = self.selected_body()
        operation = self.create(body)
        self.assertEqual(operation['budget']['model_id'], 'fast')
        self.assertEqual(operation['budget']['max_credits'], 12)
        self.worker.process_once()
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual(self.fast.calls, [operation['operation_id']])
        self.assertEqual(self.provider.calls + self.quality.calls, [])
        self.assertEqual(final['state'], 'succeeded')
        self.wallet(100 - final['captured_credits'], 0)
        self.service.close_budget(self.user, body['budget_id'], {}, 'close-fast')
        quality = self.create(self.selected_body('quality', 'turn-2', 'step-2'))
        self.worker.process_once()
        self.assertEqual(self.quality.calls, [quality['operation_id']])
        self.assertEqual(self.fast.calls, [operation['operation_id']])
        self.assertEqual(self.service.get(self.user, quality['operation_id'])['budget']['model_id'], 'quality')

    def test_switching_model_cannot_reuse_a_budget_or_evade_an_active_turn(self):
        body = self.selected_body()
        self.expect_error('ASSISTANT_SCOPE_MISMATCH', lambda: self.service.quote(
            self.user, body | {'model_id': 'quality'}, 'wrong-model'))
        self.expect_error('ASSISTANT_ACTIVE_TURN', lambda: self.selected_body('quality', 'turn-2'))
        self.expect_error('ASSISTANT_IDEMPOTENCY_CONFLICT', lambda: self.service.turn_budget(
            self.user, dict(model_id='quality', client_request_id='budget-turn-1',
                            local_session_id='session-1', turn_id='turn-1'), 'budget-turn-1'))

    def test_quote_and_create_identity_include_the_selected_alias(self):
        body = self.selected_body()
        quote = self.service.quote(self.user, body, 'shared-key')
        self.expect_error('ASSISTANT_IDEMPOTENCY_CONFLICT', lambda: self.service.quote(
            self.user, body | {'model_id': 'quality'}, 'shared-key'))
        operation, created = self.service.create(self.user, body | {'quote_id': quote['quote_id']}, 'create')
        self.assertTrue(created)
        self.expect_error('ASSISTANT_IDEMPOTENCY_CONFLICT', lambda: self.service.create(
            self.user, body | {'model_id': 'quality', 'quote_id': quote['quote_id']}, 'create'))
        replay, created = self.service.create(self.user, body | {'quote_id': quote['quote_id']}, 'create')
        self.assertFalse(created)
        self.assertEqual(replay['operation_id'], operation['operation_id'])
        self.assertEqual(self.service.by_request(self.user, body['client_request_id'])['budget']['model_id'], 'fast')

    def test_unknown_reconciliation_uses_the_original_model(self):
        self.fast.failure = ProviderFailure()
        operation = self.create(self.selected_body())
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, operation['operation_id'])['state'], 'outcome_unknown')
        self.fast.lookup_result = dict(state='succeeded', usage={'input': 100, 'output': 50}, result=done())
        self.service.reconcile(self.user, operation['operation_id'], {}, 'original')
        self.assertEqual(self.fast.lookups, [operation['operation_id']])
        self.assertEqual(self.quality.lookups + self.provider.lookups, [])
        self.assertEqual(len(self.fast.calls), 1)
        self.assertEqual(self.service.get(self.user, operation['operation_id'])['billing_state'], 'captured')

    def test_removed_model_can_be_read_and_cancelled_but_never_redirected(self):
        operation = self.create(self.selected_body())
        del self.service.models['fast']
        self.assertEqual(self.service.get(self.user, operation['operation_id'])['budget']['model_id'], 'fast')
        self.expect_error('ASSISTANT_MODEL_UNAVAILABLE', lambda: self.service.reconcile(
            self.user, operation['operation_id'], {}, 'wrong-provider'))
        self.worker.process_once()
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual(final['state'], 'failed')
        self.assertEqual(final['error']['code'], 'ASSISTANT_ROUTE_CHANGED')
        self.assertEqual(self.fast.calls + self.provider.calls + self.quality.calls, [])
        self.wallet(100, 0)

    def test_removed_running_model_stays_held_until_original_route_is_restored(self):
        self.fast.failure = ProviderFailure()
        operation = self.create(self.selected_body())
        self.worker.process_once()
        original = self.service.models.pop('fast')
        cancelled = self.service.cancel(self.user, operation['operation_id'], {}, 'cancel')
        self.assertEqual(cancelled['state'], 'outcome_unknown')
        self.assertEqual(cancelled['billing_state'], 'held')
        self.expect_error('ASSISTANT_MODEL_UNAVAILABLE', lambda: self.service.reconcile(
            self.user, operation['operation_id'], {}, 'no-route'))
        self.service.models['fast'] = original
        self.fast.lookup_result = dict(state='succeeded', usage={'input': 100, 'output': 50}, result=done())
        self.service.reconcile(self.user, operation['operation_id'], {}, 'restored')
        self.assertEqual(len(self.fast.calls), 1)
        self.assertEqual(self.fast.lookups, [operation['operation_id']])

    def test_other_account_cannot_query_or_cancel_model_operations(self):
        operation = self.create(self.selected_body())
        self.expect_error('ASSISTANT_OPERATION_NOT_FOUND', lambda: self.service.get(self.other, operation['operation_id']))
        self.expect_error('ASSISTANT_OPERATION_NOT_FOUND', lambda: self.service.cancel(self.other, operation['operation_id'], {}, 'cross-user'))

    def test_capabilities_expose_aliases_limits_and_availability_only(self):
        value = self.service.capabilities()
        self.assertEqual(value['default_model_id'], 'default')
        models = {item['model_id']: item for item in value['models']}
        self.assertEqual(models['fast']['label'], '快捷模型')
        self.assertEqual(models['quality']['max_turn_credits'], 20)
        self.assertTrue(models['fast']['available'])
        self.assertNotIn('synthetic-key', json.dumps(value))
        self.assertNotIn('base_url', json.dumps(value))
        self.assertNotIn('synthetic-quality', json.dumps(value))

    def test_unknown_alias_and_client_routing_overrides_are_rejected(self):
        self.expect_error('ASSISTANT_MODEL_UNAVAILABLE', lambda: self.selected_body('missing'))
        body = self.selected_body()
        self.expect_error('ASSISTANT_INVALID_REQUEST', lambda: self.service.quote(
            self.user, body | {'base_url': 'http://127.0.0.1:1'}, 'routing'))

    def test_catalog_configuration_rejects_unknown_fields_and_bad_limits(self):
        bad = [dict(id='fast', label='快捷', model='fixture', api_key='forbidden')]
        for value in [bad, bad + bad, [dict(id='default', label='快捷', model='fixture')],
                      [dict(id='fast', label='快捷', model='fixture', max_steps=True)],
                      [dict(id='fast', label='快捷', model='fixture', max_output_tokens=131073)],
                      [dict(id='fast', label='快捷', model='fixture')] * 2]:
            with self.subTest(value=value), patch.dict(os.environ,
                    {'SECUT_ENV': 'test', 'SECUT_ASSISTANT_MODELS_JSON': json.dumps(value)}, clear=True):
                with self.assertRaisesRegex(ConfigError, 'Invalid Agent model catalog'):
                    Config.from_env()

    def test_model_output_limit_leaves_room_for_real_input(self):
        specification = dict(id='fast', label='快捷', model='fixture', context_window=64)
        for limit in (64, 65):
            with self.subTest(limit=limit), self.assertRaisesRegex(ValueError, 'Invalid Agent model limits'):
                model_config(self.config, specification | {'max_output_tokens': limit})
        self.assertEqual(model_config(self.config, specification | {'max_output_tokens': 63}).assistant_max_output_tokens, 63)

    def test_configuration_alone_does_not_enable_unverified_models(self):
        service = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config)).assistant
        for model in service.capabilities()['models']:
            self.assertFalse(model['available'])
            self.assertEqual(model['reason_code'], 'ASSISTANT_INPUT_CONTRACT_UNAVAILABLE')
        self.expect_error('ASSISTANT_NOT_CONFIGURED', lambda: service.turn_budget(
            self.user, dict(model_id='fast', client_request_id='not-dispatched',
                            local_session_id='session', turn_id='turn'), 'not-dispatched'))
        self.assertEqual(self.fast.calls + self.quality.calls + self.provider.calls, [])

    def test_existing_budget_migration_preserves_model_and_money(self):
        old_schema = (Path(__file__).parents[1] / 'schema.sql').read_text().replace(
            " model_id TEXT NOT NULL DEFAULT 'default',\n", '')
        path = Path(self.temp.name) / 'old-schema.sql'
        path.write_text(old_schema)
        old = Database(Path(self.temp.name) / 'old.db', path)
        with old.connect() as connection:
            connection.executescript(old_schema)
        with self.db.connect() as connection:
            user = dict(connection.execute('SELECT * FROM users WHERE id=?', (self.user,)).fetchone())
            wallet = dict(connection.execute('SELECT * FROM wallets WHERE user_id=?', (self.user,)).fetchone())
        with old.transaction(immediate=True) as connection:
            for table, row in [('users', user), ('wallets', wallet)]:
                columns = ','.join(row)
                parameters = ','.join('?' for _ in row)
                connection.execute(f'INSERT INTO {table} ({columns}) VALUES ({parameters})', tuple(row.values()))
            connection.execute('UPDATE wallets SET available_credits=94,held_credits=4 WHERE user_id=?', (self.user,))
            connection.execute("INSERT INTO assistant_turn_budgets (id,user_id,local_session_id,turn_id,max_credits,held_credits,spent_credits,operation_count,max_steps,state,created_at,expires_at) VALUES ('legacy',?,'session','turn',12,4,2,1,8,'active',1,9999999999)", (self.user,))
        with old.connect() as connection:
            self.assertNotIn('model_id', {row['name'] for row in connection.execute('PRAGMA table_info(assistant_turn_budgets)')})
        old.schema_path = Path(__file__).parents[1] / 'schema.sql'
        old.initialize()
        old.initialize()
        with old.connect() as connection:
            self.assertIn('model_id', {row['name'] for row in connection.execute('PRAGMA table_info(assistant_turn_budgets)')})
        with old.connect() as connection:
            budget = connection.execute("SELECT * FROM assistant_turn_budgets WHERE id='legacy'").fetchone()
            self.assertEqual((budget['model_id'], budget['held_credits'], budget['spent_credits']), ('default', 4, 2))
            wallet = connection.execute('SELECT * FROM wallets WHERE user_id=?', (self.user,)).fetchone()
            self.assertEqual((wallet['available_credits'], wallet['held_credits']), (94, 4))


class AssistantModelHTTPTest(AssistantFixture):
    request = _http.AssistantHTTPTest.request
    stop_server = _http.AssistantHTTPTest.stop_server
    selected_body = AssistantModelTest.selected_body

    def setUp(self):
        super().setUp()
        AssistantModelTest.configure_catalog(self)
        handler = type('QuietModelHandler', (SeeCutHandler,), {'service': self.account, 'log_message': lambda *args: None})
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={'poll_interval': 0.02}, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_server)

    def test_http_selection_dispatch_and_original_request_recovery(self):
        status, budget = self.request('POST', '/api/assistant/turn-budgets',
            dict(model_id='quality', client_request_id='selected-budget', local_session_id='session-1', turn_id='turn-1'),
            token=self.token, key='selected-budget')
        self.assertEqual(status, 200)
        self.assertEqual(budget['model_id'], 'quality')
        body = self.body(budget=budget) | {'model_id': 'quality'}
        status, quote = self.request('POST', '/api/assistant/quotes', body, token=self.token, key='selected-quote')
        self.assertEqual(status, 200)
        status, operation = self.request('POST', '/api/assistant/operations', body | {'quote_id': quote['quote_id']},
                                        token=self.token, key='selected-create')
        self.assertEqual(status, 202)
        self.worker.process_once()
        status, recovered = self.request('GET', '/api/assistant/operations/by-request/step-1', token=self.token)
        self.assertEqual(status, 200)
        self.assertEqual(recovered['operation_id'], operation['operation_id'])
        self.assertEqual(recovered['budget']['model_id'], 'quality')
        self.assertEqual(self.quality.calls, [operation['operation_id']])
        self.assertEqual(self.fast.calls + self.provider.calls, [])

    def test_http_model_swap_and_cross_account_access_are_rejected(self):
        body = self.selected_body()
        status, error = self.request('POST', '/api/assistant/quotes', body | {'model_id': 'quality'},
                                     token=self.token, key='wrong-selection')
        self.assertEqual((status, error['error']['code']), (409, 'ASSISTANT_SCOPE_MISMATCH'))
        status, _ = self.request('POST', '/api/assistant/turn-budgets/' + body['budget_id'] + '/close', {},
                                 token=self.other_token, key='cross-account')
        self.assertEqual(status, 404)
        self.wallet(100, 0)

    def test_http_catalog_requires_current_account_and_exposes_selected_limits(self):
        self.assertEqual(self.request('GET', '/api/assistant/capabilities')[0], 401)
        status, catalog = self.request('GET', '/api/assistant/capabilities', token=self.token)
        self.assertEqual(status, 200)
        self.assertEqual({model['model_id'] for model in catalog['models']}, {'default', 'fast', 'quality'})
        self.request('POST', '/api/auth/logout', {}, token=self.token)
        self.assertEqual(self.request('GET', '/api/assistant/capabilities', token=self.token)[0], 401)
