"""One effort can allow several independently frozen Agent models."""
from copy import deepcopy
from dataclasses import replace
import json

from seecut_server import assistant_contract as contract
from seecut_server.efforts import materialize_policy, parse_efforts, validate_efforts
from seecut_server.assistant_models import parse_models
from test_assistant_contract import AssistantFixture, FakeAssistant
from test_generation_robustness import FakeImage2
from test_assistant_efforts import specification
import test_assistant_efforts as legacy



class EffortModelSetTest(AssistantFixture):
    reopen = legacy.AssistantEffortTest.reopen

    def setUp(self):
        super().setUp()
        self.fast, self.quality = FakeAssistant(), FakeAssistant()
        self.image2 = FakeImage2()
        self.config = replace(self.config, assistant_models=parse_models(json.dumps([
            dict(id='fast', label='快捷', model='synthetic-fast', max_turn_credits=12),
            dict(id='quality', label='深入', model='synthetic-quality', max_turn_credits=20),
        ])), model_prices={'image:gpt-image-2.5-flare': 7})

    def enable_sets(self):
        policy = specification()
        policy['agent_models'] = [deepcopy(policy['agent']),
            specification(model='quality', output=200, steps=4, credits=18)['agent']]
        self.config = replace(self.config, efforts=parse_efforts(json.dumps([policy])))
        self.reopen()

    def v4_body(self, model, turn):
        effort = self.service.capabilities()['efforts'][0]
        snapshot = materialize_policy(effort, model)
        return dict(model_id=model, effort_id=effort['id'], policy_revision=effort['policy_revision'],
                    expected_policy_digest=contract.digest(snapshot), client_request_id='budget-' + turn,
                    local_session_id='session-1', turn_id=turn)

    def test_two_models_have_distinct_limits_and_original_budget_replays(self):
        self.enable_sets()
        self.assertEqual(self.service.capabilities()['effort_policy_version'], 4)
        requests = [self.v4_body(model, model) for model in ('fast', 'quality')]
        budgets = []
        for body in requests:
            budget = self.service.turn_budget(self.user, body, body['client_request_id'])
            budgets.append(self.service.close_budget(self.user, budget['budget_id'], {}, 'close-' + body['turn_id']))
        self.assertEqual([b['policy']['agent']['max_output_tokens'] for b in budgets], [100, 200])
        for request, budget in zip(requests, budgets):
            self.assertEqual(contract.digest(budget['policy']), request['expected_policy_digest'])
            self.assertEqual(budget['policy']['policy_version'], 4)
            self.assertNotIn('agent_models', budget['policy'])
            self.assertNotIn('available', budget['policy']['agent'])
        changed = deepcopy(self.config.efforts)
        changed[0]['agent_models'][1]['max_steps'] = 1
        self.config = replace(self.config, efforts=changed)
        self.reopen()
        for request, budget in zip(requests, budgets):
            self.assertEqual(self.service.turn_budget(self.user, request, 'resume-' + request['turn_id']), budget)
        self.expect_error('EFFORT_POLICY_MISMATCH', lambda: self.service.turn_budget(
            self.user, requests[0] | {'model_id': 'quality'}, 'wrong-model'))

    def test_disallowed_model_rejected_before_any_budget(self):
        self.enable_sets()
        body = self.v4_body('fast', 'rejected') | {'model_id': 'default'}
        self.expect_error('EFFORT_POLICY_MISMATCH', lambda: self.service.turn_budget(self.user, body, 'rejected'))
        with self.db.connect() as connection:
            self.assertEqual(connection.execute('SELECT COUNT(*) FROM assistant_turn_budgets').fetchone()[0], 0)
        self.wallet(100, 0)

    def test_agent_and_generation_availability_are_independent(self):
        self.enable_sets()
        self.image2.configured = False
        effort = self.service.capabilities()['efforts'][0]
        self.assertTrue(effort['available'])
        self.assertFalse(effort['generation_available'])
        body = self.v4_body('quality', 'no-generation')
        self.service.turn_budget(self.user, body, 'no-generation')
        self.image2.configured = True
        self.fast.configured = self.quality.configured = False
        effort = self.account.generation_models()['efforts'][0]
        self.assertFalse(effort['available'])
        self.assertTrue(effort['generation_available'])
        quote = self.account.quote_generation(self.user, 'image', dict(model='gpt-image-2.5-flare',
            prompt='fixture', effort_id='lite', policy_revision='policy-1'))
        self.assertEqual(quote['policy']['policy_version'], 4)

    def test_v4_config_validates_each_model_and_default_membership(self):
        self.enable_sets()
        validate_efforts(self.config)
        for mutation in ('missing_default', 'duplicate', 'limits'):
            values = deepcopy(self.config.efforts)
            if mutation == 'missing_default': values[0]['agent_models'].pop(0)
            elif mutation == 'duplicate': values[0]['agent_models'].append(values[0]['agent_models'][0])
            else: values[0]['agent_models'][1]['max_steps'] = 9
            with self.assertRaises(ValueError): parse_efforts(json.dumps(values))
