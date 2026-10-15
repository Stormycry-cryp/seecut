"""Public retail contracts keep private procurement frozen in server records."""
import copy
import json
from pathlib import Path
from seecut_server.assistant_contract import digest
from seecut_server.model_pricing import public_price_snapshot
from seecut_server.efforts import public_policy_snapshot
from seecut_server.service import SeeCutService
from test_provider_usage_billing import CappedUsageBillingTest, FakeUsageSupplier
from test_assistant_contract import AssistantFixture

PRIVATE = {'cost_yuan', 'cost_status', 'source', 'verified_at', 'credits_per_yuan', 'multiplier', 'retail_source', 'sale_override_credits', 'model_pricing'}
def assert_public(test, value):
    if isinstance(value, dict):
        test.assertFalse(PRIVATE & value.keys())
        for item in value.values(): assert_public(test, item)
    elif isinstance(value, list):
        for item in value: assert_public(test, item)

class PublicPriceProjectionTest(AssistantFixture):
    install = CappedUsageBillingTest.install
    def setUp(self):
        super().setUp()
        self.provider = FakeUsageSupplier()
        self.install()
    def test_public_receipts_keep_original_private_contract_unchanged(self):
        capabilities = self.service.capabilities()
        assert_public(self, capabilities)
        body = self.body()
        with self.db.connect() as c:
            before = dict(c.execute('SELECT * FROM assistant_turn_budgets WHERE id=?', (body['budget_id'],)).fetchone())
        frozen = json.loads(before['billing_contract_json'])
        self.assertEqual(frozen['schema_version'], 1)
        self.assertIn('cost_yuan', frozen['pricing_snapshot']['dimensions']['input'])
        quote = self.service.quote(self.user, body, 'projection-quote')
        assert_public(self, quote)
        op, _ = self.service.create(self.user, body | dict(quote_id=quote['quote_id']), 'projection-create')
        assert_public(self, op)
        self.assertEqual(op['billing_contract']['schema_version'], 2)
        self.assertEqual(op['billing_contract_digest'], digest(op['billing_contract']))
        self.worker.process_once()
        result = self.service.get(self.user, op['operation_id'])
        assert_public(self, result)
        with self.db.connect() as c:
            after = dict(c.execute('SELECT * FROM assistant_turn_budgets WHERE id=?', (body['budget_id'],)).fetchone())
        self.assertEqual((after['billing_contract_json'], after['billing_contract_digest']), (before['billing_contract_json'], before['billing_contract_digest']))
        self.assertEqual(result['captured_credits'], 1)

    def test_shared_public_fixture_and_nested_generation_policy(self):
        base = Path(__file__).parent / 'fixtures'
        old = json.loads((base / 'provider_usage_catalog.json').read_text())
        public = json.loads((base / 'public_usage_catalog.json').read_text())
        original = copy.deepcopy(old)
        self.assertEqual(public_price_snapshot(old['pricing_snapshot']), public['pricing_snapshot'])
        self.assertEqual(public_price_snapshot(public['pricing_snapshot']), public['pricing_snapshot'])
        self.assertEqual(digest(public['pricing_snapshot']), public['pricing_digest'])
        policy = {key: value for key, value in old['efforts'][0].items() if key not in ('available', 'reason_code')}
        request = SeeCutService._public_generation_request(dict(prompt='fixture', policy=policy, model_pricing={'private':True}))
        assert_public(self, request)
        self.assertEqual(request['policy'], public_policy_snapshot(policy))
        self.assertEqual(old, original)
