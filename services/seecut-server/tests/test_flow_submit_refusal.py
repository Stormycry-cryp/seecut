"""Known HTTP submit refusals, real local service transactions, synthetic wallet only."""
from test_generation_request_recovery import GenerationRequestRecoveryTest, MODEL, ROUTE


class FlowSubmitRefusalTest(GenerationRequestRecoveryTest):
    def test_competing_hold_then_explicit_requote_after_owned_topup(self):
        with self.db.transaction(immediate=True) as connection:
            connection.execute('UPDATE wallets SET available_credits=7 WHERE user_id=?', (self.user,))
        request = {'model': MODEL, 'prompt': 'synthetic image'}
        quote = self.account.quote_generation(self.user, 'image', request)
        self.submit('competing-key')
        before = self.business_snapshot()
        status, error = self.request('POST', '/api/generation/images', token=self.token,
                                    key='refused-original', body=request | {'quote_id': quote['quote_id']})
        self.assertEqual((status, error['error']['code']), (409, 'INSUFFICIENT_CREDITS'))
        self.assertEqual(self.business_snapshot(), before)
        status, error = self.request('GET', ROUTE, token=self.token, key='refused-original')
        self.assertEqual((status, error['error']['code']), (404, 'GENERATION_TASK_NOT_FOUND'))
        with self.db.transaction(immediate=True) as connection:
            connection.execute('UPDATE wallets SET available_credits=available_credits+7 WHERE user_id=?', (self.user,))
        fresh = self.account.quote_generation(self.user, 'image', request)
        self.assertNotEqual(quote['quote_id'], fresh['quote_id'])
        status, accepted = self.request('POST', '/api/generation/images', token=self.token,
                                       key='explicit-fresh-run', body=request | {'quote_id': fresh['quote_id']})
        self.assertEqual(status, 202)
        self.assertEqual(self.request('GET', ROUTE, token=self.token, key='explicit-fresh-run')[1]['id'], accepted['id'])
        self.assertEqual(self.request('GET', ROUTE, token=self.token, key='refused-original')[0], 404)
        with self.db.connect() as connection:
            self.assertEqual(connection.execute('SELECT count(*) FROM generation_tasks WHERE user_id=?', (self.user,)).fetchone()[0], 2)
            self.assertEqual(connection.execute('SELECT count(*) FROM wallet_holds WHERE user_id=?', (self.user,)).fetchone()[0], 2)
        self.assertEqual(self.image2.calls, 0)

    def test_expired_quote_refusal_preserves_business_state(self):
        request = {'model': MODEL, 'prompt': 'synthetic image'}
        quote = self.account.quote_generation(self.user, 'image', request)
        with self.db.transaction(immediate=True) as connection:
            connection.execute('UPDATE generation_quotes SET expires_at=? WHERE id=?',
                               (self.account.now() - 1, quote['quote_id']))
        before = self.business_snapshot()
        status, error = self.request('POST', '/api/generation/images', token=self.token,
                                    key='expired-original', body=request | {'quote_id': quote['quote_id']})
        self.assertEqual((status, error['error']['code']), (409, 'QUOTE_MISMATCH'))
        self.assertEqual(self.request('GET', ROUTE, token=self.token, key='expired-original')[0], 404)
        self.assertEqual(self.business_snapshot(), before)
        self.assertEqual(self.image2.calls, 0)
