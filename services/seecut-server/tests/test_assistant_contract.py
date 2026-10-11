from __future__ import annotations

import copy
import json
import os
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from seecut_server.assistant_contract import canonical, model_event
from seecut_server.assistant_provider import PreparedInference, ProviderFailure
from seecut_server.assistant_worker import AssistantWorker
from seecut_server.config import Config, ConfigError
from seecut_server.db import Database
from seecut_server.errors import ApiError
from seecut_server.service import SeeCutService
from test_service import FakeEmailSender


def done(tool=False, usage=True):
    message = {'id': 'reply', 'role': 'assistant', 'content': [{'type': 'text', 'text': '已完成'}]}
    if tool:
        message['toolCalls'] = [{'id': 'call-1', 'name': 'context', 'arguments': {}}]
    result = {'type': 'done', 'message': message, 'stopReason': 'tools' if tool else 'stop'}
    if usage:
        result['usage'] = {'input': 100, 'output': 50}
    return result


class FakeAssistant:
    configured = True
    # This synthetic model consumes canonical request bytes: one byte is one token.
    # It is not a tokenizer assertion about a real provider.
    token_contract_id = 'fixture-canonical-byte-v1'

    def prepare(self, request):
        payload = canonical(request).encode('utf-8')
        return PreparedInference(payload, len(payload), self.token_contract_id)

    def __init__(self):
        self.calls = []
        self.events = [{'type': 'text_delta', 'text': '已'}, done()]
        self.failure = None
        self.lookup_result = None
        self.lookups = []
        self.on_stream = None
        self.usage_override = None
        self.prepared = {}

    def stream(self, operation_id, prepared, cancelled):
        self.calls.append(operation_id)
        self.prepared[operation_id] = prepared
        if self.on_stream:
            self.on_stream(operation_id, cancelled)
        for original in self.events:
            event = copy.deepcopy(original)
            if event['type'] == 'done' and 'usage' in event:
                event['usage'] = self.usage_override or {**event['usage'], 'input': prepared.input_token_upper_bound}
            yield event
        if self.failure:
            raise self.failure

    def lookup(self, operation_id):
        self.lookups.append(operation_id)
        return self.lookup_result


class AssistantFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        with patch.dict(os.environ, {'SECUT_ENV': 'test', 'SECUT_EXPOSE_TEST_TOKENS': 'true'}, clear=True):
            config = Config.from_env()
        self.config = replace(config, database_path=root/'test.db', storage_path=root/'storage',
                              assistant_base_url='http://127.0.0.1:1/v1', assistant_model='synthetic-model',
                              assistant_api_key='synthetic-key', assistant_pricing_version='fixture-1',
                              assistant_input_rate=2, assistant_output_rate=5, assistant_context_window=1000,
                              assistant_max_output_tokens=200, assistant_max_turn_credits=10)
        self.clock = [1_700_000_000]
        self.db = Database(self.config.database_path, Path(__file__).resolve().parents[1]/'schema.sql')
        self.db.initialize()
        self.provider = FakeAssistant()
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
                                    clock=lambda: self.clock[0], assistant_provider=self.provider)
        self.service = self.account.assistant
        self.worker = AssistantWorker(self.service)
        self.user, self.token = self.new_user('one@example.test')
        self.other, self.other_token = self.new_user('two@example.test')

    def new_user(self, email):
        registration = self.account.register(email, 'correct-horse-battery')
        self.account.verify_email(email, registration['verification_token'])
        login = self.account.login(email, 'correct-horse-battery')
        user = login['user']['id']
        with self.db.transaction(immediate=True) as connection:
            connection.execute('UPDATE wallets SET available_credits=100 WHERE user_id=?', (user,))
        return user, login['access_token']

    def body(self, step='step-1', budget=None, session='session-1', turn='turn-1'):
        budget = budget or self.service.turn_budget(self.user, {'client_request_id': 'budget-' + turn,
            'local_session_id': session, 'turn_id': turn}, 'budget-' + turn)
        return dict(client_request_id=step, protocol_version=1, local_session_id=session, turn_id=turn, step_id=step,
            budget_id=budget['budget_id'], request={'system': '合成测试', 'messages': [
                {'id': 'input-1', 'role': 'user', 'content': [{'type': 'text', 'text': '检查画布'}]}],
                'tools': [{'name': 'context', 'description': '读取画布', 'parameters': {'type': 'object'}}],
                'maxOutputTokens': 200})

    def create(self, body=None):
        body = body or self.body()
        quote = self.service.quote(self.user, body, 'quote-' + body['client_request_id'])
        return self.service.create(self.user, {**body, 'quote_id': quote['quote_id']}, 'create-' + body['client_request_id'])[0]

    def expect_error(self, code, call):
        with self.assertRaises(ApiError) as caught:
            call()
        self.assertEqual(caught.exception.code, code)
        return caught.exception

    def wallet(self, available, held):
        snapshot = self.account.wallet(self.user)
        self.assertEqual(snapshot['available_credits'], available)
        self.assertEqual(snapshot['held_credits'], held)
        with self.db.connect() as connection:
            rows = list(connection.execute("SELECT * FROM ledger_entries WHERE user_id=? AND reference_type='assistant_operation'", (self.user,)))
        self.assertEqual(100 + sum(row['delta_available'] for row in rows), available)
        self.assertEqual(sum(row['delta_held'] for row in rows), held)
        return rows


class AssistantContractTest(AssistantFixture):
    def test_success_partial_settlement_and_exact_replays(self):
        body = self.body()
        op = self.create(body)
        self.assertEqual(op['reserved_credits'], 3)
        self.wallet(97, 3)
        self.assertEqual(self.worker.process_once(), 1)
        result = self.service.get(self.user, op['operation_id'])
        self.assertEqual((result['state'], result['captured_credits'], result['released_credits']), ('succeeded', 1, 2))
        self.assertEqual(result['budget']['state'], 'active')
        self.service.close_budget(self.user, op['budget_id'], {}, 'turn-complete')
        self.assertEqual(len(self.wallet(99, 0)), 3)
        self.assertEqual(self.create(body)['operation_id'], op['operation_id'])
        self.service.finish(op['operation_id'], 'failed', {'input': 0, 'output': 0})
        self.service.cancel(self.user, op['operation_id'], {}, 'late-cancel')
        self.assertEqual(self.worker.process_once(), 0)
        self.assertEqual(len(self.provider.calls), 1)
        self.assertEqual(len(self.wallet(99, 0)), 3)
        page = self.service.events(self.user, op['operation_id'], 0)
        self.assertEqual([event['seq'] for event in page['events']], list(range(1, page['next_cursor'] + 1)))
        types = [event['type'] for event in page['events']]
        self.assertLess(types.index('billing.changed'), next(index for index, event in enumerate(page['events']) if event['type'] == 'model' and event['payload']['type'] == 'done'))
        self.assertEqual(self.service.events(self.user, op['operation_id'], page['next_cursor'])['events'], [])

    def test_concurrent_duplicate_creates_one_hold_one_dispatch(self):
        body = self.body()
        quote = self.service.quote(self.user, body, 'quote')
        request = {**body, 'quote_id': quote['quote_id']}
        with ThreadPoolExecutor(max_workers=8) as pool:
            responses = list(pool.map(lambda _: self.service.create(self.user, request, 'create'), range(16)))
        self.assertEqual(len({op['operation_id'] for op, created in responses}), 1)
        self.assertEqual(sum(created for op, created in responses), 1)
        self.assertEqual(len(self.wallet(97, 3)), 1)
        with ThreadPoolExecutor(max_workers=3) as pool:
            list(pool.map(lambda _: self.worker.process_once(), range(3)))
        self.assertEqual(len(self.provider.calls), 1)
        self.wallet(99, 0)

    def test_body_identity_and_header_conflicts(self):
        body = self.body()
        op = self.create(body)
        altered = copy.deepcopy(body)
        altered['request']['system'] += 'changed'
        self.expect_error('ASSISTANT_IDEMPOTENCY_CONFLICT', lambda: self.service.create(self.user, {**altered, 'quote_id': 'unused'}, 'new-header'))
        altered['client_request_id'] = 'new-request'
        self.expect_error('ASSISTANT_IDEMPOTENCY_CONFLICT', lambda: self.service.create(self.user, {**altered, 'quote_id': 'unused'}, 'create-step-1'))
        self.assertEqual(self.service.by_request(self.user, 'step-1')['operation_id'], op['operation_id'])

    def test_queued_cancel_releases_exactly_once(self):
        op = self.create()
        for key in ('cancel', 'cancel', 'cancel-new'):
            result = self.service.cancel(self.user, op['operation_id'], {}, key)
            self.assertEqual(result['state'], 'cancelled')
        self.assertEqual(len(self.wallet(100, 0)), 2)
        self.assertEqual(self.worker.process_once(), 0)
        self.assertEqual(self.provider.calls, [])

    def test_running_cancel_does_not_falsely_claim_provider_cancellation(self):
        op = self.create()
        self.provider.on_stream = lambda operation, cancelled: self.service.cancel(self.user, operation, {}, 'cancel-running')
        self.worker.process_once()
        result = self.service.get(self.user, op['operation_id'])
        self.assertTrue(result['cancel_requested'])
        self.assertEqual(result['state'], 'succeeded')
        self.wallet(99, 0)

    def test_confirmed_failure_and_partial_cancel_charge_actual_usage(self):
        self.provider.events = []
        self.provider.failure = ProviderFailure(usage={'input': 100, 'output': 0}, cancelled=True)
        op = self.create()
        self.worker.process_once()
        result = self.service.get(self.user, op['operation_id'])
        self.assertEqual((result['state'], result['captured_credits']), ('cancelled', 1))
        self.wallet(99, 0)

    def test_known_not_executed_failure_releases(self):
        self.provider.events = []
        self.provider.failure = ProviderFailure(known_not_executed=True)
        op = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, op['operation_id'])['state'], 'failed')
        self.wallet(100, 0)

    def test_unknown_blocks_new_turns_and_never_replays(self):
        self.provider.events = [{'type': 'text_delta', 'text': 'partial'}]
        self.provider.failure = RuntimeError('synthetic-secret-must-not-leak')
        op = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, op['operation_id'])['state'], 'outcome_unknown')
        self.clock[0] += 300
        self.expect_error('ASSISTANT_ACTIVE_TURN', lambda: self.body('step-new', session='changed', turn='changed'))
        for _ in range(2):
            self.worker.process_once()
            self.service.recover_interrupted()
            result = self.service.reconcile(self.user, op['operation_id'], {}, 'lookup')
        self.assertEqual(len(self.provider.calls), 1)
        self.assertNotIn('synthetic-secret', json.dumps(result))
        self.wallet(97, 3)
        self.provider.lookup_result = dict(state='succeeded', usage={'input': 100, 'output': 50}, result=done())
        self.service.reconcile(self.user, op['operation_id'], {}, 'lookup')
        self.assertEqual(len(self.provider.calls), 1)
        self.wallet(99, 0)

    def test_worker_restart_claimed_row_is_unknown_and_not_redispatched(self):
        op = self.create()
        self.service.claim()
        self.assertEqual(self.service.recover_interrupted(), 1)
        self.assertEqual(self.worker.process_once(), 0)
        self.assertEqual(self.provider.calls, [])
        self.assertEqual(self.service.get(self.user, op['operation_id'])['state'], 'outcome_unknown')
        self.wallet(97, 3)

    def test_missing_usage_does_not_publish_done_or_release(self):
        self.provider.events = [done(usage=False)]
        op = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, op['operation_id'])['billing_state'], 'settlement_pending')
        page = self.service.events(self.user, op['operation_id'], 0)
        self.assertFalse(any(e['type'] == 'model' and e['payload']['type'] == 'done' for e in page['events']))
        self.wallet(97, 3)

    def test_usage_outside_frozen_bounds_stays_held(self):
        event = done()
        self.provider.usage_override = {'input': 1001, 'output': 50}
        self.provider.events = [event]
        op = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, op['operation_id'])['billing_state'], 'settlement_pending')
        self.wallet(97, 3)

    def test_fixed_multi_step_budget_and_step_limit(self):
        self.service.config = replace(self.config, assistant_max_turn_credits=4, assistant_max_steps=2)
        body = self.body()
        self.provider.events = [done(tool=True)]
        first = self.create(body)
        self.worker.process_once()
        self.expect_error('ASSISTANT_ACTIVE_TURN', lambda: self.body('new', session='new', turn='new'))
        second_body = self.body('step-2')
        self.create(second_body)
        self.worker.process_once()
        self.expect_error('ASSISTANT_BUDGET_EXCEEDED', lambda: self.create(self.body('step-3')))
        self.assertEqual(self.service.get(self.user, first['operation_id'])['budget']['spent_credits'], 2)
        self.wallet(98, 0)

    def test_active_tool_turn_survives_approval_delay_and_explicit_cancel_closes(self):
        body = self.body()
        self.provider.events = [done(tool=True)]
        op = self.create(body)
        self.worker.process_once()
        self.clock[0] += 600
        self.create(self.body('step-after-approval'))
        self.worker.process_once()
        last = self.service.by_request(self.user, 'step-after-approval')
        self.service.cancel(self.user, last['operation_id'], {}, 'stop-turn')
        self.assertEqual(self.service.get(self.user, op['operation_id'])['budget']['state'], 'closed')
        self.body('next-step', session='session-2', turn='turn-2')
        self.wallet(98, 0)

    def test_compaction_stop_and_inference_share_budget_until_real_turn_completion(self):
        first = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user, first['operation_id'])['budget']['state'], 'active')
        self.create(self.body('after-compaction'))
        self.expect_error('ASSISTANT_UNRESOLVED_OPERATION', lambda: self.service.close_budget(self.user, first['budget_id'], {}, 'close'))
        self.worker.process_once()
        self.expect_error('ASSISTANT_BUDGET_NOT_FOUND', lambda: self.service.close_budget(self.other, first['budget_id'], {}, 'close'))
        closed = self.service.close_budget(self.user, first['budget_id'], {}, 'close')
        self.assertEqual(closed['spent_credits'], 2)
        self.assertEqual(closed['state'], 'closed')
        self.service.close_budget(self.user, first['budget_id'], {}, 'close')
        self.body('new-step', session='session-2', turn='turn-2')
        self.wallet(98, 0)

    def test_parallel_steps_cannot_exceed_active_operation_or_budget(self):
        one, two = self.body('one'), self.body('two')
        quotes = [self.service.quote(self.user, body, 'q-' + body['step_id']) for body in (one, two)]
        def submit(index):
            try:
                return self.service.create(self.user, {**(one, two)[index], 'quote_id': quotes[index]['quote_id']}, 'c-' + str(index))[0]
            except ApiError as exc:
                return exc.code
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(submit, range(2)))
        self.assertEqual(sum(isinstance(r, dict) for r in results), 1)
        self.assertIn('ASSISTANT_UNRESOLVED_OPERATION', results)
        self.wallet(97, 3)

    def test_expired_unused_turn_can_be_replaced_but_not_reactivated(self):
        original = self.body()
        self.clock[0] += 121
        self.body('new-step', session='session-2', turn='turn-2')
        self.expect_error('ASSISTANT_BUDGET_EXCEEDED', lambda: self.service.quote(self.user, original, 'old'))

    def test_foreign_operation_and_budget_are_not_disclosed(self):
        op = self.create()
        for call in (lambda: self.service.get(self.other, op['operation_id']),
                     lambda: self.service.events(self.other, op['operation_id'], 0),
                     lambda: self.service.cancel(self.other, op['operation_id'], {}, 'cancel'),
                     lambda: self.service.by_request(self.other, 'step-1'),
                     lambda: self.service.reconcile(self.other, op['operation_id'], {}, 'reconcile')):
            self.assertEqual(self.expect_error('ASSISTANT_OPERATION_NOT_FOUND', call).status, 404)

    def test_quote_freezes_price_and_route_even_if_version_reused(self):
        body = self.body()
        quote = self.service.quote(self.user, body, 'q')
        original_config = self.service.config
        for change in ({'assistant_input_rate': 3}, {'assistant_model': 'another'}, {'assistant_base_url': 'http://127.0.0.1:2/v1'}, {'assistant_context_window': 999}):
            self.service.config = replace(original_config, **change)
            self.expect_error('ASSISTANT_QUOTE_MISMATCH', lambda: self.service.create(self.user, {**body, 'quote_id': quote['quote_id']}, 'create'))
        self.wallet(100, 0)

    def test_queued_route_change_fails_before_network(self):
        op = self.create()
        self.service.config = replace(self.config, assistant_model='changed')
        self.worker.process_once()
        self.assertEqual(self.provider.calls, [])
        self.assertEqual(self.service.get(self.user, op['operation_id'])['error']['code'], 'ASSISTANT_ROUTE_CHANGED')
        self.wallet(100, 0)

    def test_cursor_missing_prefix_or_middle_returns_snapshot_without_execution(self):
        op = self.create()
        self.worker.process_once()
        with self.db.transaction(immediate=True) as connection:
            connection.execute('DELETE FROM assistant_events WHERE operation_id=? AND seq IN (1,4)', (op['operation_id'],))
        for after in (0, 2):
            error = self.expect_error('ASSISTANT_CURSOR_EXPIRED', lambda: self.service.events(self.user, op['operation_id'], after))
            self.assertEqual(error.details['snapshot']['state'], 'succeeded')
        self.assertEqual(len(self.provider.calls), 1)
        self.wallet(99, 0)

    def test_unknown_reconcile_does_not_query_changed_route(self):
        op = self.create()
        self.service.claim()
        self.service.recover_interrupted()
        self.service.config = replace(self.config, assistant_model='changed')
        self.service.reconcile(self.user, op['operation_id'], {}, 'lookup')
        self.assertEqual(self.provider.lookups, [])
        self.wallet(97, 3)

    def test_reject_bad_types_unknown_fields_and_nonfinite_schema(self):
        body = self.body()
        for mutation in (lambda b: b['request']['messages'][0].update(role=[]),
                         lambda b: b['request']['tools'][0].update(name={}),
                         lambda b: b['request'].update(providerData={}),
                         lambda b: b['request']['tools'][0]['parameters'].update(value=float('nan')),
                         lambda b: b['request']['messages'][0]['content'].append({'type':'image','mimeType':[], 'data':'aaaa'}),
                         lambda b: b['request'].update(maxOutputTokens=True)):
            altered = copy.deepcopy(body)
            mutation(altered)
            self.expect_error('ASSISTANT_INVALID_REQUEST', lambda: self.service.quote(self.user, altered, 'invalid'))
        event = done()
        event['stopReason'] = []
        self.expect_error('ASSISTANT_INVALID_REQUEST', lambda: model_event(event))
        self.wallet(100, 0)

    def test_unconfigured_service_is_not_free_or_available(self):
        self.service.config = replace(self.config, assistant_max_turn_credits=0)
        self.assertFalse(self.service.capabilities()['available'])
        self.expect_error('ASSISTANT_NOT_CONFIGURED', lambda: self.body())
        self.wallet(100, 0)

    def test_environment_numeric_limits(self):
        for value in ('0', '-1', 'NaN', '1.5', '1000000001'):
            with patch.dict(os.environ, {'SECUT_ASSISTANT_MAX_TURN_CREDITS': value}, clear=True):
                with self.assertRaises(ConfigError):
                    Config.from_env()
