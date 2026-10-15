"""Preparation-only tests: no model, tokenizer, or remote requests."""
from dataclasses import replace
from unittest.mock import patch, MagicMock
from seecut_server.assistant_glm import DOMESTIC_BASE, MODEL, GLMAssistantProvider
from seecut_server.assistant_models import model_config, parse_models
from seecut_server.assistant_provider import make_assistant_provider, PreparedUsageInference, ProviderUsage, ProviderFailure
from test_assistant_contract import AssistantFixture


class ProviderUsagePreparationTest(AssistantFixture):
    def configuration(self, mode='provider_usage_capped_v1'):
        return replace(self.config, assistant_model=MODEL, assistant_base_url=DOMESTIC_BASE, assistant_billing_mode=mode)

    def test_capped_wire_has_no_invented_token_bound_or_counter(self):
        provider = make_assistant_provider(self.configuration(), self.db)
        provider.counter = lambda payload: (_ for _ in ()).throw(AssertionError('counter must not run'))
        request = self.body()['request']
        scope = dict(user_id=self.user, budget_id='budget', client_request_id='step')
        prepared = provider.prepare_scoped(request, scope)
        self.assertIsInstance(prepared, PreparedUsageInference)
        self.assertFalse(hasattr(prepared, 'input_token_upper_bound'))
        self.assertFalse(hasattr(prepared, 'token_contract_id'))
        self.assertEqual(prepared, provider.prepare_scoped(request, scope))
        self.assertEqual(prepared.usage_contract_id, 'glm-domestic-provider-usage-v1')

    def test_old_default_and_unknown_mode_stay_strict(self):
        provider = make_assistant_provider(self.configuration('verified_input_v1'), self.db)
        self.assertIsNone(provider.token_contract_id)
        self.assertIsNone(provider.usage_contract_id)
        with self.assertRaises(ValueError):
            provider.prepare_scoped(self.body()['request'], dict(user_id=self.user, budget_id='budget', client_request_id='step'))
        with self.assertRaises(ValueError):
            self.configuration('unrecognized')
        alias = model_config(self.configuration(), dict(id='old', label='old', model=MODEL))
        self.assertEqual(alias.assistant_billing_mode, 'verified_input_v1')
        with self.assertRaises(ValueError):
            parse_models('[{"id":"x","label":"x","model":"x","billing_mode":"bad"}]')

    def test_cache_breakdown_is_explicit_and_total_inclusive(self):
        raw = dict(prompt_tokens=100, completion_tokens=50, total_tokens=150,
                   prompt_tokens_details=dict(cached_tokens=40))
        evidence = GLMAssistantProvider._provider_usage(raw)
        self.assertEqual(evidence.raw, raw)
        self.assertEqual(evidence.classified, dict(input=100, cached_input=40, output=50))
        self.assertIsNone(GLMAssistantProvider._provider_usage(dict(prompt_tokens=100, completion_tokens=50)).classified)
        raw['prompt_tokens_details']['cached_tokens'] = 101
        self.assertIsNone(GLMAssistantProvider._provider_usage(raw).classified)

    def test_malformed_supplier_usage_is_retained_before_failure(self):
        provider = make_assistant_provider(self.configuration(), self.db)
        prepared = provider.prepare_scoped(self.body()['request'],
            dict(user_id=self.user, budget_id='budget', client_request_id='step'))
        raw = dict(prompt_tokens=10, completion_tokens=0, total_tokens=999)
        provider._chunks = lambda response, started: iter([dict(usage=raw), None])
        response = MagicMock(); response.status = 200
        with patch('seecut_server.assistant_glm.open_no_redirect') as opener:
            opener.return_value.__enter__.return_value = response
            stream = provider.stream('operation', prepared, lambda: False)
            evidence = next(stream)
            self.assertIsInstance(evidence, ProviderUsage)
            self.assertEqual((evidence.raw, evidence.classified), (raw, None))
            with self.assertRaises(ProviderFailure):
                next(stream)
