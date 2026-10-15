"""Exact pricing/cap tests against synthetic contract records only."""
import copy
import json
import unittest
from seecut_server.model_pricing import (
    PriceUnavailable, capped_capture, parse_price_catalog, price_snapshot, token_charge, validate_price_snapshot)


def record(overrides=None):
    value = dict(provider='synthetic', model='synthetic-exact', kind='agent', version='synthetic-cost-v1',
        cost_status='verified', source='synthetic-fixture-contract', verified_at='2026-10-12T00:00:00Z',
        costs={name: dict(unit='per_million_tokens', yuan=cost, selectors={})
               for name, cost in [('input', '0.8'), ('cached_input', '0.23'), ('output', '2.8')]})
    if overrides:
        value['sale_overrides'] = overrides
    return value


def price(overrides=None):
    return price_snapshot(parse_price_catalog(json.dumps([record(overrides)])), 'synthetic', 'synthetic-exact')


class ExactModelPriceTest(unittest.TestCase):
    def test_cost_times_three_and_cache_are_exact(self):
        snapshot = price()
        self.assertEqual(snapshot['denominator'], '1')
        self.assertEqual({key: row['rate_numerator'] for key, row in snapshot['dimensions'].items()},
                         {'input': '24000', 'cached_input': '6900', 'output': '84000'})
        self.assertEqual(token_charge(snapshot, dict(input=10, cached_input=5, output=2)), 322500)

    def test_reference_or_unknown_never_becomes_account_cost(self):
        for status in ('reference_only', 'unknown'):
            value = record(); value['cost_status'] = status
            catalog = parse_price_catalog(json.dumps([value]))
            with self.assertRaises(PriceUnavailable):
                price_snapshot(catalog, 'synthetic', 'synthetic-exact')
        with self.assertRaises(PriceUnavailable):
            price_snapshot(parse_price_catalog(json.dumps([record()])), 'other-provider', 'synthetic-exact')

    def test_fractional_nanocredit_rates_remain_exact(self):
        value = record(); value['costs']['input']['yuan'] = '0.000000000001'
        snapshot = price_snapshot(parse_price_catalog(json.dumps([value])), 'synthetic', 'synthetic-exact')
        self.assertEqual(snapshot['denominator'], '100000000')
        first = capped_capture(snapshot, 0, 0, 10, dict(input=1, cached_input=0, output=0))
        second = capped_capture(snapshot, int(first['accrued_numerator']), first['captured_credits'], 10,
                                dict(input=1, cached_input=0, output=0))
        self.assertEqual((first['accrued_numerator'], second['accrued_numerator']), ('3', '6'))
        self.assertEqual((first['captured_credits'], second['captured_credits']), (1, 0))
        self.assertEqual(second['accrued_microcredits'], 1)

    def test_two_point_two_steps_and_point_six_steps_round_whole_task(self):
        snapshot = price({'input': '20', 'output': '60'})
        for count, total in ((10000, 1), (30000, 2)):
            first = capped_capture(snapshot, 0, 0, 10, dict(input=count, cached_input=0, output=0))
            second = capped_capture(snapshot, int(first['accrued_numerator']), first['captured_credits'], 10,
                                    dict(input=count, cached_input=0, output=0))
            self.assertEqual(first['captured_credits'] + second['captured_credits'], total)

    def test_zero_exact_cap_and_exceeded_difference(self):
        snapshot = price()
        zero = capped_capture(snapshot, 0, 0, 1, dict(input=0, cached_input=0, output=0))
        self.assertEqual((zero['captured_credits'], zero['accrued_numerator']), (0, '0'))
        exact_price = price({'input': '20'})
        exact = capped_capture(exact_price, 0, 0, 1, dict(input=50000, cached_input=0, output=0))
        self.assertEqual((exact['captured_credits'], exact['exhausted'], exact['exceeded']), (1, True, False))
        exceeded = capped_capture(snapshot, 0, 0, 1, dict(input=2_000_000, cached_input=0, output=0))
        self.assertEqual((exceeded['captured_credits'], exceeded['reported_numerator'],
                          exceeded['uncollected_numerator'], exceeded['accrued_microcredits']),
                         (1, '48000000000', '47000000000', 1_000_000))
        self.assertTrue(exceeded['exhausted'] and exceeded['exceeded'])

    def test_missing_cache_breakdown_and_tampered_markup_rejected(self):
        snapshot = price()
        for usage in (dict(input=10, output=0), dict(input=10, cached_input=11, output=0),
                      dict(input=True, cached_input=0, output=0)):
            with self.assertRaises(ValueError):
                token_charge(snapshot, usage)
        tampered = copy.deepcopy(snapshot); tampered['dimensions']['cached_input']['rate_numerator'] = '7000'
        with self.assertRaises(ValueError):
            validate_price_snapshot(tampered)
        tampered = copy.deepcopy(snapshot)
        tampered['dimensions']['input'].update(retail_source='override', rate_numerator='0')
        with self.assertRaises(ValueError):
            validate_price_snapshot(tampered)
        tampered = price({'input': '20'})
        tampered['dimensions']['input']['sale_override_credits'] = '21'
        with self.assertRaises(ValueError):
            validate_price_snapshot(tampered)
        value = record(); value['costs']['input']['yuan'] = 0.8
        with self.assertRaises(ValueError):
            parse_price_catalog(json.dumps([value]))


if __name__ == '__main__':
    unittest.main()
