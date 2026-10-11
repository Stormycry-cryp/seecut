"""Public model aliases; all provider routing remains server controlled."""
from __future__ import annotations

import json
import re
from dataclasses import replace

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
                    not item.keys() <= {'id', 'label', 'model', 'pricing_version'} | LIMITS.keys():
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
            seen.add(identity)
            result.append(dict(item))
        return tuple(result)
    except (ValueError, TypeError, OverflowError) as error:
        raise ValueError('Invalid Agent model catalog') from error


def model_config(config, specification):
    fields = {'assistant_model': specification['model']}
    for key in (*LIMITS, 'pricing_version'):
        if key in specification:
            fields['assistant_' + key] = specification[key]
    value = replace(config, **fields, assistant_models=())
    if value.assistant_max_output_tokens >= value.assistant_context_window:
        raise ValueError('Invalid Agent model limits')
    return value
