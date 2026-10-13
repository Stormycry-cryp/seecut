"""Opt-in per-model generation procurement costs; no supplier requests."""
from dataclasses import replace
import io
import json
from unittest.mock import patch
from seecut_server.model_pricing import parse_price_catalog, generation_charge, price_snapshot
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture
from test_model_pricing import record
from test_service import FakeEmailSender
from test_generation_efforts import RecordingImage2, RecordingXiangxin
from test_generation_robustness import PNG


def generation_record(kind='image'):
    value = record()
    value.update(provider='image2' if kind == 'image' else 'xiangxin',
        model='gpt-image-2.5-flare' if kind == 'image' else 'sd_2.0_mini_special', kind=kind)
    value['costs'] = {'size_or_time': dict(unit='per_image' if kind == 'image' else 'per_second',
        yuan='0.1', selectors={} if kind == 'image' else dict(resolution='720p'))}
    return value


class GenerationCostPricingTest(AssistantFixture):
    def wallet(self, available, held):
        snapshot = self.account.wallet(self.user)
        self.assertEqual((snapshot['available_credits'], snapshot['held_credits']), (available, held))
        with self.db.connect() as connection:
            rows = list(connection.execute('SELECT * FROM ledger_entries WHERE user_id=?', (self.user,)))
        self.assertEqual((100 + sum(row['delta_available'] for row in rows), sum(row['delta_held'] for row in rows)), (available, held))
        return rows

    def install(self, records):
        self.config = replace(self.config, model_cost_pricing_enabled=True,
            model_price_catalog=parse_price_catalog(json.dumps(records)), model_prices={'image:gpt-image-2.5-flare': 0})
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
            image2=RecordingImage2(), xiangxin=RecordingXiangxin(), assistant_provider=self.provider, clock=lambda: self.clock[0])

    def test_dispatch_keeps_price_snapshot_private_for_image_create_edit_and_video(self):
        self.install([generation_record(), generation_record('video')])
        upload = self.account.prepare_upload(self.user, 'generation_input', 'reference.png', 'image/png', len(PNG))
        self.account.save_upload_stream(upload['upload_id'], io.BytesIO(PNG), len(PNG), 'image/png')
        asset = self.account.register_generation_asset(self.user, upload['upload_id'])
        requests = [
            ('image', dict(model='gpt-image-2.5-flare', prompt='synthetic')),
            ('image', dict(model='gpt-image-2.5-flare', prompt='synthetic edit', operation='edit', reference_asset_ids=[asset['id']])),
            ('video', dict(model='sd_2.0_mini_special', prompt='synthetic video', duration=5, resolution='720p', aspect_ratio='16:9', reference_asset_ids=[asset['id']])),
        ]
        for index, (kind, request) in enumerate(requests):
            with self.subTest(kind=kind, operation=request.get('operation', 'create')):
                quote = self.account.quote_generation(self.user, dict(kind=kind, **request))
                task = self.account.create_generation_task(self.user, kind, request | dict(quote_id=quote['quote_id']), f'outbound-{index}')
                with self.db.connect() as connection:
                    frozen = connection.execute('SELECT request_json FROM generation_tasks WHERE id=?', (task['id'],)).fetchone()[0]
                self.assertNotIn('model_pricing', quote['request'])
                self.assertIn('model_pricing', json.loads(frozen))
                with patch.object(self.account, '_store_binary_outputs'):
                    self.account.process_generation_task(self.account.claim_generation_tasks()[0])
                if kind == 'video':
                    sent = self.account.xiangxin.payloads[-1]
                    self.assertEqual(sent['reference_images'], ['assetId://provider-asset'])
                elif request.get('operation') == 'edit':
                    sent, images = self.account.image2.edit_calls[-1]
                    self.assertEqual(len(images), 1)
                else:
                    sent = self.account.image2.payloads[-1]
                self.assertEqual(sent['model'], request['model'])
                self.assertEqual(sent['prompt'], request['prompt'])
                self.assertFalse({'model_pricing', 'effort_id', 'policy_revision', 'policy', 'operation', 'quantity', 'reference_asset_ids'} & sent.keys())
                with self.db.connect() as connection:
                    after = connection.execute('SELECT request_json FROM generation_tasks WHERE id=?', (task['id'],)).fetchone()[0]
                self.assertEqual(after, frozen)

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
        self.assertNotIn('model_pricing', quote['request'])
        with self.db.connect() as connection:
            frozen = json.loads(connection.execute('SELECT canonical_json FROM generation_quotes WHERE id=?', (quote['quote_id'],)).fetchone()[0])['model_pricing']
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

    def batch(self, price, key='batch'):
        record = generation_record()
        record['sale_overrides'] = dict(size_or_time=price)
        self.install([record])
        request = dict(model=record['model'], prompt='batch fixture', quantity=2)
        quote = self.account.quote_generation(self.user, 'image', request)
        payload = request | dict(quote_id=quote['quote_id'])
        task = self.account.create_generation_task(self.user, 'image', payload, key)
        return task, quote, payload

    def succeed(self, task_id):
        with patch.object(self.account, '_probe_media'):
            self.account._store_binary_outputs(task_id, [PNG], ['image/png'])

    def test_parent_batch_ceil_uses_success_total_in_both_orders(self):
        for price, expected in [('0.2', [1, 1]), ('0.6', [1, 2])]:
            for reverse in (False, True):
                with self.subTest(price=price, reverse=reverse):
                    key = f'{price}-{reverse}'
                    task, quote, payload = self.batch(price, key)
                    ids = [child['id'] for child in task['tasks']]
                    if reverse: ids.reverse()
                    self.assertEqual(quote['credits'], expected[-1])
                    with self.db.connect() as c:
                        self.assertEqual(c.execute('SELECT COUNT(*) FROM wallet_holds WHERE task_id=?', (task['billing_id'],)).fetchone()[0], 1)
                    for task_id, cumulative in zip(ids, expected):
                        self.succeed(task_id)
                        current = self.account.get_generation_task(self.user, task_id)
                        self.assertEqual(current['billing_captured_credits'], cumulative)
                    before = self.account.wallet(self.user)
                    self.succeed(ids[0])
                    self.account._fail_and_release(ids[0], 'TEST', 'late failure')
                    replay = self.account.create_generation_task(self.user, 'image', payload, key)
                    self.assertEqual(sum(child['captured_credits'] for child in replay['tasks']), expected[-1])
                    self.assertEqual(self.account.wallet(self.user), before)
                    self.assertEqual(replay['billing_id'], task['billing_id'])

    def test_partial_failure_and_unknown_hold_only_remaining_upper_bound(self):
        task, _, _ = self.batch('0.6')
        first, second = [child['id'] for child in task['tasks']]
        self.succeed(first)
        self.account._set_task_status(second, 'pending_reconcile')
        self.wallet(98, 1)
        self.account._fail_and_release(second, 'NOT_EXECUTED', 'confirmed no charge')
        self.wallet(99, 0)
        self.succeed(second)  # A late callback cannot revive a refunded terminal task.
        self.wallet(99, 0)
        with self.db.connect() as c:
            bill = c.execute('SELECT * FROM generation_billings WHERE id=?', (task['billing_id'],)).fetchone()
            self.assertEqual((bill['captured_credits'], bill['released_credits'], bill['succeeded_count'], bill['failed_count']), (1, 1, 1, 1))
            self.assertEqual(c.execute('SELECT COUNT(*) FROM generation_outputs WHERE task_id=?', (second,)).fetchone()[0], 0)

    def test_all_fail_and_failure_before_success_settle_once(self):
        first_batch, _, _ = self.batch('0.2', 'all-fail')
        for task in first_batch['tasks']:
            self.account._fail_and_release(task['id'], 'NOT_EXECUTED', 'confirmed')
            self.account._fail_and_release(task['id'], 'NOT_EXECUTED', 'duplicate')
        self.wallet(100, 0)
        second_batch, _, _ = self.batch('0.6', 'failure-first')
        first, second = second_batch['tasks']
        self.account._fail_and_release(first['id'], 'NOT_EXECUTED', 'confirmed')
        self.wallet(99, 1)
        self.succeed(second['id'])
        self.wallet(99, 0)

    def test_legacy_quote_retains_per_item_holds_and_old_rounding(self):
        record = generation_record(); record['sale_overrides'] = dict(size_or_time='0.2')
        self.install([record])
        request = dict(model=record['model'], prompt='legacy quote', quantity=2)
        quote = self.account.quote_generation(self.user, 'image', request)
        with self.db.transaction() as c:
            c.execute("UPDATE generation_quotes SET rounding_mode='legacy_item',credits=2 WHERE id=?", (quote['quote_id'],))
        task = self.account.create_generation_task(self.user, 'image', request | dict(quote_id=quote['quote_id']), 'legacy')
        self.assertNotIn('billing_id', task)
        for child in task['tasks']: self.succeed(child['id'])
        self.wallet(98, 0)
        with self.db.connect() as c:
            self.assertEqual(c.execute('SELECT COUNT(*) FROM generation_billings').fetchone()[0], 0)
            self.assertEqual(c.execute('SELECT COUNT(*) FROM wallet_holds').fetchone()[0], 2)
