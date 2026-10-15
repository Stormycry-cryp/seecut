"""Server-owned effort policies over the existing verified model catalogs."""
from __future__ import annotations

import hashlib
import json
from copy import deepcopy
from itertools import product

from .assistant_models import IDENTITY, model_config
from .assistant_billing import credit_cost
from .catalog import billing_key, public_catalog
from .errors import ApiError
from .model_pricing import price_snapshot, public_price_snapshot

AGENT_LIMITS = {'max_output_tokens': 131072, 'max_steps': 8, 'max_turn_credits': 1_000_000_000}
FROZEN_FIELDS = (*AGENT_LIMITS, 'pricing_version', 'context_window', 'input_rate', 'output_rate')


def parse_efforts(raw):
    """A small explicit configuration; absence never invents production tiers."""
    try:
        if not isinstance(raw, str) or len(raw.encode('utf-8')) > 32768:
            raise ValueError
        values = json.loads(raw)
        if not isinstance(values, list) or len(values) > 8:
            raise ValueError
        seen = set()
        for value in values:
            if not isinstance(value, dict) or set(value) not in (
                    {'id', 'label', 'policy_revision', 'agent', 'generation'},
                    {'id', 'label', 'policy_revision', 'agent', 'agent_models', 'generation'}):
                raise ValueError
            for name in ('id', 'policy_revision'):
                if not isinstance(value[name], str) or not IDENTITY.fullmatch(value[name]):
                    raise ValueError
            if value['id'] in seen:
                raise ValueError
            seen.add(value['id'])
            label = value['label']
            if not isinstance(label, str) or not label.strip() or len(label) > 64 or any(ord(char) < 32 for char in label):
                raise ValueError
            agent, generation = value['agent'], value['generation']
            if not isinstance(agent, dict) or set(agent) != {'model_id', *AGENT_LIMITS} or \
                    not isinstance(agent['model_id'], str) or not IDENTITY.fullmatch(agent['model_id']):
                raise ValueError
            for name, maximum in AGENT_LIMITS.items():
                if type(agent[name]) is not int or not 1 <= agent[name] <= maximum:
                    raise ValueError
            if 'agent_models' in value:
                models = value['agent_models']
                if not isinstance(models, list) or not 1 <= len(models) <= 17:
                    raise ValueError
                identities = set()
                for model in models:
                    if not isinstance(model, dict) or set(model) != {'model_id', *AGENT_LIMITS} or \
                            not isinstance(model['model_id'], str) or not IDENTITY.fullmatch(model['model_id']) or \
                            model['model_id'] in identities:
                        raise ValueError
                    identities.add(model['model_id'])
                    if any(type(model[name]) is not int or not 1 <= model[name] <= maximum
                           for name, maximum in AGENT_LIMITS.items()):
                        raise ValueError
                if agent not in models:
                    raise ValueError
            if not isinstance(generation, dict) or set(generation) != {'image_model_ids', 'video_model_ids'}:
                raise ValueError
            for names in generation.values():
                if not isinstance(names, list) or len(names) > 16 or any(
                    not isinstance(name, str) or not name or len(name) > 256 or '\0' in name for name in names
                ) or len(names) != len(set(names)):
                    raise ValueError
            if not any(generation.values()) and 'agent_models' not in value:
                raise ValueError
        return tuple(deepcopy(values))
    except (ValueError, TypeError, OverflowError) as error:
        raise ValueError('Invalid effort policies') from error


def validate_efforts(config):
    # Also validate dataclass replacements used by embedders, not only env input.
    values = parse_efforts(json.dumps(config.efforts))
    agents = {'default': config}
    agents.update({model['id']: model_config(config, model) for model in config.assistant_models})
    generations = public_catalog(config.image2_model)['models']
    supported = {kind: {model['id'] for model in generations if model['kind'] == kind} for kind in ('image', 'video')}
    for value in values:
        for agent in value.get('agent_models', [value['agent']]):
            model = agents.get(agent['model_id'])
            if model is None or any(agent[name] > getattr(model, 'assistant_' + name) for name in AGENT_LIMITS) or \
                    agent['max_output_tokens'] >= model.assistant_context_window:
                raise ValueError('Invalid effort model policy')
        for kind in supported:
            if not set(value['generation'][kind + '_model_ids']) <= supported[kind]:
                raise ValueError('Invalid effort generation model')


def selection_pair(effort_id, policy_revision):
    if effort_id is None and policy_revision is None:
        return None
    if not all(isinstance(value, str) and IDENTITY.fullmatch(value) for value in (effort_id, policy_revision)):
        raise ApiError(400, 'EFFORT_POLICY_INVALID', '强度策略标识无效')
    return effort_id, policy_revision


def _priced(account, model):
    if account.config.model_cost_pricing_enabled:
        try:
            snapshot = price_snapshot(account.config.model_price_catalog, model['provider'], model['id'], model['kind'])
            units = {'per_image'} if model['kind'] == 'image' else {'per_second', 'per_millisecond'}
            return any(value['unit'] in units for value in snapshot['dimensions'].values())
        except (ValueError, KeyError, TypeError):
            return False
    prices = account.config.model_prices
    if account.config.env in {'development', 'test'} and f"{model['kind']}:{model['id']}" in prices:
        return True
    choices = []
    for name in model['billing_dimensions']:
        if name == 'operation':
            values = model['operations']
        elif name == 'reference_count':
            values = ([], ['reference'])
        else:
            values = model['parameters'][name]['values']
        choices.append(values)
    for values in product(*choices):
        normalized = dict(zip(model['billing_dimensions'], values))
        if 'reference_count' in normalized:
            normalized['reference_asset_ids'] = normalized.pop('reference_count')
        if billing_key(model, normalized) in prices:
            return True
    return False


def _generation_models(account):
    models = public_catalog(account.config.image2_model)['models']
    for model in models:
        provider = account.image2 if model['provider'] == 'image2' else account.xiangxin
        reason = None if provider.configured else 'GENERATION_NOT_CONFIGURED'
        if reason is None and not _priced(account, model):
            reason = 'MODEL_COST_UNVERIFIED' if account.config.model_cost_pricing_enabled else 'MODEL_PRICE_NOT_CONFIGURED'
        model.update(available=reason is None, reason_code=reason)
    return models


def public_efforts(account):
    generations = _generation_models(account)
    available_ids = {kind: {model['id'] for model in generations if model['kind'] == kind and model['available']}
                     for kind in ('image', 'video')}
    policies = []
    for specification in account.config.efforts:
        policy = deepcopy(specification)
        if 'agent_models' in policy:
            rows = [_public_agent(account, agent) for agent in policy['agent_models']]
            policy['agent_models'] = rows
            policy['agent'] = {key: value for key, value in next(
                row for row in rows if row['model_id'] == policy['agent']['model_id']).items()
                if key not in {'available', 'reason_code'}}
            for kind, supported in available_ids.items():
                policy['generation'][kind + '_model_ids'] = [
                    identity for identity in policy['generation'][kind + '_model_ids'] if identity in supported]
            available = any(row['available'] for row in rows)
            policy.update(available=available, reason_code=None if available else 'EFFORT_POLICY_UNAVAILABLE',
                          generation_available=any(policy['generation'].values()))
            policies.append(policy)
            continue
        service = account.assistant._model_service(policy['agent']['model_id'], read_only=True)
        capability = service.capabilities(include_models=False)
        for name in FROZEN_FIELDS:
            if name not in policy['agent']:
                policy['agent'][name] = getattr(service.config, 'assistant_' + name)
        # Keep legacy policy JSON and digests unchanged.
        if service.config.assistant_rate_denominator == 1_000_000:
            policy['agent']['rate_denominator'] = 1_000_000
        capped = capability.get('billing_mode') == 'provider_usage_capped_v1'
        if capped:
            for name in ('billing_mode', 'usage_contract_id', 'provider_id', 'upstream_model', 'pricing_snapshot', 'pricing_digest', 'context_preflight'):
                policy['agent'][name] = deepcopy(capability[name])
            policy['agent']['pricing_version'] = capability['pricing_version']
        for kind, supported in available_ids.items():
            policy['generation'][kind + '_model_ids'] = [
                identity for identity in policy['generation'][kind + '_model_ids'] if identity in supported]
        reason = capability['reason_code'] if not capability['available'] else None
        agent = policy['agent']
        reserve = credit_cost(agent['context_window'], agent['max_output_tokens'], agent['input_rate'],
                              agent['output_rate'], agent.get('rate_denominator', 1000))
        if reason is None and not capped and reserve > agent['max_turn_credits']:
            reason = 'EFFORT_BUDGET_UNAVAILABLE'
        if reason is None and not capped and not any(policy['generation'].values()):
            reason = 'EFFORT_GENERATION_UNAVAILABLE'
        policy.update(available=reason is None, reason_code=reason)
        policies.append(policy)
    return dict(effort_policy_version=4 if any('agent_models' in policy for policy in policies) else 3 if any(policy['agent'].get('billing_mode') == 'provider_usage_capped_v1' for policy in policies) else 2 if any(policy['agent'].get('rate_denominator', 1000) == 1_000_000 for policy in policies) else 1,
                default_effort_id=policies[0]['id'] if policies else None, efforts=policies)


def _public_agent(account, specification):
    agent = deepcopy(specification)
    service = account.assistant._model_service(agent['model_id'], read_only=True)
    capability = service.capabilities(include_models=False)
    for name in FROZEN_FIELDS:
        if name not in agent:
            agent[name] = getattr(service.config, 'assistant_' + name)
    if service.config.assistant_rate_denominator == 1_000_000:
        agent['rate_denominator'] = 1_000_000
    capped = capability.get('billing_mode') == 'provider_usage_capped_v1'
    if capped:
        for name in ('billing_mode', 'usage_contract_id', 'provider_id', 'upstream_model', 'pricing_snapshot',
                     'pricing_digest', 'context_preflight', 'pricing_version'):
            agent[name] = deepcopy(capability[name])
    reason = capability['reason_code'] if not capability['available'] else None
    if reason is None and not capped and credit_cost(agent['context_window'], agent['max_output_tokens'],
            agent['input_rate'], agent['output_rate'], agent.get('rate_denominator', 1000)) > agent['max_turn_credits']:
        reason = 'EFFORT_BUDGET_UNAVAILABLE'
    return agent | dict(available=reason is None, reason_code=reason)


def materialize_policy(policy, model_id):
    """Freeze one allowed model, never a mutable catalog or its availability."""
    agent = next((row for row in policy['agent_models'] if row['model_id'] == model_id), None)
    if agent is None:
        raise ApiError(409, 'EFFORT_POLICY_MISMATCH', 'Agent 模型与所选强度策略不一致')
    return dict(policy_version=4, id=policy['id'], label=policy['label'], policy_revision=policy['policy_revision'],
                agent={key: deepcopy(value) for key, value in agent.items() if key not in {'available', 'reason_code'}},
                generation=deepcopy(policy['generation']))


def generation_catalog(account):
    return public_catalog(account.config.image2_model) | {'models': _generation_models(account)} | public_efforts(account)


def resolve_effort(account, effort_id, policy_revision, *, model_id=None, purpose='agent'):
    pair = selection_pair(effort_id, policy_revision)
    if pair is None:
        if account.config.efforts:
            raise ApiError(409, 'EFFORT_POLICY_REQUIRED', '请先选择当前强度策略')
        return None
    policy = next((policy for policy in public_efforts(account)['efforts'] if policy['id'] == effort_id), None)
    if policy is None or policy['policy_revision'] != policy_revision:
        raise ApiError(409, 'EFFORT_POLICY_STALE', '强度策略已变更，请重新选择')
    if 'agent_models' in policy:
        selected_id = model_id or policy['agent']['model_id']
        snapshot = materialize_policy(policy, selected_id)
        selected = next(row for row in policy['agent_models'] if row['model_id'] == selected_id)
        if not (policy['generation_available'] if purpose == 'generation' else selected['available']):
            raise ApiError(409, 'EFFORT_POLICY_UNAVAILABLE', '当前强度策略暂不可用')
        return snapshot
    if not policy['available']:
        raise ApiError(409, 'EFFORT_POLICY_UNAVAILABLE', '当前强度策略暂不可用')
    return {key: value for key, value in policy.items() if key not in {'available', 'reason_code'}}


def resolve_generation_effort(account, kind, model_id, effort_id, policy_revision):
    policy = resolve_effort(account, effort_id, policy_revision, purpose='generation')
    if policy is not None and model_id not in policy['generation'].get(kind + '_model_ids', []):
        raise ApiError(409, 'EFFORT_POLICY_MISMATCH', '生成模型与所选强度策略不一致')
    return policy


def configured_policy_fingerprint(account, policy):
    """Internal quote guard: transient catalog filtering is not a policy change."""
    if policy is None:
        return None
    specification = next(value for value in account.config.efforts if value['id'] == policy['id'])
    configured = deepcopy(policy)
    configured['generation'] = deepcopy(specification['generation'])
    canonical = json.dumps(configured, ensure_ascii=False, sort_keys=True, separators=(',', ':'))
    return hashlib.sha256(canonical.encode('utf-8')).hexdigest()


def public_policy_snapshot(policy):
    value = deepcopy(policy)
    if value is not None:
        for agent in [value['agent'], *value.get('agent_models', [])]:
            if agent.get('pricing_snapshot') is not None:
                agent['pricing_snapshot'] = public_price_snapshot(agent['pricing_snapshot'])
                canonical = json.dumps(agent['pricing_snapshot'], ensure_ascii=False, sort_keys=True, separators=(',', ':'))
                agent['pricing_digest'] = hashlib.sha256(canonical.encode('utf-8')).hexdigest()
    return value
