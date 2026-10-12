"""Frozen model contracts against synthetic account costs only."""
from dataclasses import replace
import copy
import json
from seecut_server.assistant_contract import digest
from seecut_server.assistant_usage_contract import freeze, restore_config, validate
from seecut_server.model_pricing import parse_price_catalog
from test_assistant_contract import AssistantFixture
from test_model_pricing import record


class FrozenUsageContractTest(AssistantFixture):
    def config_for_usage(self):
        return replace(self.config, assistant_billing_mode='provider_usage_capped_v1',
            assistant_provider_id='synthetic', assistant_model='synthetic-exact',
            model_price_catalog=parse_price_catalog(json.dumps([record()])))

    def test_no_effort_freezes_exact_model_price_and_limits(self):
        config = self.config_for_usage()
        frozen = freeze(config, 'synthetic-usage-v1', 'a' * 64)
        self.assertIsNone(frozen['effort_digest'])
        changed = replace(config, assistant_model='other-model', assistant_provider_id='other',
            model_price_catalog=(), assistant_max_turn_credits=1, assistant_pricing_version='changed')
        restored = restore_config(changed, frozen)
        self.assertEqual((restored.assistant_model, restored.assistant_provider_id,
            restored.assistant_max_turn_credits, restored.assistant_pricing_version),
            ('synthetic-exact', 'synthetic', config.assistant_max_turn_credits, 'synthetic-cost-v1'))
        self.assertEqual(validate(frozen, expected_digest=digest(frozen)), frozen)

    def test_modified_contract_cannot_supply_its_own_digest(self):
        frozen = freeze(self.config_for_usage(), 'synthetic-usage-v1', 'a' * 64)
        original_digest = digest(frozen)
        modified = copy.deepcopy(frozen)
        modified['limits']['max_turn_credits'] += 1
        with self.assertRaises(ValueError):
            validate(modified, expected_digest=original_digest)
        modified = copy.deepcopy(frozen)
        modified['upstream_model'] = 'other'
        with self.assertRaises(ValueError):
            validate(modified)

    def test_unknown_mode_and_effort_mismatch_rejected(self):
        policy = dict(id='synthetic-policy')
        frozen = freeze(self.config_for_usage(), 'synthetic-usage-v1', 'a' * 64, policy)
        self.assertEqual(validate(frozen, check_effort=True, effort_policy=policy), frozen)
        with self.assertRaises(ValueError):
            validate(frozen, check_effort=True)
        frozen['billing_mode'] = 'unknown'
        with self.assertRaises(ValueError):
            validate(frozen)
