"""Capped transactions and replay against disposable wallets/fake suppliers."""
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
import copy
import json

from seecut_server.assistant_contract import canonical
from seecut_server.assistant_provider import PreparedUsageInference, ProviderUsage, ProviderFailure
from seecut_server.assistant_worker import AssistantWorker
from seecut_server.assistant_local_resolution import LocalResolution
from seecut_server.model_pricing import parse_price_catalog
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture, done
from test_model_pricing import record
from test_service import FakeEmailSender


class FakeUsageSupplier:
    configured = True
    billing_mode = 'provider_usage_capped_v1'
    usage_contract_id = 'synthetic-provider-usage-v1'
    token_contract_id = None

    def __init__(self):
        self.calls = []
        self.usage = dict(input=10000, cached_input=0, output=0)
        self.tool = False
        self.failure = None
        self.complete_usage = True

    def prepare_scoped(self, request, scope):
        return PreparedUsageInference(canonical(dict(request=request, scope=scope)).encode(), self.usage_contract_id)

    def stream(self, operation_id, prepared, cancelled):
        self.calls.append(operation_id)
        if self.failure:
            raise self.failure
        u = self.usage
        raw = dict(prompt_tokens=u['input'], completion_tokens=u['output'],
                   total_tokens=u['input'] + u['output'], prompt_tokens_details=dict(cached_tokens=u['cached_input']))
        yield ProviderUsage(raw, copy.deepcopy(u) if self.complete_usage else None)
        final = done(tool=self.tool)
        final['usage'] = dict(input=u['input'], output=u['output'])
        yield final


class CappedUsageBillingTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        self.provider = FakeUsageSupplier()
        self.install()

    def install(self, max_credits=10):
        self.config = replace(self.config, assistant_billing_mode='provider_usage_capped_v1',
            assistant_provider_id='synthetic', assistant_model='synthetic-exact',
            assistant_context_window=1000000,
            assistant_max_turn_credits=max_credits,
            model_price_catalog=parse_price_catalog(json.dumps([record({'input': '20', 'output': '60'})])))
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
            clock=lambda: self.clock[0], assistant_provider=self.provider)
        self.service = self.account.assistant
        self.worker = AssistantWorker(self.service)

    def execute(self, body=None):
        op = self.create(body)
        self.worker.process_once()
        return self.service.get(self.user, op['operation_id'])

    def rows(self):
        with self.db.connect() as c:
            return {t: [dict(r) for r in c.execute('SELECT * FROM '+t+' ORDER BY rowid')]
                    for t in ('wallets', 'wallet_holds', 'ledger_entries', 'assistant_turn_budgets', 'assistant_operations', 'assistant_events')}

    def test_task_rounding_uses_current_authorized_remaining_hold(self):
        for count, total in ((10000, 1), (30000, 2)):
            with self.subTest(count=count):
                self.provider.usage['input'] = count
                first = self.execute(self.body(step='first'+str(count), session='session'+str(count), turn='turn'+str(count)))
                second_body = self.body(step='second'+str(count), session=first['local_session_id'],
                                        turn=first['turn_id'], budget=first['budget'])
                second = self.execute(second_body)
                self.assertEqual(first['captured_credits'] + second['captured_credits'], total)
                self.assertEqual(second['reserved_credits'], 9)
                self.assertEqual(second['budget']['spent_credits'], total)
                before = self.rows()
                evidence = ProviderUsage({}, dict(self.provider.usage))
                self.service.finish(second['operation_id'], 'succeeded', second['usage'], second['result'], provider_usage=evidence)
                self.assertEqual(self.rows(), before)
                self.service.close_budget(self.user, first['budget_id'], {}, 'close'+str(count))

    def test_zero_release_and_exact_cap_clear_executable_tools(self):
        self.install(1)
        self.provider.usage = dict(input=0, cached_input=0, output=0)
        zero = self.execute()
        self.assertEqual((zero['state'], zero['billing_state'], zero['captured_credits']), ('succeeded', 'released', 0))
        self.provider.usage['input'] = 50000; self.provider.tool = True
        cap = self.execute(self.body(step='cap', budget=zero['budget']))
        self.assertEqual((cap['state'], cap['billing_state'], cap['budget']['state'], cap['result']), ('failed', 'captured', 'closed', None))
        events = self.service.events(self.user, cap['operation_id'], 0)['events']
        self.assertFalse(any(e['type'] == 'model' and e['payload']['type'] == 'done' for e in events))
        self.expect_error('ASSISTANT_BUDGET_EXCEEDED', lambda: self.service.quote(self.user,
            self.body(step='next', budget=cap['budget']), 'next-quote'))
        self.assertEqual(len(self.provider.calls), 2)

    def test_over_cap_keeps_uncapped_retail_and_uncollected_difference(self):
        self.install(1); self.provider.usage['input'] = 100000; self.provider.tool = True
        op = self.execute()
        self.assertEqual((op['state'], op['captured_credits'], op['result']), ('failed', 1, None))
        with self.db.connect() as c:
            r = c.execute('SELECT * FROM assistant_operations WHERE id=?', (op['operation_id'],)).fetchone()
            self.assertEqual((r['reported_numerator'], r['uncollected_numerator']), ('2000000000', '1000000000'))
            self.assertEqual(json.loads(r['provider_usage_json'])['prompt_tokens'], 100000)
        self.wallet(99, 0)

    def test_concurrent_create_has_one_hold_and_one_operation(self):
        body = self.body(); quote = self.service.quote(self.user, body, 'quote')
        with ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(lambda i: self.service.create(self.user, body | dict(quote_id=quote['quote_id']), 'create'+str(i)), range(8)))
        self.assertEqual(sum(fresh for op, fresh in results), 1)
        self.assertEqual(len({op['operation_id'] for op, fresh in results}), 1)
        self.wallet(90, 10)

    def test_missing_classification_and_disconnect_do_not_free_or_resend(self):
        self.provider.complete_usage = False
        op = self.execute()
        self.assertEqual((op['billing_state'], op['result']), ('settlement_pending', None))
        self.wallet(90, 10)
        self.expect_error('ASSISTANT_BUDGET_EXCEEDED', lambda: self.create(self.body(step='next', budget=op['budget'])))
        self.worker.process_once(); self.assertEqual(len(self.provider.calls), 1)

    def test_queued_cancel_is_known_unexecuted_and_restart_is_unknown(self):
        op = self.create()
        cancelled = self.service.cancel(self.user, op['operation_id'], {}, 'cancel')
        self.assertEqual((cancelled['state'], cancelled['billing_state']), ('cancelled', 'released'))
        self.wallet(100, 0); self.assertEqual(self.provider.calls, [])
        op = self.create(self.body(step='second-turn', turn='turn-2'))
        self.service.claim(); self.service.recover_interrupted()
        recovered = self.service.get(self.user, op['operation_id'])
        self.assertEqual((recovered['state'], recovered['billing_state']), ('outcome_unknown', 'held'))
        self.worker.process_once(); self.assertEqual(self.provider.calls, [])
        self.wallet(90, 10)

    def test_context_limit_settles_actual_usage_but_never_delivers_tools(self):
        self.config = replace(self.config, assistant_context_window=1000)
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
            clock=lambda: self.clock[0], assistant_provider=self.provider)
        self.service = self.account.assistant; self.worker = AssistantWorker(self.service)
        self.provider.usage['input'] = 2000; self.provider.tool = True
        op = self.execute()
        self.assertEqual((op['state'], op['billing_state'], op['result'], op['budget']['state']), ('failed', 'captured', None, 'closed'))
        self.wallet(99, 0)

    def test_unknown_local_resolution_uses_same_frozen_exact_calculator(self):
        self.provider.failure = ProviderFailure()
        op = self.execute()
        self.assertEqual((op['state'], op['billing_state']), ('outcome_unknown', 'held'))
        u = dict(input=10000, output=0); result = done(); result['usage'] = u
        body = dict(user_id=self.user, operation_id=op['operation_id'], expected_version=op['version'],
            resolution_id='synthetic-resolution', operator='fixture', reason='synthetic verified report',
            outcome='confirmed_usage', terminal_state='succeeded', usage=u, result=result,
            cached_input_tokens=0, evidence_confirmed=True, evidence_reference=dict(id='fixture:usage', sha256='a'*64))
        resolver = LocalResolution(self.db, clock=lambda: self.clock[0])
        self.assertEqual(resolver.resolve(body)['status'], 'ready')
        resolved = resolver.resolve(body, apply=True)
        self.assertEqual(resolved['status'], 'resolved')
        self.assertEqual(resolver.resolve(body, apply=True), resolved)
        settled = self.service.get(self.user, op['operation_id'])
        self.assertEqual((settled['captured_credits'], settled['budget']['accrued_microcredits']), (1, 200000))
        self.wallet(99, 0)

    def test_corrupt_contract_after_claim_is_unknown_without_dispatch(self):
        op = self.create()
        with self.db.transaction(immediate=True) as c:
            c.execute("UPDATE assistant_turn_budgets SET billing_contract_digest=? WHERE id=?", ('0'*64, op['budget_id']))
        self.worker.process_once()
        with self.db.connect() as c:
            r = c.execute('SELECT state,billing_state FROM assistant_operations WHERE id=?', (op['operation_id'],)).fetchone()
            self.assertEqual(tuple(r), ('outcome_unknown', 'held'))
        self.assertEqual(self.provider.calls, []); self.wallet(90, 10)

    def test_malformed_raw_cache_can_be_completed_by_confirmed_local_evidence(self):
        op = self.create(); self.service.claim()
        raw = dict(prompt_tokens=10000, completion_tokens=0, total_tokens=10000,
                   prompt_tokens_details=dict(cached_tokens='0'))
        self.service.observe_provider_usage(op['operation_id'], ProviderUsage(raw, None))
        self.service.unknown(op['operation_id']); op = self.service.get(self.user, op['operation_id'])
        body = dict(user_id=self.user, operation_id=op['operation_id'], expected_version=op['version'],
            resolution_id='synthetic-cache-resolution', operator='fixture', reason='synthetic verified cache report',
            outcome='confirmed_usage', terminal_state='failed', usage=dict(input=10000, output=0),
            cached_input_tokens=0, evidence_confirmed=True, evidence_reference=dict(id='fixture:cache', sha256='a'*64))
        resolver = LocalResolution(self.db, clock=lambda: self.clock[0])
        self.assertEqual(resolver.resolve(body)['status'], 'ready')
        self.assertEqual(resolver.resolve(body, apply=True)['status'], 'resolved')
        with self.db.connect() as c:
            r=c.execute('SELECT provider_usage_json FROM assistant_operations WHERE id=?',(op['operation_id'],)).fetchone()
            self.assertEqual(json.loads(r[0]), raw)
        self.wallet(99, 0)

    def test_capped_price_version_is_authoritative_without_legacy_pricing_label(self):
        self.service.config = replace(self.service.config, assistant_pricing_version='')
        capability = self.service.capabilities(include_models=False)
        self.assertTrue(capability['available'])
        self.assertEqual(capability['pricing_version'], 'synthetic-cost-v1')

    def test_unused_quote_refreshes_when_task_remaining_authorization_changes(self):
        first_body = self.body(step='first')
        next_body = self.body(step='next', budget={'budget_id': first_body['budget_id']})
        old = self.service.quote(self.user, next_body, 'next-quote')
        self.assertEqual(old['max_credits'], 10)
        first = self.execute(first_body)
        refreshed = self.service.quote(self.user, next_body, 'next-quote')
        self.assertNotEqual(old['quote_id'], refreshed['quote_id'])
        self.assertEqual(refreshed['max_credits'], 9)
        self.assertEqual(refreshed['billing_contract'], old['billing_contract'])
        self.expect_error('ASSISTANT_QUOTE_MISMATCH', lambda: self.service.create(self.user,
            next_body | dict(quote_id=old['quote_id']), 'create-next'))
        op, fresh = self.service.create(self.user, next_body | dict(quote_id=refreshed['quote_id']), 'create-next')
        self.assertTrue(fresh); self.assertEqual(op['reserved_credits'], 9)
        self.wallet(90, 9)
