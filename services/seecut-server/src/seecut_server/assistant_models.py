"""Public model aliases; all provider routing remains server controlled."""
from __future__ import annotations

import json
import re
from dataclasses import replace

from .assistant_billing import rate_denominator
from .model_pricing import billing_mode

IDENTITY = re.compile(r"[A-Za-z0-9_.:-]{1,128}\Z")
LIMITS = {
    'input_rate': (0, 1_000_000), 'output_rate': (0, 1_000_000),
    'context_window': (1, 1_000_000), 'max_output_tokens': (1, 131072),
    'max_steps': (1, 8), 'max_turn_credits': (0, 1_000_000_000),
}


def parse_models(raw):
    """Validate at startup without echoing configuration values in errors."""
    try:
        value = json.loads(raw)
        if not isinstance(value, list) or len(value) > 16:
            raise ValueError
        result, seen = [], {'default'}
        for item in value:
            if not isinstance(item, dict) or not {'id', 'label', 'model'} <= item.keys() or \
                    not item.keys() <= {'id', 'label', 'model', 'pricing_version', 'rate_denominator', 'billing_mode', 'provider_id'} | LIMITS.keys():
                raise ValueError
            identity = item['id']
            if not isinstance(identity, str) or not IDENTITY.fullmatch(identity) or identity in seen:
                raise ValueError
            for key, limit in [('label', 64), ('model', 128)]:
                text = item[key]
                if not isinstance(text, str) or not text.strip() or len(text) > limit or \
                        any(ord(char) < 32 for char in text):
                    raise ValueError
            if 'pricing_version' in item and (not isinstance(item['pricing_version'], str) or
                    not IDENTITY.fullmatch(item['pricing_version'])):
                raise ValueError
            for key, (minimum, maximum) in LIMITS.items():
                if key in item and (type(item[key]) is not int or not minimum <= item[key] <= maximum):
                    raise ValueError
            if 'rate_denominator' in item:
                rate_denominator(item['rate_denominator'])
            if 'billing_mode' in item:
                billing_mode(item['billing_mode'])
            if 'provider_id' in item and (not isinstance(item['provider_id'], str) or not IDENTITY.fullmatch(item['provider_id'])):
                raise ValueError
            seen.add(identity)
            result.append(dict(item))
        return tuple(result)
    except (ValueError, TypeError, OverflowError) as error:
        raise ValueError('Invalid Agent model catalog') from error


def model_config(config, specification):
    # A legacy alias never silently inherits a million-token unit.
    denominator = rate_denominator(specification.get('rate_denominator', 1000))
    if denominator != config.assistant_rate_denominator and not {'input_rate', 'output_rate'} <= specification.keys():
        raise ValueError('Agent alias changing rate units requires both rates')
    fields = {'assistant_model': specification['model'], 'assistant_rate_denominator': denominator,
              'assistant_billing_mode': billing_mode(specification.get('billing_mode', 'verified_input_v1')),
              'assistant_provider_id': specification.get('provider_id', config.assistant_provider_id)}
    for key in (*LIMITS, 'pricing_version'):
        if key in specification:
            fields['assistant_' + key] = specification[key]
    value = replace(config, **fields, assistant_models=())
    if value.assistant_max_output_tokens >= value.assistant_context_window:
        raise ValueError('Invalid Agent model limits')
    return value
