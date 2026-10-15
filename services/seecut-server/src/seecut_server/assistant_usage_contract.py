"""Immutable opt-in usage billing contract, including no-effort budgets."""
from copy import deepcopy
from dataclasses import replace

from . import assistant_contract as contract
from .model_pricing import billing_mode, price_snapshot, validate_price_snapshot, public_price_snapshot

MODE = 'provider_usage_capped_v1'
LIMITS = {'context_window': 1_000_000, 'max_output_tokens': 131072,
          'max_steps': 8, 'max_turn_credits': 1_000_000_000}


def freeze(config, usage_contract_id, route_fingerprint, effort_policy=None):
    if billing_mode(config.assistant_billing_mode) != MODE:
        raise ValueError('Usage contract requires explicit capped mode')
    price = price_snapshot(config.model_price_catalog, config.assistant_provider_id, config.assistant_model)
    value = dict(schema_version=1, billing_mode=MODE, provider_id=config.assistant_provider_id,
        upstream_model=config.assistant_model, usage_contract_id=usage_contract_id,
        route_fingerprint=route_fingerprint, pricing_snapshot=price, pricing_digest=contract.digest(price),
        limits={name: getattr(config, 'assistant_' + name) for name in LIMITS},
        effort_digest=contract.digest(effort_policy) if effort_policy is not None else None)
    return validate(value)


def validate(value, *, expected_digest=None, effort_policy=None, check_effort=False):
    required = {'schema_version', 'billing_mode', 'provider_id', 'upstream_model', 'usage_contract_id',
                'route_fingerprint', 'pricing_snapshot', 'pricing_digest', 'limits', 'effort_digest'}
    if not isinstance(value, dict) or set(value) != required or type(value['schema_version']) is not int or value['schema_version'] != 1 or billing_mode(value['billing_mode']) != MODE:
        raise ValueError('Invalid frozen usage contract')
    for name in ('provider_id', 'usage_contract_id'):
        if not isinstance(value[name], str) or not contract.IDENTITY.fullmatch(value[name]):
            raise ValueError('Invalid frozen usage identity')
    for name in ('route_fingerprint', 'pricing_digest'):
        if not isinstance(value[name], str) or len(value[name]) != 64 or any(c not in '0123456789abcdef' for c in value[name]):
            raise ValueError('Invalid frozen usage digest')
    price = validate_price_snapshot(value['pricing_snapshot'])
    if price['kind'] != 'agent' or (price['provider'], price['model']) != (value['provider_id'], value['upstream_model']) or contract.digest(price) != value['pricing_digest']:
        raise ValueError('Frozen pricing identity mismatch')
    limits = value['limits']
    if not isinstance(limits, dict) or set(limits) != set(LIMITS) or any(
        type(limits[name]) is not int or not 1 <= limits[name] <= maximum for name, maximum in LIMITS.items()
    ) or limits['max_output_tokens'] > limits['context_window']:
        raise ValueError('Invalid frozen usage limits')
    effort = value['effort_digest']
    if effort is not None and (not isinstance(effort, str) or len(effort) != 64 or any(c not in '0123456789abcdef' for c in effort)):
        raise ValueError('Invalid frozen effort digest')
    if check_effort and effort != (contract.digest(effort_policy) if effort_policy is not None else None):
        raise ValueError('Frozen effort mismatch')
    if expected_digest is not None and contract.digest(value) != expected_digest:
        raise ValueError('Original billing contract digest mismatch')
    return deepcopy(value)


def restore_config(config, frozen):
    frozen = validate(frozen)
    return replace(config, assistant_billing_mode=MODE, assistant_provider_id=frozen['provider_id'],
        assistant_model=frozen['upstream_model'], assistant_pricing_version=frozen['pricing_snapshot']['version'],
        assistant_images=False, assistant_models=(), efforts=(),
        **{'assistant_' + name: value for name, value in frozen['limits'].items()})


def public_snapshot(internal, *, expected_digest, effort_policy):
    frozen = validate(internal, expected_digest=expected_digest, effort_policy=effort_policy, check_effort=True)
    from .efforts import public_policy_snapshot
    frozen['schema_version'] = 2
    frozen['pricing_snapshot'] = public_price_snapshot(frozen['pricing_snapshot'])
    frozen['pricing_digest'] = contract.digest(frozen['pricing_snapshot'])
    policy = public_policy_snapshot(effort_policy)
    frozen['effort_digest'] = contract.digest(policy) if policy is not None else None
    return frozen
