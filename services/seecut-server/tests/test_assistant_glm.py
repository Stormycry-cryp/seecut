"""Loopback wire/ledger tests; byte counter and keys are synthetic, not GLM proof."""
from __future__ import annotations
import copy
import json
import threading
import time
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from seecut_server.assistant_glm import DOMESTIC_BASE, MODEL, GLMAssistantProvider
from seecut_server.assistant_provider import make_assistant_provider
from seecut_server.assistant_worker import AssistantWorker
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture
from test_service import FakeEmailSender

class GLMWireTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        self.mode, self.requests, self.hook = 'stream', [], None
        fixture = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                fixture.requests.append((self.path, body, self.headers.get('Authorization')))
                if fixture.hook:
                    fixture.hook()
                mode = fixture.mode
                if mode.startswith('error-'):
                    status, code = mode.split('-')[1:]
                    data = json.dumps({'error': {'code': code, 'message': 'synthetic-key must never leak'}}).encode()
                    self.send_response(int(status)); self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
                    return
                usage = {'prompt_tokens': 100, 'completion_tokens': 50, 'total_tokens': 150}
                if mode == 'bad-usage':
                    usage['total_tokens'] = 151
                if mode == 'over-usage':
                    usage = {'prompt_tokens': 100, 'completion_tokens': 201, 'total_tokens': 301}
                if mode in {'json', 'json-tools'}:
                    message = {'role': 'assistant', 'content': '正常响应', 'reasoning_content': 'hidden'}
                    if mode == 'json-tools':
                        message['tool_calls'] = [{'id': 'json-call', 'type': 'function', 'function': {'name': 'context', 'arguments': '{}'}}]
                    data = json.dumps({'choices': [{'index': 0, 'message': message, 'finish_reason': 'tool_calls' if mode == 'json-tools' else 'stop'}], 'usage': usage}).encode()
                    self.send_response(200); self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
                    return
                self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
                if mode == 'timeout':
                    time.sleep(0.15)
                tools = mode in {'tools', 'bad-tool', 'unknown-tool', 'finish-mismatch'}
                if tools:
                    name = 'capabilities' if mode == 'unknown-tool' else 'context'
                    deltas = [dict(reasoning_content='内部思考'), dict(tool_calls=[{'index': 0, 'id': 'call-1',
                        'type': 'function', 'function': {'name': name, 'arguments': '{'}}]),
                        dict(tool_calls=[{'index': 0, 'function': {'arguments': 'invalid}' if mode == 'bad-tool' else '}'}}])]
                else:
                    deltas = [dict(reasoning_content='内部思考'), dict(content='正常'), dict(content='响应')]
                finish = 'tool_calls' if tools else 'stop'
                if mode == 'finish-mismatch':
                    finish = 'stop'
                if mode in {'finish-sensitive', 'finish-network_error', 'finish-model_context_window_exceeded', 'finish-unrecognised'}:
                    finish = mode[len('finish-'):]
                chunks = [{'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]} for delta in deltas]
                if mode != 'partial':
                    chunks += [{'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]}]
                    if mode != 'missing-usage':
                        chunks += [{'choices': [], 'usage': usage}]
                try:
                    for chunk in chunks:
                        self.wfile.write(b'data: ' + json.dumps(chunk, ensure_ascii=False).encode() + b'\n\n')
                        self.wfile.flush()
                    if mode not in {'partial', 'no-done'}:
                        self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
        self.http = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.http.daemon_threads = True
        thread = threading.Thread(target=self.http.serve_forever, kwargs={'poll_interval': 0.02}, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 1); self.addCleanup(self.http.server_close); self.addCleanup(self.http.shutdown)
        self.config = replace(self.config, assistant_base_url=f'http://127.0.0.1:{self.http.server_port}/api/paas/v4',
            assistant_model=MODEL, assistant_context_window=8192, assistant_max_turn_credits=40)
        self.install_provider()

    def install_provider(self):
        self.provider = GLMAssistantProvider(self.config, self.db, input_counter=len, token_contract_id='fixture-glm-byte-v1')
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
            clock=lambda: self.clock[0], assistant_provider=self.provider)
        self.service = self.account.assistant
        self.worker = AssistantWorker(self.service)

    def run_operation(self, mode='stream'):
        self.mode = mode; body = self.body(); op = self.create(body)
        self.assertEqual(op['reserved_credits'], 18)
        self.worker.process_once()
        return body, self.service.get(self.user, op['operation_id'])

    def final_model(self, op):
        events = self.service.events(self.user, op['operation_id'], 0)['events']
        return [event['payload'] for event in events if event['type'] == 'model' and event['payload']['type'] == 'done']

    def test_stream_payload_and_exactly_once_billing(self):
        body, op = self.run_operation()
        self.assertEqual((op['state'], op['captured_credits'], op['released_credits']), ('succeeded', 1, 17))
        path, payload, header = self.requests[0]
        self.assertEqual(path, '/api/paas/v4/chat/completions'); self.assertEqual(header, 'Bearer synthetic-key')
        self.assertEqual(payload['model'], MODEL)
        self.assertEqual(payload['thinking'], {'type': 'enabled', 'clear_thinking': True})
        self.assertTrue(payload['stream']); self.assertTrue(payload['tool_stream']); self.assertNotIn('stream_options', payload)
        self.assertEqual(payload['messages'][1]['content'], '检查画布'); self.assertEqual(len(payload['request_id']), 63)
        frozen = self.service.prepare(body['request'], user=self.user, budget_id=body['budget_id'], client_request_id=body['client_request_id'])
        self.assertEqual(json.loads(frozen.payload), payload); self.assertEqual(frozen.input_token_upper_bound, len(frozen.payload))
        done = self.final_model(op)[0]
        self.assertEqual(done['message']['content'][0]['text'], '正常响应'); self.assertEqual(done['usage'], {'input': 100, 'output': 50})
        self.assertNotIn('内部思考', json.dumps(self.service.events(self.user, op['operation_id'], 0), ensure_ascii=False))
        self.assertEqual(self.create(body)['operation_id'], op['operation_id'])
        self.service.finish(op['operation_id'], 'failed', {'input': 0, 'output': 0})
        self.service.reconcile(self.user, op['operation_id'], {}, 'replay')
        self.assertEqual(self.worker.process_once(), 0); self.assertEqual(len(self.requests), 1)
        self.assertEqual(len(self.wallet(99, 0)), 3)

    def test_json_normal_response(self):
        _, op = self.run_operation('json')
        self.assertEqual(op['state'], 'succeeded'); self.assertEqual(self.final_model(op)[0]['message']['content'][0]['text'], '正常响应')
        self.wallet(99, 0)

    def test_documented_auth_failure_releases_without_expiring_user(self):
        _, op = self.run_operation('error-401-1001')
        self.assertEqual((op['state'], op['billing_state']), ('failed', 'released')); self.wallet(100, 0)
        self.assertNotIn('synthetic-key', json.dumps(op)); self.assertEqual(self.account.authenticate(self.token)['id'], self.user)

    def test_documented_quota_failure_is_zero_cost(self):
        _, op = self.run_operation('error-429-1302')
        self.assertEqual((op['state'], op['captured_credits']), ('failed', 0)); self.wallet(100, 0)

    def assert_unknown(self, op):
        self.assertEqual((op['state'], op['billing_state']), ('outcome_unknown', 'held'))
        self.service.reconcile(self.user, op['operation_id'], {}, 'check'); self.install_provider()
        self.assertEqual(self.worker.process_once(), 0)
        self.service.reconcile(self.user, op['operation_id'], {}, 'check-after-restart')
        self.assertEqual(len(self.requests), 1); self.wallet(82, 18); self.assertFalse(self.final_model(op))

    def test_uncertain_vendor_error_retains_hold_without_resend(self):
        _, op = self.run_operation('error-500-500'); self.assert_unknown(op)

    def test_unrecognised_auth_error_not_assumed_free(self):
        _, op = self.run_operation('error-401-unknown'); self.assert_unknown(op)

    def test_sensitive_error_not_assumed_preinference(self):
        _, op = self.run_operation('error-400-1301'); self.assert_unknown(op)

    def test_partial_stream_unknown(self):
        _, op = self.run_operation('partial'); self.assert_unknown(op)

    def test_missing_final_usage_unknown(self):
        _, op = self.run_operation('missing-usage'); self.assert_unknown(op)

    def test_inconsistent_usage_unknown(self):
        _, op = self.run_operation('bad-usage'); self.assert_unknown(op)

    def test_timeout_unknown_and_no_retry(self):
        self.config = replace(self.config, provider_timeout_seconds=0.05); self.install_provider()
        _, op = self.run_operation('timeout'); self.assert_unknown(op)

    def test_cancel_before_dispatch_costs_zero(self):
        op = self.create(); self.service.cancel(self.user, op['operation_id'], {}, 'stop'); self.worker.process_once()
        self.assertEqual(self.service.get(self.user, op['operation_id'])['state'], 'cancelled'); self.assertEqual(self.requests, [])
        self.wallet(100, 0)

    def test_cancel_after_acceptance_drains_usage_and_suppresses_output(self):
        op = self.create(); self.hook = lambda: self.service.cancel(self.user, op['operation_id'], {}, 'stop')
        self.worker.process_once(); result = self.service.get(self.user, op['operation_id'])
        self.assertEqual((result['state'], result['captured_credits']), ('cancelled', 1))
        self.assertFalse(any(event['type'] == 'model' for event in self.service.events(self.user, op['operation_id'], 0)['events']))
        self.assertEqual(self.worker.process_once(), 0); self.wallet(99, 0)

    def test_invalid_tools_with_complete_usage_settle_failure_once(self):
        _, op = self.run_operation('bad-tool')
        self.assertEqual((op['state'], op['billing_state'], op['captured_credits']), ('failed', 'captured', 1))
        self.assertFalse(self.final_model(op)); self.wallet(99, 0)

    def test_unrequested_tool_with_complete_usage_settles_failure(self):
        _, op = self.run_operation('unknown-tool')
        self.assertEqual((op['state'], op['captured_credits']), ('failed', 1)); self.assertFalse(self.final_model(op))

    def test_tool_finish_mismatch_cannot_execute(self):
        _, op = self.run_operation('finish-mismatch')
        self.assertEqual((op['state'], op['captured_credits']), ('failed', 1)); self.assertFalse(self.final_model(op))

    def test_usage_above_frozen_limits_remains_pending(self):
        _, op = self.run_operation('over-usage')
        self.assertEqual(op['billing_state'], 'settlement_pending'); self.wallet(82, 18); self.assertFalse(self.final_model(op))

    def test_reasoning_persists_server_side_for_same_turn_tool_step(self):
        body, op = self.run_operation('tools'); done = self.final_model(op)[0]; message = done['message']
        self.assertEqual(done['stopReason'], 'tools'); self.assertEqual(message['toolCalls'][0]['arguments'], {})
        self.assertNotIn('reasoning_content', message); self.db.initialize(); self.install_provider()
        next_body = {**body, 'client_request_id': 'step-2', 'step_id': 'step-2', 'request': copy.deepcopy(body['request'])}
        next_body['request']['messages'] += [message, {'id': 'tool-1', 'role': 'tool', 'toolCallId': 'call-1',
            'toolName': 'context', 'content': [{'type': 'text', 'text': '合成画布结果'}]}]
        self.mode = 'stream'; second = self.create(next_body); self.worker.process_once()
        self.assertEqual(self.service.get(self.user, second['operation_id'])['state'], 'succeeded')
        payload = self.requests[1][1]; self.assertEqual(payload['messages'][2]['reasoning_content'], '内部思考')
        self.assertEqual(payload['messages'][2]['content'], ''); self.assertEqual(payload['messages'][3]['tool_call_id'], 'call-1')
        self.wallet(99, 0)  # Both verified steps share one cumulative task rounding.
        for scope in ({'user_id': self.other, 'budget_id': body['budget_id'], 'client_request_id': 'scope'},
                      {'user_id': self.user, 'budget_id': 'other-budget', 'client_request_id': 'scope'}):
            with self.assertRaises(ValueError):
                self.provider.prepare_scoped(next_body['request'], scope)
        changed = copy.deepcopy(next_body['request']); changed['messages'][1]['toolCalls'][0]['arguments'] = {'tampered': True}
        with self.assertRaises(ValueError):
            self.provider.prepare_scoped(changed, dict(user_id=self.user, budget_id=body['budget_id'], client_request_id='tamper'))
        self.assertEqual(len(self.requests), 2)

    def test_real_factory_requires_verified_input_contract(self):
        provider = make_assistant_provider(replace(self.config, env='production', assistant_base_url=DOMESTIC_BASE), self.db)
        self.assertIsInstance(provider, GLMAssistantProvider); self.assertIsNone(provider.token_contract_id)
        account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config), assistant_provider=provider)
        self.assertFalse(account.assistant.capabilities()['available'])
        self.assertEqual(account.assistant.capabilities()['reason_code'], 'ASSISTANT_INPUT_CONTRACT_UNAVAILABLE')
        with self.assertRaises(ValueError):
            provider.prepare_scoped(self.body()['request'], dict(user_id=self.user, budget_id='budget', client_request_id='step'))
        configured = SeeCutService(replace(self.config, env='production', assistant_base_url=DOMESTIC_BASE), self.db, emailer=FakeEmailSender(self.config))
        self.assertIsInstance(configured.assistant.provider, GLMAssistantProvider)
        self.assertFalse(configured.assistant.capabilities()['available'])
        self.assertEqual(self.requests, [])

    def test_only_domestic_or_test_loopback_routes_and_text_enabled(self):
        for update in (dict(env='production'), dict(assistant_base_url='https://api.z.ai/api/paas/v4'),
                       dict(assistant_base_url='https://open.bigmodel.cn/api/coding/paas/v4'),
                       dict(assistant_images=True), dict(assistant_model='glm-5.3-flashx')):
            provider = GLMAssistantProvider(replace(self.config, **update), self.db, input_counter=len, token_contract_id='fixture-byte')
            self.assertFalse(provider.configured)

    def test_json_tool_calls_normalise_indices_and_store_reasoning(self):
        _, op = self.run_operation('json-tools')
        self.assertEqual(op['state'], 'succeeded')
        self.assertEqual(self.final_model(op)[0]['message']['toolCalls'][0]['arguments'], {})
        with self.db.connect() as connection:
            row = connection.execute('SELECT reasoning FROM assistant_glm_messages WHERE operation_id=?', (op['operation_id'],)).fetchone()
        self.assertEqual(row['reasoning'], 'hidden')
        self.wallet(99, 0)

    def test_final_usage_without_done_does_not_prove_complete_response(self):
        _, op = self.run_operation('no-done'); self.assert_unknown(op)

    def test_bounded_response_overflow_does_not_publish_or_refund(self):
        self.config = replace(self.config, max_provider_response_bytes=80); self.install_provider()
        _, op = self.run_operation(); self.assert_unknown(op)

    def test_cancel_after_acceptance_without_usage_retains_unknown(self):
        self.config = replace(self.config, provider_timeout_seconds=0.05); self.install_provider()
        self.mode = 'timeout'; op = self.create()
        self.hook = lambda: self.service.cancel(self.user, op['operation_id'], {}, 'stop')
        self.worker.process_once(); result = self.service.get(self.user, op['operation_id'])
        self.assert_unknown(result)
        self.assertTrue(result['cancel_requested'])

    def assert_confirmed_failure(self, mode):
        _, op = self.run_operation(mode)
        self.assertEqual((op['state'], op['billing_state'], op['captured_credits']), ('failed', 'captured', 1))
        self.assertFalse(self.final_model(op))
        self.service.reconcile(self.user, op['operation_id'], {}, 'verify-final-usage')
        self.assertEqual(self.worker.process_once(), 0)
        self.assertEqual(len(self.requests), 1)
        self.wallet(99, 0)

    def test_network_error_with_complete_final_usage_settles_failure_once(self):
        self.assert_confirmed_failure('finish-network_error')

    def test_sensitive_finish_with_complete_final_usage_is_not_zero_cost(self):
        self.assert_confirmed_failure('finish-sensitive')

    def test_context_error_with_complete_final_usage_settles_failure(self):
        self.assert_confirmed_failure('finish-model_context_window_exceeded')

    def test_unrecognised_finish_is_not_authoritative_success(self):
        _, op = self.run_operation('finish-unrecognised'); self.assert_unknown(op)
