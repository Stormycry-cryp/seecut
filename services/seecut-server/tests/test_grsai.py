"""Controlled contracts only: no credentials, provider requests or paid calls."""
from dataclasses import replace
import io
import json
import sqlite3
import urllib.error
from email.message import Message
from unittest.mock import patch

from seecut_server.catalog import public_catalog, validate_request
from seecut_server.grsai import GrsaiClient, GrsaiError
from seecut_server.model_pricing import parse_price_catalog
from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture
from test_generation_robustness import PNG
from test_model_pricing import record
from test_service import FakeEmailSender


class FakeGrsai:
    configured = True

    def __init__(self):
        self.posts, self.polls = [], []
        self.failure = None
        self.status = 'running'

    def create(self, body, references):
        self.posts.append((body, references))
        if self.failure:
            raise self.failure
        return dict(id='original-upstream', status='running', results=[])

    def result(self, identity):
        self.polls.append(identity)
        return dict(id=identity, status=self.status,
                    results=[dict(url='https://example.test/result')] if self.status == 'succeeded' else [])


class GrsaiContractsTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        self.grsai = FakeGrsai()
        prices = []
        for model, kind in [('nano-banana-2-lite', 'image'), ('minimax-h3', 'video')]:
            price = record()
            price.update(provider='grsai', model=model, kind=kind,
                         costs={'media': dict(unit='per_image' if kind == 'image' else 'per_second', yuan='0.02', selectors={})})
            prices.append(price)
        self.config = replace(self.config, model_cost_pricing_enabled=True,
                              model_price_catalog=parse_price_catalog(json.dumps(prices)))
        self.account = SeeCutService(self.config, self.db, emailer=FakeEmailSender(self.config),
                                    grsai=self.grsai, assistant_provider=self.provider, clock=lambda: self.clock[0])

    def create(self, kind='image', quantity=1, key='original-request'):
        request = dict(model='nano-banana-fast' if kind == 'image' else 'minimax-h3', prompt='synthetic fixture', quantity=quantity)
        quote = self.account.quote_generation(self.user, kind, request)
        payload = dict(request, quote_id=quote['quote_id'])
        return self.account.create_generation_task(self.user, kind, payload, key), payload

    def process(self):
        tasks = self.account.claim_generation_tasks()
        self.assertEqual(len(tasks), 1)
        self.account.process_generation_task(tasks[0])

    def test_catalog_aliases_distinct_channels_and_maintenance(self):
        rows = public_catalog()['models']
        ids = [r['id'] for r in rows]
        self.assertEqual(len(ids), len(set(ids)))
        self.assertNotIn('nano-banana-fast', ids)
        self.assertNotIn('gpt-image-2.5-sunburst', ids)
        self.assertEqual(next(r for r in rows if r['id'] == 'gpt-image-2.5-flare')['provider'], 'image2')
        self.assertTrue({'nano-banana-2-cl', 'nano-banana-2-2k-cl', 'nano-banana-2-4k-cl'} <= set(ids))
        model, request = validate_request('image', dict(model='nano-banana-fast', prompt='draft'))
        self.assertEqual((model['id'], request['model']), ('nano-banana-2-lite', 'nano-banana-2-lite'))

    def test_contract_rejects_unsupported_audio_frames_and_duration(self):
        for extra in [dict(generate_audio=True), dict(first_frame='anything'), dict(resolution='1080p', duration=11)]:
            with self.subTest(extra=extra), self.assertRaises(Exception):
                validate_request('video', dict(model='minimax-h3', prompt='test', **extra))
        self.assertNotIn('generate_audio', next(m for m in public_catalog()['models'] if m['id'] == 'agnes-video-2.5')['parameters'])

    def test_explicit_image2_fast_override_and_original_replay_never_change_provider(self):
        from test_generation_efforts import RecordingImage2
        price = record()
        price.update(provider='image2', model='nano-banana-fast', kind='image',
                     costs={'media': dict(unit='per_image', yuan='0.02', selectors={})})
        config = replace(self.config, image2_model='nano-banana-fast',
                         model_price_catalog=parse_price_catalog(json.dumps([price])))
        image2 = RecordingImage2()
        service = SeeCutService(config, self.db, emailer=FakeEmailSender(config), image2=image2,
                                grsai=self.grsai, assistant_provider=self.provider, clock=lambda: self.clock[0])
        model = next(m for m in service.generation_models()['models'] if m['id'] == 'nano-banana-fast')
        self.assertEqual(model['provider'], 'image2')
        request = dict(model='nano-banana-fast', prompt='old draft', size='1024x1024', quality='high')
        quote = service.quote_generation(self.user, 'image', request)
        self.assertEqual(quote['request']['model'], 'nano-banana-fast')
        payload = request | dict(quote_id=quote['quote_id'])
        original = service.create_generation_task(self.user, 'image', payload, 'old-image2-fast')
        service.process_generation_task(service.claim_generation_tasks()[0])
        self.assertEqual(image2.payloads[0]['model'], 'nano-banana-fast')
        self.assertEqual(self.grsai.posts, [])
        service.config = replace(config, image2_model='new-image2-route', model_price_catalog=())
        replay = service.create_generation_task(self.user, 'image', payload, 'old-image2-fast')
        self.assertEqual(replay['id'], original['id'])
        self.assertEqual(replay['status'], 'succeeded')

    def test_unverified_cost_and_retail_override_are_unavailable_before_submission(self):
        price = record({'media': '1'})
        price.update(provider='grsai', model='nano-banana-2-lite', kind='image',
                     costs={'media': dict(unit='per_image', yuan='0.02', selectors={})})
        for records in ([], [price]):
            self.account.config = replace(self.config, model_price_catalog=parse_price_catalog(json.dumps(records)))
            model = next(m for m in self.account.generation_models()['models'] if m['id'] == 'nano-banana-2-lite')
            self.assertFalse(model['available'])
            self.expect_error('MODEL_COST_UNVERIFIED', lambda: self.create())
        self.assertEqual(self.grsai.posts, [])

    def test_paid_post_once_unknown_without_id_never_requeued(self):
        task, payload = self.create()
        self.grsai.failure = GrsaiError('GRSAI_RESPONSE_UNKNOWN', 'unknown')
        self.process()
        self.clock[0] += 1000
        self.assertEqual(self.account.claim_generation_tasks(), [])
        replay = self.account.create_generation_task(self.user, 'image', payload, 'original-request')
        self.assertEqual(replay['id'], task['id'])
        self.assertEqual(replay['status'], 'pending_reconcile')
        self.assertEqual(len(self.grsai.posts), 1)
        self.assertEqual(replay['captured_credits'], 0)

    def test_image_async_poll_download_retry_and_actual_batch_delta(self):
        task, _ = self.create(quantity=2)
        self.assertEqual(task['billing_captured_credits'], 0)
        self.process()
        self.process()  # second child, independently submitted once
        self.clock[0] += 11
        self.grsai.status = 'succeeded'
        with patch.object(self.account, '_download_provider_result', side_effect=RuntimeError('download unavailable')):
            self.process()
        self.clock[0] += 301
        with patch.object(self.account, '_download_provider_result', return_value=(PNG, 'image/png')), patch.object(self.account, '_probe_media'):
            self.process()
            self.clock[0] += 301
            self.process()
        completed = self.account.get_generation_task_by_request(self.user, 'original-request')
        self.assertEqual({item['status'] for item in completed['tasks']}, {'succeeded'})
        self.assertEqual(sum(item['captured_credits'] for item in completed['tasks']), 2)  # ceil(2 * .02 * 3 * 10)
        self.assertEqual(completed['billing_captured_credits'], 2)
        self.assertEqual(len(self.grsai.posts), 2)
        self.assertEqual(len(self.grsai.polls), 2)  # completed receipt reused after failed download
        self.assertEqual({item for item in self.grsai.polls}, {'original-upstream'})

    def test_failure_does_not_invent_refund_or_final_free_result(self):
        task, _ = self.create(kind='video')
        self.process()
        self.clock[0] += 11
        self.grsai.status = 'failed'
        self.process()
        current = self.account.get_generation_task(self.user, task['id'])
        self.assertEqual(current['status'], 'pending_reconcile')
        self.assertEqual(current['captured_credits'], 0)
        self.assertGreater(self.account.wallet(self.user)['held_credits'], 0)

    def test_transport_uses_exact_route_no_key_in_json_and_no_automatic_retry(self):
        client = GrsaiClient(replace(self.config, grsai_api_key='synthetic-not-a-secret'))
        calls = []
        def send(request, **kwargs):
            calls.append(request)
            response = io.BytesIO(json.dumps(dict(id='original', status='running')).encode())
            return response
        with patch('seecut_server.grsai.open_no_redirect', side_effect=send):
            _, body = validate_request('video', dict(model='minimax-h3', prompt='motion', aspect_ratio='9:16'))
            client.create(body, [('image', 'https://example.test/input')])
            client.result('original?&')
        data = json.loads(calls[0].data)
        self.assertEqual((data['aspectRatio'], data['replyType']), ('portrait', 'async'))
        self.assertNotIn('generate_audio', data)
        self.assertNotIn('synthetic-not-a-secret', calls[0].data.decode())
        self.assertTrue(calls[1].full_url.endswith('id=original%3F%26'))

    def test_existing_provider_constraint_migrates_with_rows_and_foreign_keys(self):
        # Preserve real account/task/output rows across the provider constraint upgrade.
        from seecut_server.db import Database
        old_schema = self.db.schema_path.read_text().replace("'image2', 'xiangxin', 'grsai'", "'image2', 'xiangxin'")
        old_path = self.config.database_path.with_name('old.sqlite')
        with sqlite3.connect(old_path) as connection:
            connection.executescript(old_schema)
            connection.execute("INSERT INTO users(id,email,password_hash,created_at,updated_at) VALUES('owner','fixture@example.test','synthetic',1,1)")
            connection.execute("""INSERT INTO generation_tasks(id,user_id,kind,provider,operation,model,prompt,request_json,idempotency_key,quoted_credits,status,next_attempt_at,created_at,updated_at)
                VALUES('old-task','owner','image','image2','generate','old-model','fixture','{}','old-request',3,'succeeded',0,1,1)""")
            connection.execute("""INSERT INTO generation_outputs(id,task_id,object_key,content_type,size_bytes,sha256,expires_at,created_at)
                VALUES('old-output','old-task','outputs/old','image/png',3,'original-sha',99,1)""")
        migrated = Database(old_path, self.db.schema_path)
        migrated.initialize()
        migrated.initialize()
        with migrated.connect() as connection:
            sql = connection.execute("SELECT sql FROM sqlite_master WHERE name='generation_tasks'").fetchone()[0]
            self.assertIn("'grsai'", sql)
            self.assertEqual(connection.execute("SELECT task_id,sha256 FROM generation_outputs WHERE id='old-output'").fetchone()[:], ('old-task', 'original-sha'))
            self.assertEqual(connection.execute("SELECT provider,quoted_credits FROM generation_tasks WHERE id='old-task'").fetchone()[:], ('image2', 3))
            self.assertEqual(connection.execute('PRAGMA foreign_key_check').fetchall(), [])

    def test_grsai_result_download_does_not_attach_other_provider_credentials(self):
        self.account.config = replace(self.config, xiangxin_base_url='https://example.test',
                                      xiangxin_api_key='synthetic-other-provider',
                                      provider_result_hosts=('example.test',))
        response = io.BytesIO(PNG)
        response.headers = Message()
        response.headers['Content-Type'] = 'image/png'
        with patch('seecut_server.service.open_no_redirect', return_value=response) as download:
            content, mime = self.account._download_provider_result(
                'https://example.test/result', 'image', authenticate_xiangxin=False)
        self.assertEqual((content, mime), (PNG, 'image/png'))
        self.assertFalse(download.call_args.args[0].has_header('Authorization'))
