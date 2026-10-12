"""Read-only recovery of a lost image submission response, using local fakes only."""
from __future__ import annotations

import http.client
import json
import threading
from dataclasses import replace
from http.server import ThreadingHTTPServer
from unittest.mock import patch

from seecut_server.http import SeeCutHandler
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture
from test_generation_robustness import FakeImage2, FakeXiangxin
from test_service import FakeEmailSender


MODEL = 'gpt-image-2.5-flare'
ROUTE = '/api/generation/tasks/by-request'


class GenerationRequestRecoveryTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        self.config = replace(self.config, model_prices={'image:' + MODEL: 7})
        self.image2 = FakeImage2()
        self.xiangxin = FakeXiangxin()
        self.reopen()
        handler = type('QuietHandler', (SeeCutHandler,), {
            'service': self.account, 'log_message': lambda *args: None,
        })
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
        self.thread = threading.Thread(
            target=self.server.serve_forever, kwargs={'poll_interval': 0.02}, daemon=True,
        )
        self.thread.start()
        self.addCleanup(self.stop_server)

    def reopen(self):
        self.account = SeeCutService(
            self.config, self.db, emailer=FakeEmailSender(self.config),
            image2=self.image2, xiangxin=self.xiangxin,
            assistant_provider=self.provider, clock=lambda: self.clock[0],
        )

    def stop_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(2)

    def request(self, method, path, *, token=None, key=None, body=None):
        headers = {'Content-Type': 'application/json'}
        if token is not None:
            headers['Authorization'] = 'Bearer ' + token
        if key is not None:
            headers['Idempotency-Key'] = key
        connection = http.client.HTTPConnection(*self.server.server_address, timeout=3)
        try:
            connection.request(method, path, None if body is None else json.dumps(body), headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def submit(self, key='flow-original-key', *, user=None, quantity=1):
        user = user or self.user
        request = {'model': MODEL, 'prompt': 'synthetic image', 'quantity': quantity}
        quote = self.account.quote_generation(user, 'image', request)
        payload = request | {'quote_id': quote['quote_id']}
        return self.account.create_generation_task(user, 'image', payload, key), payload

    def business_snapshot(self):
        with self.db.connect() as connection:
            return {
                name: [tuple(row) for row in connection.execute(f'SELECT * FROM {name} ORDER BY 1')]
                for name in ('generation_tasks', 'generation_quotes', 'generation_outputs',
                             'wallet_holds', 'wallets', 'ledger_entries')
            }

    def test_lost_http_response_recovers_original_identity_without_mutation(self):
        request = {'model': MODEL, 'prompt': 'synthetic image'}
        status, quote = self.request('POST', '/api/generation/quote', token=self.token,
                                     body=request | {'kind': 'image'})
        self.assertEqual(status, 200)
        status, lost = self.request('POST', '/api/generation/images', token=self.token,
                                    key='original', body=request | {'quote_id': quote['quote_id']})
        self.assertEqual(status, 202)
        before = self.business_snapshot()
        for _ in range(3):
            status, recovered = self.request('GET', ROUTE, token=self.token, key='original')
            self.assertEqual(status, 200)
            self.assertEqual(recovered, lost)
        self.assertEqual(self.business_snapshot(), before)
        self.assertEqual(self.image2.calls, 0)
        self.assertEqual(self.account.wallet(self.user)['held_credits'], 7)

    def test_owner_isolation_and_same_key_for_two_accounts(self):
        first, _ = self.submit()
        status, error = self.request('GET', ROUTE, token=self.other_token, key='flow-original-key')
        self.assertEqual((status, error['error']['code']), (404, 'GENERATION_TASK_NOT_FOUND'))
        second, _ = self.submit(user=self.other)
        self.assertNotEqual(first['id'], second['id'])
        before = self.business_snapshot()
        for token, expected in ((self.token, first), (self.other_token, second)):
            status, recovered = self.request('GET', ROUTE, token=token, key='flow-original-key')
            self.assertEqual(status, 200)
            self.assertEqual(recovered, expected)
        self.assertEqual(self.business_snapshot(), before)

    def test_missing_invalid_and_unknown_keys_do_not_create_business_records(self):
        before = self.business_snapshot()
        for key, expected in ((None, 'INVALID_IDEMPOTENCY_KEY'),
                              ('x' * 129, 'INVALID_IDEMPOTENCY_KEY'),
                              ('unknown', 'GENERATION_TASK_NOT_FOUND')):
            status, error = self.request('GET', ROUTE, token=self.token, key=key)
            self.assertEqual(status, 404 if key == 'unknown' else 400)
            self.assertEqual(error['error']['code'], expected)
        self.assertEqual(self.business_snapshot(), before)
        self.assertEqual(self.image2.calls, 0)

    def test_authentication_expiry_and_revocation_before_lookup(self):
        self.submit()
        self.assertEqual(self.request('GET', ROUTE, key='flow-original-key')[0], 401)
        self.account.logout(self.other_token)
        self.assertEqual(self.request('GET', ROUTE, token=self.other_token, key='flow-original-key')[0], 401)
        self.clock[0] += self.config.session_ttl_seconds + 1
        self.assertEqual(self.request('GET', ROUTE, token=self.token, key='flow-original-key')[0], 401)

    def test_unknown_recovers_after_price_policy_and_quote_change_without_dispatch(self):
        original, _ = self.submit()
        with self.db.transaction() as connection:
            connection.execute("UPDATE generation_tasks SET status='pending_reconcile', "
                               "error_code='SUBMISSION_RESULT_UNKNOWN' WHERE id=?", (original['id'],))
        self.config = replace(self.config, image2_model='unavailable-replacement',
                              efforts=(), model_prices={})
        self.image2.configured = False
        self.reopen()
        self.clock[0] += 10000
        before = self.business_snapshot()
        with patch.object(self.account, '_validate_generation_assets', side_effect=AssertionError('revalidated')):
            recovered = self.account.get_generation_task_by_request(self.user, 'flow-original-key')
        self.assertEqual(recovered['id'], original['id'])
        self.assertEqual(recovered['request'], original['request'])
        self.assertEqual(recovered['status'], 'pending_reconcile')
        self.assertEqual(self.business_snapshot(), before)
        self.assertEqual(self.account.claim_generation_tasks(), [])
        self.assertEqual(self.account.wallet(self.user)['held_credits'], 7)
        self.assertEqual(self.image2.calls, 0)

    def test_batch_lookup_preserves_all_tasks_and_order_without_new_holds(self):
        original, _ = self.submit(quantity=2)
        before = self.business_snapshot()
        status, recovered = self.request('GET', ROUTE, token=self.token, key='flow-original-key')
        self.assertEqual(status, 200)
        self.assertEqual(recovered, original)
        self.assertEqual([task['batch_index'] for task in recovered['tasks']], [0, 1])
        self.assertEqual(self.business_snapshot(), before)
        self.assertEqual(self.account.wallet(self.user)['held_credits'], 14)

    def test_lookup_does_not_weaken_existing_same_key_payload_conflict(self):
        original, payload = self.submit()
        before = self.business_snapshot()
        self.account.get_generation_task_by_request(self.user, 'flow-original-key')
        status, error = self.request('POST', '/api/generation/images', token=self.token,
                                    key='flow-original-key', body=payload | {'prompt': 'changed'})
        self.assertEqual((status, error['error']['code']), (409, 'IDEMPOTENCY_CONFLICT'))
        self.assertEqual(self.business_snapshot(), before)
        self.assertEqual(self.account.get_generation_task(self.user, original['id'])['request'], original['request'])
