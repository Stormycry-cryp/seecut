"""Opt-in per-model generation procurement costs; no supplier requests."""
from dataclasses import replace
import json
from seecut_server.model_pricing import parse_price_catalog, generation_charge, price_snapshot
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture
from test_model_pricing import record
from test_service import FakeEmailSender, FakeImage2, FakeXiangxin


def generation_record(kind='image'):
    value = record()
    value.update(provider='image2' if kind == 'image' else 'xiangxin',
        model='gpt-image-2.5-flare' if kind == 'image' else 'sd_2.0_mini_special', kind=kind)
    value['costs'] = {'size_or_time': dict(unit='per_image' if kind == 'image' else 'per_second',
        yuan='0.1', selectors={} if kind == 'image' else dict(resolution='720p'))}
    return value


class GenerationCostPricingTest(AssistantFixture):
    def install(self, records):
        self.config = replace(self.config, model_cost_pricing_enabled=True,
            model_price_catalog=parse_price_catalog(json.dumps(records)), model_prices={'image:gpt-image-2.5-flare': 0})
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
            image2=FakeImage2(), xiangxin=FakeXiangxin(), assistant_provider=self.provider, clock=lambda: self.clock[0])

    def test_unknown_cost_cannot_sell_existing_zero_price(self):
        self.install([])
        model = next(m for m in self.account.generation_models()['models'] if m['id'] == 'gpt-image-2.5-flare')
        self.assertEqual((model['available'], model['reason_code']), (False, 'MODEL_COST_UNVERIFIED'))
        self.expect_error('MODEL_COST_UNVERIFIED', lambda: self.account.quote_generation(self.user,
            dict(kind='image', model='gpt-image-2.5-flare', prompt='synthetic')))
        self.wallet(100, 0)

    def test_cost_times_three_quote_and_replay_freeze_original_price(self):
        self.install([generation_record()])
        request = dict(model='gpt-image-2.5-flare', prompt='synthetic')
        quote = self.account.quote_generation(self.user, dict(kind='image', **request))
        self.assertEqual(quote['credits'], 3)
        frozen = quote['request']['model_pricing']
        changed = generation_record(); changed['costs']['size_or_time']['yuan'] = '0.2'; changed['version'] = 'new-cost'
        self.install([changed])
        op = self.account.create_generation_task(self.user, 'image', request | dict(quote_id=quote['quote_id']), 'synthetic-generation')
        with self.db.connect() as c:
            row = c.execute('SELECT * FROM generation_tasks WHERE id=?', (op['id'],)).fetchone()
            self.assertEqual(row['quoted_credits'], 3)
            self.assertEqual(json.loads(row['request_json'])['model_pricing'], frozen)
        before = self.account.wallet(self.user)
        replay = self.account.create_generation_task(self.user, 'image', request | dict(quote_id=quote['quote_id']), 'synthetic-generation')
        self.assertEqual(replay['id'], op['id']); self.assertEqual(self.account.wallet(self.user), before)

    def test_video_duration_and_model_override_remain_exact(self):
        value = generation_record('video'); value['sale_overrides'] = dict(size_or_time='2.5')
        snapshot = price_snapshot(parse_price_catalog(json.dumps([value])), value['provider'], value['model'], 'video')
        charged = generation_charge(snapshot, dict(resolution='720p', duration=5))
        self.assertEqual((charged['reported_numerator'], charged['unit_credits']), ('12500000000', 13))
        with self.assertRaises(ValueError):
            generation_charge(snapshot, dict(resolution='1080p', duration=5))

    def test_confirmed_free_is_separate_from_reference_only(self):
        value = generation_record(); value['cost_status'] = 'verified_free'; value['costs']['size_or_time']['yuan'] = '0'
        self.install([value])
        quote = self.account.quote_generation(self.user, dict(kind='image', model=value['model'], prompt='synthetic'))
        self.assertEqual(quote['credits'], 0)
        value['cost_status'] = 'reference_only'; self.install([value])
        self.expect_error('MODEL_COST_UNVERIFIED', lambda: self.account.quote_generation(self.user, dict(kind='image', model=value['model'], prompt='synthetic')))
