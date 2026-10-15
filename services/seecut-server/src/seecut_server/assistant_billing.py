"""Integer-only Agent prices, frozen units and task-level cumulative rounding."""
from __future__ import annotations

from dataclasses import dataclass

MICROCREDITS = 1_000_000
RATE_DENOMINATORS = (1000, MICROCREDITS)


def rate_denominator(value):
    if type(value) is not int or value not in RATE_DENOMINATORS:
        raise ValueError('Invalid Agent rate denominator')
    return value


def _read(row, name, default):
    return row[name] if name in row.keys() else default


def _numerator(input_tokens, output_tokens, input_rate, output_rate):
    values = (input_tokens, output_tokens, input_rate, output_rate)
    if any(type(value) is not int or value < 0 for value in values):
        raise ValueError('Invalid Agent price or usage')
    return input_tokens * input_rate + output_tokens * output_rate


def credit_cost(input_tokens, output_tokens, input_rate, output_rate, denominator=1000):
    """Reserve or legacy settlement: round the combined price exactly once."""
    denominator = rate_denominator(denominator)
    numerator = _numerator(input_tokens, output_tokens, input_rate, output_rate)
    return (numerator + denominator - 1) // denominator


@dataclass(frozen=True)
class Capture:
    credits: int
    accrued_microcredits: int


def capture_for_usage(operation, budget, usage):
    """Pure calculation; caller commits this once with the original operation."""
    denominator = rate_denominator(_read(operation, 'rate_denominator', 1000))
    numerator = _numerator(usage['input'], usage['output'], operation['input_rate'], operation['output_rate'])
    mode = _read(budget, 'rounding_mode', 'legacy_operation')
    accrued = _read(budget, 'accrued_microcredits', 0)
    spent = budget['spent_credits']
    if any(type(value) is not int or value < 0 for value in (accrued, spent)):
        raise ValueError('Invalid Agent task accounting')
    if mode == 'legacy_operation':
        return Capture(credit_cost(usage['input'], usage['output'], operation['input_rate'], operation['output_rate'], denominator), accrued)
    if mode != 'task_cumulative_v1' or (accrued + MICROCREDITS - 1) // MICROCREDITS != spent:
        raise ValueError('Invalid Agent task accounting')
    total = accrued + numerator * (MICROCREDITS // denominator)
    if total > 1_000_000_000 * MICROCREDITS:
        raise ValueError('Agent task accounting exceeds limit')
    due = (total + MICROCREDITS - 1) // MICROCREDITS
    return Capture(due - spent, total)
