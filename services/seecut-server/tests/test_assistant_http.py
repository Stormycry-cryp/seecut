from __future__ import annotations

import copy
import http.client
import json
import threading
import time
import unittest
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from seecut_server.assistant_provider import AssistantProvider, PreparedInference
from seecut_server.assistant_contract import canonical
from seecut_server.http import SeeCutHandler
from test_assistant_contract import AssistantFixture


class AssistantHTTPTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        handler = type('QuietSeeCutHandler', (SeeCutHandler,), {'service': self.account, 'log_message': lambda *a: None})
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={'poll_interval': 0.02}, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_server)

    def stop_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(2)

    def request(self, method, path, body=None, token=None, key=None):
        headers = {'Content-Type': 'application/json'}
        if token is not None:
            headers['Authorization'] = 'Bearer ' + token
        if key is not None:
            headers['Idempotency-Key'] = key
        connection = http.client.HTTPConnection(*self.server.server_address, timeout=3)
        try:
            connection.request(method, path, None if body is None else json.dumps(body), headers)
            response = connection.getresponse()
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
        finally:
            connection.close()

    def test_real_login_authentication_expiry_revocation_and_owner(self):
        path = '/api/assistant/capabilities'
        self.assertEqual(self.request('GET', path)[0], 401)
        status, login = self.request('POST', '/api/auth/login', {'email': 'one@example.test', 'password': 'correct-horse-battery'})
        self.assertEqual(status, 200)
        token = login['access_token']
        self.assertEqual(self.request('GET', path, token=token)[1]['available'], True)
        op = self.create()
        self.assertEqual(self.request('GET', '/api/assistant/operations/' + op['operation_id'], token=self.other_token)[0], 404)
        self.request('POST', '/api/auth/logout', {}, token=token)
        self.assertEqual(self.request('GET', path, token=token)[0], 401)
        self.clock[0] += self.config.session_ttl_seconds + 1
        self.assertEqual(self.request('GET', path, token=self.token)[0], 401)

    def test_http_budget_quote_create_events_and_lost_response_recovery(self):
        status, budget = self.request('POST', '/api/assistant/turn-budgets', {'client_request_id':'b', 'local_session_id':'session-1', 'turn_id':'turn-1'}, token=self.token, key='b')
        self.assertEqual(status, 200)
        body = self.body(budget=budget)
        status, quote = self.request('POST', '/api/assistant/quotes', body, token=self.token, key='q')
        self.assertEqual(status, 200)
        status, ignored = self.request('POST', '/api/assistant/operations', {**body, 'quote_id':quote['quote_id']}, token=self.token, key='c')
        self.assertEqual(status, 202)
        # Client treats acceptance response as lost and reads the original identity only.
        self.worker.process_once()
        status, recovered = self.request('GET', '/api/assistant/operations/by-request/step-1', token=self.token)
        self.assertEqual(status, 200)
        self.assertEqual(recovered['state'], 'succeeded')
        self.assertEqual(recovered['operation_id'], ignored['operation_id'])
        status, page = self.request('GET', '/api/assistant/operations/' + recovered['operation_id'] + '/events?after=0', token=self.token)
        self.assertEqual(status, 200)
        self.assertEqual(page['events'][-1]['type'], 'terminal')
        self.assertEqual(len(self.provider.calls), 1)
        self.wallet(99, 0)

    def test_malformed_types_missing_idempotency_and_client_results_rejected(self):
        body = self.body()
        bad = copy.deepcopy(body)
        bad['request']['messages'][0]['role'] = []
        status, result = self.request('POST', '/api/assistant/quotes', bad, token=self.token, key='q')
        self.assertEqual((status, result['error']['code']), (400, 'ASSISTANT_INVALID_REQUEST'))
        self.assertEqual(self.request('POST', '/api/assistant/quotes', body, token=self.token)[0], 400)
        op = self.create(body)
        status, result = self.request('POST', '/api/assistant/operations/' + op['operation_id'] + '/reconcile', {'state':'succeeded','cost':0}, token=self.token, key='r')
        self.assertEqual(status, 400)
        self.assertEqual(self.request('GET', '/api/assistant/operations/' + op['operation_id'] + '/events?after=bad', token=self.token)[0], 400)
        self.wallet(97, 3)

    def test_every_assistant_endpoint_reauthenticates(self):
        body = self.body()
        op = self.create(body)
        base = '/api/assistant/operations/' + op['operation_id']
        routes = [('GET', '/api/assistant/capabilities'), ('POST','/api/assistant/turn-budgets'),
                  ('POST','/api/assistant/quotes'), ('POST','/api/assistant/operations'),
                  ('POST','/api/assistant/turn-budgets/'+op['budget_id']+'/close'),
                  ('GET',base), ('GET',base+'/events'), ('POST',base+'/cancel'), ('POST',base+'/reconcile'),
                  ('GET','/api/assistant/operations/by-request/step-1')]
        self.account.logout(self.token)
        for method, path in routes:
            self.assertEqual(self.request(method, path, {} if method=='POST' else None, token=self.token, key='k')[0], 401)


class SyntheticNetworkProvider(AssistantProvider):
    token_contract_id = 'fixture-chat-json-byte-v1'

    def prepare(self, request):
        payload = self.payload(request)
        return PreparedInference(payload, len(payload), self.token_contract_id)


class AssistantNetworkProviderTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        self.received = []
        self.mode = 'success'
        fixture = self

        class Gateway(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                fixture.received.append((self.path, request, self.headers['Idempotency-Key']))
                if fixture.mode == 'http-failure':
                    self.send_response(429)
                    self.end_headers()
                    self.wfile.write(b'{"error":"synthetic gateway response"}')
                    return
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                try:
                    if fixture.mode == 'drip':
                        for _ in range(100):
                            self.wfile.write(b'x')
                            self.wfile.flush()
                            time.sleep(0.025)
                        return
                    if fixture.mode == 'invalid-tool':
                        delta = {'tool_calls': [{'index':0, 'id':'call-1','function':{'name':'context','arguments':'broken-json'}}]}
                        finish = 'tool_calls'
                    else:
                        delta, finish = {'content':'真实 HTTP 合成流'}, 'stop'
                    chunks = [dict(choices=[dict(index=0, delta=delta, finish_reason=None)])]
                    if fixture.mode != 'disconnect':
                        chunks += [dict(choices=[dict(index=0, delta={}, finish_reason=finish)]),
                                   dict(choices=[], usage={'prompt_tokens':len(canonical(request).encode()), 'completion_tokens':50})]
                    for chunk in chunks:
                        self.wfile.write(('data: '+json.dumps(chunk, ensure_ascii=False)+'\n\n').encode())
                        self.wfile.flush()
                    if fixture.mode != 'disconnect':
                        self.wfile.write(b'data: [DONE]\n\n')
                        self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
        self.gateway = ThreadingHTTPServer(('127.0.0.1',0), Gateway)
        self.gateway_thread = threading.Thread(target=self.gateway.serve_forever, kwargs={'poll_interval':0.02}, daemon=True)
        self.gateway_thread.start()
        self.addCleanup(self.stop_gateway)
        self.service.config = replace(self.config, assistant_base_url=f'http://127.0.0.1:{self.gateway.server_port}/v1', provider_timeout_seconds=1)
        self.service.provider = SyntheticNetworkProvider(self.service.config)

    def stop_gateway(self):
        self.gateway.shutdown()
        self.gateway.server_close()
        self.gateway_thread.join(3)

    def test_real_loopback_stream_and_stable_dispatch_identity(self):
        op = self.create()
        self.worker.process_once()
        result = self.service.get(self.user, op['operation_id'])
        self.assertEqual(result['state'], 'succeeded')
        self.assertEqual(result['result']['message']['content'][0]['text'], '真实 HTTP 合成流')
        self.assertEqual(self.received[0][0], '/v1/chat/completions')
        self.assertEqual(self.received[0][2], op['operation_id'])
        self.assertEqual(self.received[0][1]['model'], 'synthetic-model')
        self.assertEqual(len(self.received),1)
        self.wallet(99,0)

    def test_disconnect_leaves_hold_and_no_new_post(self):
        self.mode = 'disconnect'
        op = self.create()
        self.worker.process_once()
        for _ in range(2):
            self.service.reconcile(self.user,op['operation_id'],{},'lookup')
            self.worker.process_once()
        self.assertEqual(len(self.received),1)
        self.assertEqual(self.service.get(self.user,op['operation_id'])['state'],'outcome_unknown')
        self.wallet(97,3)

    def test_generic_gateway_http_code_does_not_prove_zero_cost(self):
        self.mode = 'http-failure'
        op = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user,op['operation_id'])['state'],'outcome_unknown')
        self.wallet(97,3)

    def test_confirmed_usage_survives_invalid_final_tool_json(self):
        self.mode = 'invalid-tool'
        op = self.create()
        self.worker.process_once()
        self.assertEqual(self.service.get(self.user,op['operation_id'])['state'],'failed')
        self.wallet(99,0)

    def test_dripping_bytes_without_newline_cannot_hold_worker_forever(self):
        self.mode = 'drip'
        op = self.create()
        start = time.monotonic()
        self.worker.process_once()
        self.assertLess(time.monotonic()-start, 1.8)
        self.assertEqual(self.service.get(self.user,op['operation_id'])['state'],'outcome_unknown')
        self.wallet(97,3)
