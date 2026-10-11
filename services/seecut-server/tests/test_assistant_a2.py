"""Input bounds, delayed quotes and the pre-assistant shared-wallet upgrade."""
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from pathlib import Path

from seecut_server.assistant_provider import AssistantProvider, PreparedInference, ProviderFailure
from seecut_server.assistant_worker import AssistantWorker
from seecut_server.db import Database
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture, FakeAssistant
from test_service import FakeEmailSender, FakeImage2, FakeXiangxin


class AssistantA2Test(AssistantFixture):
    def test_full_input_plus_output_accepts_exact_boundary(self):
        body = self.body()
        before = self.provider.prepare(body['request']).input_token_upper_bound
        body['request']['system'] += 'x' * (800 - before)
        op = self.create(body)
        self.worker.process_once()
        self.assertEqual(self.provider.prepared[op['operation_id']].input_token_upper_bound, 800)
        result = self.service.get(self.user, op['operation_id'])
        self.assertEqual((result['state'], result['captured_credits']), ('succeeded', 2))
        self.wallet(98, 0)

    def test_complete_input_overflow_rejected_before_quote_hold_or_dispatch(self):
        body = self.body()
        # Only the user text is short. System, tool schema and history all count.
        body['request']['tools'][0]['description'] += 'x' * 700
        body['request']['messages'].append({'id':'old-result', 'role':'assistant',
                                          'content':[{'type':'text','text':'earlier context'}]})
        self.expect_error('ASSISTANT_CONTEXT_EXCEEDED', lambda: self.service.quote(self.user, body, 'q'))
        self.wallet(100, 0)
        with self.db.connect() as connection:
            for table in ('assistant_quotes', 'assistant_operations', 'wallet_holds'):
                self.assertEqual(connection.execute('SELECT COUNT(*) FROM '+table).fetchone()[0], 0)
        self.assertEqual(self.provider.calls, [])

    def test_generic_configured_endpoint_and_unproven_images_stay_unavailable(self):
        self.service.provider = AssistantProvider(self.config)
        self.assertTrue(self.service.provider.configured)
        self.assertFalse(self.service.capabilities()['available'])
        self.assertEqual(self.service.capabilities()['reason_code'], 'ASSISTANT_INPUT_CONTRACT_UNAVAILABLE')
        self.service.provider = self.provider
        self.service.config = replace(self.config, assistant_images=True)
        self.assertFalse(self.service.capabilities()['available'])
        self.assertEqual(self.provider.calls, [])

    def test_invalid_or_missing_adapter_measurement_never_holds_credits(self):
        body = self.body()
        for prepared in (None, PreparedInference(b'{}', True, self.provider.token_contract_id),
                         PreparedInference(b'{}', -1, self.provider.token_contract_id),
                         PreparedInference(b'{}', 1, 'different-contract')):
            with self.subTest(prepared=prepared):
                self.provider.prepare = lambda request, value=prepared: value
                self.expect_error('ASSISTANT_INPUT_CONTRACT_UNAVAILABLE', lambda: self.service.quote(self.user, body, 'q'))
                self.wallet(100, 0)
        self.assertEqual(self.provider.calls, [])

    def test_changed_prepared_payload_is_rejected_before_acceptance(self):
        body = self.body()
        quote = self.service.quote(self.user, body, 'q')
        original = self.provider.prepare(body['request'])
        self.provider.prepare = lambda request: replace(original, payload=original.payload.replace(b'system', b'SYSTEM'))
        self.expect_error('ASSISTANT_QUOTE_MISMATCH', lambda: self.service.create(self.user, body | {'quote_id':quote['quote_id']}, 'c'))
        self.wallet(100, 0)
        self.assertEqual(self.provider.calls, [])

    def test_changed_prepared_payload_after_acceptance_releases_without_dispatch(self):
        op = self.create()
        original = self.provider.prepare
        self.provider.prepare = lambda request: replace(original(request), payload=b'changed')
        self.worker.process_once()
        result = self.service.get(self.user, op['operation_id'])
        self.assertEqual((result['state'], result['billing_state']), ('failed', 'released'))
        self.assertEqual(result['error']['code'], 'ASSISTANT_INPUT_CONTRACT_CHANGED')
        self.assertEqual(self.provider.calls, [])
        self.wallet(100, 0)

    def test_usage_above_frozen_input_but_below_context_stays_unconfirmed(self):
        body = self.body()
        bound = self.provider.prepare(body['request']).input_token_upper_bound
        self.provider.usage_override = {'input':bound + 1, 'output':50}
        self.assertLess(bound + 51, self.config.assistant_context_window)
        op = self.create(body)
        self.worker.process_once()
        result = self.service.get(self.user, op['operation_id'])
        self.assertEqual(result['billing_state'], 'settlement_pending')
        page = self.service.events(self.user, op['operation_id'], 0)
        self.assertFalse(any(e['type']=='model' and e['payload']['type']=='done' for e in page['events']))
        self.wallet(97, 3)

    def test_concurrent_expired_quote_refresh_keeps_request_budget_and_one_hold(self):
        body = self.body()
        first = self.service.quote(self.user, body, 'q')
        self.clock[0] += 121
        with ThreadPoolExecutor(max_workers=4) as pool:
            quotes = list(pool.map(lambda i: self.service.quote(self.user, body, 'q'+str(i)), range(8)))
        self.assertEqual(len({q['quote_id'] for q in quotes}), 1)
        self.assertNotEqual(first['quote_id'], quotes[0]['quote_id'])
        self.assertEqual(self.service.quote(self.user, body, 'q')['quote_id'], quotes[0]['quote_id'])
        self.expect_error('ASSISTANT_QUOTE_MISMATCH', lambda: self.service.create(self.user, body | {'quote_id':first['quote_id']}, 'c'))
        op, fresh = self.service.create(self.user, body | {'quote_id':quotes[0]['quote_id']}, 'c')
        self.assertTrue(fresh)
        replay, fresh = self.service.create(self.user, body | {'quote_id':first['quote_id']}, 'c')
        self.assertFalse(fresh)
        self.assertEqual(replay['operation_id'], op['operation_id'])
        self.assertEqual(op['budget_id'], body['budget_id'])
        self.worker.process_once()
        self.assertEqual(self.provider.calls, [op['operation_id']])
        self.wallet(99, 0)

    def test_first_quote_after_unused_budget_expiry_renews_same_budget(self):
        body = self.body()
        self.clock[0] += 121
        op = self.create(body)
        self.assertEqual(op['budget_id'], body['budget_id'])
        self.assertEqual(op['budget']['expires_at'], self.clock[0]+120)
        self.assertEqual(op['budget']['operation_count'], 1)

    def test_live_quote_renews_expired_unused_budget_without_requoting(self):
        body = self.body()
        quote = self.service.quote(self.user, body, 'q')
        with self.db.transaction(immediate=True) as connection:
            connection.execute('UPDATE assistant_turn_budgets SET expires_at=? WHERE id=?', (self.clock[0]-1, body['budget_id']))
        renewed = self.service.quote(self.user, body, 'q')
        self.assertEqual(renewed['quote_id'], quote['quote_id'])
        op, fresh = self.service.create(self.user, body | {'quote_id':quote['quote_id']}, 'c')
        self.assertTrue(fresh)
        self.assertEqual(op['budget']['expires_at'], self.clock[0]+120)

    def test_refresh_never_revives_replaced_or_closed_budget(self):
        body = self.body()
        self.service.quote(self.user, body, 'q')
        self.clock[0] += 121
        self.body(step='next', turn='other-turn')
        self.expect_error('ASSISTANT_BUDGET_EXCEEDED', lambda: self.service.quote(self.user, body, 'q'))
        self.wallet(100, 0)
        self.assertEqual(self.provider.calls, [])

    def test_unknown_retains_original_quote_operation_hold_and_expiry(self):
        body = self.body()
        quote = self.service.quote(self.user, body, 'q')
        op = self.create(body)
        self.provider.events = []
        self.provider.failure = ProviderFailure()
        self.worker.process_once()
        self.clock[0] += 1000
        self.assertEqual(self.service.quote(self.user, body, 'q2'), quote)
        replay, fresh = self.service.create(self.user, body | {'quote_id':quote['quote_id']}, 'c2')
        self.assertFalse(fresh)
        self.assertEqual(replay['operation_id'], op['operation_id'])
        self.assertEqual(replay['budget']['expires_at'], op['budget']['expires_at'])
        self.assertEqual(self.worker.process_once(), 0)
        self.assertEqual(self.provider.calls, [op['operation_id']])
        self.wallet(97, 3)

    def test_formal_pre_assistant_schema_upgrade_preserves_generation_and_wallet(self):
        # Exact schema from public HEAD 1e8009ea9ea7e90c2ceb8720037711f02970b923.
        # Temporary A1 schemas are deliberately not a supported upgrade source.
        root = Path(self.temp.name)
        schema = Path(__file__).parent / 'fixtures/schema-before-assistant-1e8009e.sql'
        database = Database(root/'legacy.db', schema)
        database.initialize()
        config = replace(self.config, database_path=database.path,
                         model_prices={'image:gpt-image-2.5-flare':7})
        old = SeeCutService(config, database, emailer=FakeEmailSender(config),
                           xiangxin=FakeXiangxin(), image2=FakeImage2(), clock=lambda:self.clock[0])
        registration = old.register('legacy@example.test', 'correct-horse-battery')
        old.verify_email('legacy@example.test', registration['verification_token'])
        user = old.login('legacy@example.test', 'correct-horse-battery')['user']['id']
        with database.transaction(immediate=True) as connection:
            connection.execute('UPDATE wallets SET available_credits=100 WHERE user_id=?', (user,))
        request = {'model':'gpt-image-2.5-flare','prompt':'synthetic still'}
        quote = old.quote_generation(user, 'image', request)
        submitted = request | {'quote_id':quote['quote_id']}
        generation = old.create_generation_task(user, 'image', submitted, 'old-generation')
        with database.connect() as connection:
            tables = [r[0] for r in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")]
            before = {table:[dict(r) for r in connection.execute('SELECT * FROM '+table)] for table in tables}
            self.assertNotIn('assistant_operations', tables)
        database.schema_path = self.db.schema_path
        database.initialize()
        database.initialize()
        with database.connect() as connection:
            for table, rows in before.items():
                self.assertEqual([dict(r) for r in connection.execute('SELECT * FROM '+table)], rows, table)
            self.assertEqual(connection.execute('PRAGMA foreign_key_check').fetchall(), [])
        service = SeeCutService(config, database, emailer=FakeEmailSender(config),
                               xiangxin=FakeXiangxin(), image2=FakeImage2(),
                               clock=lambda:self.clock[0], assistant_provider=FakeAssistant())
        budget = service.assistant.turn_budget(user, {'client_request_id':'b','local_session_id':'session-1','turn_id':'turn-1'}, 'b')
        body = self.body(budget=budget)
        aq = service.assistant.quote(user, body, 'q')
        op, _ = service.assistant.create(user, body | {'quote_id':aq['quote_id']}, 'c')
        self.assertEqual((service.wallet(user)['available_credits'],service.wallet(user)['held_credits']), (90,10))
        for claimed in service.claim_generation_tasks():
            service.process_generation_task(claimed)
        AssistantWorker(service.assistant).process_once()
        self.assertEqual(service.get_generation_task(user,generation['id'])['status'],'succeeded')
        self.assertEqual(service.assistant.get(user,op['operation_id'])['state'],'succeeded')
        self.assertEqual((service.wallet(user)['available_credits'],service.wallet(user)['held_credits']), (92,0))
        self.assertEqual(service.create_generation_task(user,'image',submitted,'old-generation')['id'], generation['id'])
        self.assertFalse(service.assistant.create(user,body | {'quote_id':aq['quote_id']},'c')[1])
        with database.connect() as connection:
            rows = list(connection.execute('SELECT * FROM ledger_entries WHERE user_id=?',(user,)))
            self.assertEqual(100+sum(r['delta_available'] for r in rows), 92)
            self.assertEqual(sum(r['delta_held'] for r in rows), 0)
            self.assertEqual(connection.execute('PRAGMA foreign_key_check').fetchall(), [])
