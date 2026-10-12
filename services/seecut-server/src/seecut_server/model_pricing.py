"""Versioned, exact supplier costs and model-specific retail prices.

No reference price becomes an account procurement cost implicitly. All values are
decimal strings or integers; snapshots are immutable JSON and never contain keys.
"""
from __future__ import annotations

from copy import deepcopy
from datetime import datetime
from decimal import Decimal, InvalidOperation
from fractions import Fraction
from math import lcm
import json
import re

NANOCREDITS = 1_000_000_000
CREDITS_PER_YUAN = 10
DEFAULT_MULTIPLIER = 3
MODES = ('verified_input_v1', 'provider_usage_capped_v1')
IDENTITY = re.compile(r'[A-Za-z0-9_.:-]{1,128}\Z')
DECIMAL = re.compile(r'(?:0|[1-9][0-9]{0,8})(?:\.[0-9]{1,12})?\Z')
TOKEN_DIMENSIONS = {'input': 'input_tokens', 'cached_input': 'cached_input_tokens', 'output': 'output_tokens'}
UNITS = {'per_million_tokens': 1_000_000, 'per_image': 1, 'per_second': 1, 'per_millisecond': 1}


class PriceUnavailable(ValueError):
    pass


def billing_mode(value):
    if not isinstance(value, str) or value not in MODES:
        raise ValueError('Unknown Agent billing mode')
    return value


def decimal_fraction(value):
    if not isinstance(value, str) or not DECIMAL.fullmatch(value):
        raise ValueError('Price must be a bounded exact decimal string')
    try:
        result = Fraction(Decimal(value))
    except (InvalidOperation, ValueError, OverflowError) as error:
        raise ValueError('Invalid model price') from error
    if result < 0:
        raise ValueError('Negative model price')
    return result


def ceil_ratio(numerator, denominator):
    if type(numerator) is not int or type(denominator) is not int or numerator < 0 or denominator <= 0:
        raise ValueError('Invalid exact amount')
    return (numerator + denominator - 1) // denominator


def parse_price_catalog(raw):
    try:
        if not isinstance(raw, str) or len(raw.encode()) > 131072:
            raise ValueError
        records = json.loads(raw)
        if not isinstance(records, list) or len(records) > 256:
            raise ValueError
        seen = set()
        for record in records:
            required = {'provider', 'model', 'kind', 'version', 'cost_status', 'source', 'verified_at', 'costs'}
            if not isinstance(record, dict) or not required <= record.keys() or record.keys() - required - {'sale_overrides'}:
                raise ValueError
            if not all(isinstance(record[name], str) and IDENTITY.fullmatch(record[name]) for name in ('provider', 'version')):
                raise ValueError
            if not isinstance(record['model'], str) or not record['model'] or len(record['model']) > 256 or '\0' in record['model']:
                raise ValueError
            if record['kind'] not in ('agent', 'image', 'video') or record['cost_status'] not in ('unknown', 'reference_only', 'verified', 'verified_free'):
                raise ValueError
            identity = record['provider'], record['model'], record['kind']
            if identity in seen:
                raise ValueError
            seen.add(identity)
            if not isinstance(record['source'], str) or len(record['source']) > 2048 or any(ord(char) < 32 for char in record['source']):
                raise ValueError
            if record['verified_at'] is not None:
                if not isinstance(record['verified_at'], str) or not re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,6})?(?:Z|[+-][0-9]{2}:[0-9]{2})', record['verified_at']):
                    raise ValueError
                stamp = datetime.fromisoformat(record['verified_at'].replace('Z', '+00:00'))
                if stamp.tzinfo is None:
                    raise ValueError
            verified = record['cost_status'] in ('verified', 'verified_free')
            if verified and (not record['source'] or record['verified_at'] is None):
                raise ValueError
            costs = record['costs']
            if not isinstance(costs, dict) or len(costs) > 32 or verified and not costs:
                raise ValueError
            for dimension, cost in costs.items():
                if not isinstance(dimension, str) or not IDENTITY.fullmatch(dimension) or not isinstance(cost, dict):
                    raise ValueError
                if set(cost) != {'unit', 'yuan', 'selectors'} or cost['unit'] not in UNITS:
                    raise ValueError
                amount = decimal_fraction(cost['yuan'])
                if record['cost_status'] == 'verified_free' and amount:
                    raise ValueError
                selectors = cost['selectors']
                if not isinstance(selectors, dict) or len(selectors) > 16 or any(
                    not isinstance(key, str) or not IDENTITY.fullmatch(key) or
                    not isinstance(value, (str, int, bool)) or isinstance(value, str) and len(value) > 256
                    for key, value in selectors.items()
                ):
                    raise ValueError
            if record['kind'] == 'agent' and costs:
                if set(costs) != set(TOKEN_DIMENSIONS) or any(
                    cost['unit'] != 'per_million_tokens' or cost['selectors'] for cost in costs.values()
                ):
                    raise ValueError
            overrides = record.get('sale_overrides', {})
            if not isinstance(overrides, dict) or not overrides.keys() <= costs.keys():
                raise ValueError
            for value in overrides.values():
                decimal_fraction(value)  # credits per the dimension's unit
        return tuple(deepcopy(records))
    except (ValueError, TypeError, AttributeError, OverflowError) as error:
        raise ValueError('Invalid verified model price catalog') from error


def price_snapshot(records, provider, model, kind='agent'):
    record = next((item for item in records if
                   (item['provider'], item['model'], item['kind']) == (provider, model, kind)), None)
    if record is None or record['cost_status'] not in ('verified', 'verified_free'):
        raise PriceUnavailable('Account procurement cost not verified for this exact model')
    rates = {}
    for dimension, cost in record['costs'].items():
        sale = decimal_fraction(record.get('sale_overrides', {}).get(dimension)) if dimension in record.get('sale_overrides', {}) else decimal_fraction(cost['yuan']) * DEFAULT_MULTIPLIER * CREDITS_PER_YUAN
        rates[dimension] = sale * NANOCREDITS / UNITS[cost['unit']]
    denominator = lcm(*(value.denominator for value in rates.values()))
    if denominator > 10 ** 18:
        raise ValueError('Price precision exceeds exact supported range')
    snapshot = dict(schema_version=1, provider=provider, model=model, kind=kind, version=record['version'],
                    cost_status=record['cost_status'], source=record['source'], verified_at=record['verified_at'],
                    credits_per_yuan=CREDITS_PER_YUAN, multiplier=DEFAULT_MULTIPLIER,
                    unit='nanocredits', denominator=str(denominator), dimensions={})
    for dimension, rate in rates.items():
        cost = record['costs'][dimension]
        snapshot['dimensions'][dimension] = dict(
            unit=cost['unit'], cost_yuan=cost['yuan'], selectors=deepcopy(cost['selectors']),
            retail_source='override' if dimension in record.get('sale_overrides', {}) else 'cost_times_three',
            sale_override_credits=record.get('sale_overrides', {}).get(dimension),
            rate_numerator=str(rate.numerator * (denominator // rate.denominator)))
    return snapshot


def validate_price_snapshot(snapshot):
    required = {'schema_version', 'provider', 'model', 'kind', 'version', 'cost_status', 'source', 'verified_at',
                'credits_per_yuan', 'multiplier', 'unit', 'denominator', 'dimensions'}
    if not isinstance(snapshot, dict) or set(snapshot) != required or type(snapshot['schema_version']) is not int or snapshot['schema_version'] != 1 or snapshot['unit'] != 'nanocredits':
        raise ValueError('Invalid frozen model pricing')
    if not isinstance(snapshot['dimensions'], dict) or not snapshot['dimensions'] or any(
        not isinstance(value, dict) or set(value) != {'unit', 'cost_yuan', 'selectors', 'retail_source', 'sale_override_credits', 'rate_numerator'}
        for value in snapshot['dimensions'].values()
    ):
        raise ValueError('Invalid frozen price dimensions')
    # Revalidate all origin metadata and dimensions, without deriving away overrides.
    costs = {name: dict(unit=value['unit'], yuan=value['cost_yuan'], selectors=value['selectors'])
             for name, value in snapshot['dimensions'].items()}
    parse_price_catalog(json.dumps([dict(provider=snapshot['provider'], model=snapshot['model'], kind=snapshot['kind'],
        version=snapshot['version'], cost_status=snapshot['cost_status'], source=snapshot['source'],
        verified_at=snapshot['verified_at'], costs=costs)]))
    if snapshot['cost_status'] not in ('verified', 'verified_free') or snapshot['credits_per_yuan'] != 10 or snapshot['multiplier'] != 3:
        raise ValueError('Unverified frozen model pricing')
    denominator = snapshot['denominator']
    if not isinstance(denominator, str) or not re.fullmatch(r'[1-9][0-9]{0,18}', denominator) or int(denominator) > 10 ** 18:
        raise ValueError('Invalid frozen price precision')
    for value in snapshot['dimensions'].values():
        if value['retail_source'] not in ('override', 'cost_times_three'):
            raise ValueError('Invalid frozen price dimension')
        numerator = value['rate_numerator']
        if not isinstance(numerator, str) or not re.fullmatch(r'0|[1-9][0-9]{0,44}', numerator):
            raise ValueError('Invalid exact frozen price')
        if value['retail_source'] == 'cost_times_three':
            if value['sale_override_credits'] is not None:
                raise ValueError('Unexpected frozen price override')
            expected = decimal_fraction(value['cost_yuan']) * 30 * NANOCREDITS / UNITS[value['unit']]
        else:
            expected = decimal_fraction(value['sale_override_credits']) * NANOCREDITS / UNITS[value['unit']]
        if Fraction(int(numerator), int(denominator)) != expected:
            raise ValueError('Frozen retail price mismatch')
    return deepcopy(snapshot)


def token_charge(snapshot, usage):
    snapshot = validate_price_snapshot(snapshot)
    if snapshot['kind'] != 'agent':
        raise ValueError('Token price needed')
    if not isinstance(usage, dict) or set(usage) != {'input', 'cached_input', 'output'} or any(
        type(value) is not int or not 0 <= value <= 2_000_000 for value in usage.values()
    ) or usage['cached_input'] > usage['input']:
        raise ValueError('Provider cache classification unverified')
    counts = {'input': usage['input'] - usage['cached_input'], 'cached_input': usage['cached_input'], 'output': usage['output']}
    return sum(counts[name] * int(snapshot['dimensions'][name]['rate_numerator']) for name in TOKEN_DIMENSIONS)


def generation_charge(snapshot, request):
    """One exact matching size/duration price, then one item quote ceiling."""
    snapshot = validate_price_snapshot(snapshot)
    if snapshot['kind'] not in ('image', 'video') or not isinstance(request, dict):
        raise ValueError('Generation price needed')
    matches = [(name, value) for name, value in snapshot['dimensions'].items()
               if all(request.get(key) == expected for key, expected in value['selectors'].items())]
    if len(matches) != 1:
        raise PriceUnavailable('Generation parameter cost missing or ambiguous')
    dimension, price = matches[0]
    quantity = Fraction(1)
    if snapshot['kind'] == 'image':
        if price['unit'] != 'per_image':
            raise PriceUnavailable('Image cost unit not supported')
    else:
        if price['unit'] not in ('per_second', 'per_millisecond'):
            raise PriceUnavailable('Video cost unit not supported')
        duration = request.get('duration')
        if type(duration) is int:
            duration = str(duration)
        quantity = decimal_fraction(duration)
        if not 0 < quantity <= 3600:
            raise ValueError('Invalid priced video duration')
        if price['unit'] == 'per_millisecond':
            quantity *= 1000
    amount = quantity * Fraction(int(price['rate_numerator']), int(snapshot['denominator']))
    return dict(dimension=dimension, reported_numerator=str(amount.numerator), denominator=str(amount.denominator),
                unit_credits=ceil_ratio(amount.numerator, amount.denominator * NANOCREDITS))


def capped_capture(snapshot, accrued_numerator, spent, max_credits, usage):
    snapshot = validate_price_snapshot(snapshot)
    denominator = int(snapshot['denominator'])
    values = (accrued_numerator, spent, max_credits)
    if any(type(value) is not int or value < 0 for value in values) or max_credits == 0:
        raise ValueError('Invalid capped task accounting')
    if ceil_ratio(accrued_numerator, denominator * NANOCREDITS) != spent:
        raise ValueError('Task amount and spent mismatch')
    cap = max_credits * denominator * NANOCREDITS
    if accrued_numerator > cap:
        raise ValueError('Task already exceeds authorization')
    raw = token_charge(snapshot, usage)
    total = min(cap, accrued_numerator + raw)
    chargeable = total - accrued_numerator
    due = ceil_ratio(total, denominator * NANOCREDITS)
    return dict(captured_credits=due - spent, accrued_numerator=str(total),
                accrued_microcredits=ceil_ratio(total, denominator * 1000),
                reported_numerator=str(raw), uncollected_numerator=str(raw - chargeable),
                denominator=str(denominator), exhausted=total == cap, exceeded=accrued_numerator + raw > cap)
