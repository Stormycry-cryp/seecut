"""Generation selections are server-owned and durable across catalog changes."""
from __future__ import annotations

from copy import deepcopy
from dataclasses import replace
from unittest.mock import patch

from seecut_server.service import SeeCutService
from test_assistant_contract import AssistantFixture
from test_generation_robustness import FakeImage2, FakeXiangxin
from test_service import FakeEmailSender


IMAGE_MODEL = 'gpt-image-2.5-flare'
VIDEO_MODEL = 'sd_2.0_mini_special'


def effort(revision='fixture-policy-1', *, images=True, videos=True):
    return {
        'id': 'balanced', 'label': '均衡', 'policy_revision': revision,
        'agent': {'model_id': 'default', 'max_output_tokens': 100,
                  'max_steps': 2, 'max_turn_credits': 8},
        'generation': {'image_model_ids': [IMAGE_MODEL] if images else [],
                       'video_model_ids': [VIDEO_MODEL] if videos else []},
    }


class RecordingImage2(FakeImage2):
    def __init__(self):
        super().__init__()
        self.payloads = []

    def create(self, body):
        self.payloads.append(deepcopy(body))
        return super().create(body)


class RecordingXiangxin(FakeXiangxin):
    def __init__(self):
        super().__init__()
        self.payloads = []

    def create_video(self, body):
        self.payloads.append(deepcopy(body))
        return super().create_video(body)


class GenerationEffortTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        self.config = replace(self.config, efforts=(effort(),), model_prices={
            'image:' + IMAGE_MODEL: 7, 'video:' + VIDEO_MODEL: 12,
        })
        self.image2 = RecordingImage2()
        self.xiangxin = RecordingXiangxin()
        self.reopen()

    def reopen(self):
        self.account = SeeCutService(
            self.config, self.db, emailer=FakeEmailSender(self.config),
            image2=self.image2, xiangxin=self.xiangxin,
            assistant_provider=self.provider, clock=lambda: self.clock[0],
        )

    def image_request(self, **changes):
        return {'model': IMAGE_MODEL, 'prompt': 'synthetic product',
                'effort_id': 'balanced', 'policy_revision': 'fixture-policy-1'} | changes

    def video_request(self, **changes):
        return self.image_request(model=VIDEO_MODEL, aspect_ratio='16:9') | changes

    def submit(self, request, key='generation-1', kind='image'):
        quote = self.account.quote_generation(self.user, kind, request)
        payload = request | {'quote_id': quote['quote_id']}
        return self.account.create_generation_task(self.user, kind, payload, key), payload, quote

    def task_counts(self):
        with self.db.connect() as connection:
            return tuple(connection.execute(f'SELECT COUNT(*) FROM {table}').fetchone()[0]
                         for table in ('generation_tasks', 'wallet_holds', 'ledger_entries'))

    def test_catalog_reports_actual_models_and_shared_effort_metadata(self):
        catalog = self.account.generation_models()
        self.assertEqual(catalog, self.account.generation_capabilities())
        self.assertEqual(catalog['effort_policy_version'], 1)
        self.assertEqual(catalog['default_effort_id'], 'balanced')
        models = {model['id']: model for model in catalog['models']}
        self.assertTrue(models[IMAGE_MODEL]['available'])
        self.assertTrue(models[VIDEO_MODEL]['available'])
        selected = catalog['efforts'][0]
        self.assertTrue(selected['available'])
        self.assertIsNone(selected['reason_code'])
        self.assertEqual(selected['generation']['image_model_ids'], [IMAGE_MODEL])
        self.assertEqual(selected['generation']['video_model_ids'], [VIDEO_MODEL])
        self.assertEqual(selected['agent']['model_id'], 'default')
        self.assertEqual(selected['agent']['max_steps'], 2)
        self.assertEqual(selected['agent']['pricing_version'], 'fixture-1')

        self.image2.configured = False
        catalog = self.account.generation_models()
        image = next(item for item in catalog['models'] if item['kind'] == 'image')
        self.assertFalse(image['available'])
        self.assertTrue(image['reason_code'])
        self.assertEqual(catalog['efforts'][0]['generation']['image_model_ids'], [])
        self.assertTrue(catalog['efforts'][0]['available'])
        self.xiangxin.configured = False
        selected = self.account.generation_models()['efforts'][0]
        self.assertFalse(selected['available'])
        self.assertTrue(selected['reason_code'])

    def test_unpriced_models_and_unverified_agent_are_unavailable(self):
        self.config = replace(self.config, model_prices={})
        self.reopen()
        catalog = self.account.generation_models()
        self.assertTrue(all(not model['available'] for model in catalog['models']))
        self.assertFalse(catalog['efforts'][0]['available'])
        self.config = replace(self.config, model_prices={'image:' + IMAGE_MODEL: 7})
        self.provider.configured = False
        self.reopen()
        selected = self.account.generation_models()['efforts'][0]
        self.assertFalse(selected['available'])
        self.expect_error('EFFORT_POLICY_UNAVAILABLE', lambda: self.account.quote_generation(
            self.user, 'image', self.image_request()))

    def test_new_request_requires_a_valid_pair_and_server_owned_policy(self):
        legacy = {'model': IMAGE_MODEL, 'prompt': 'legacy request'}
        self.expect_error('EFFORT_POLICY_REQUIRED', lambda: self.account.quote_generation(
            self.user, 'image', legacy))
        for changes in ({'effort_id': 'balanced'}, {'policy_revision': 'fixture-policy-1'},
                        {'effort_id': True, 'policy_revision': 'fixture-policy-1'},
                        {'effort_id': 'balanced', 'policy_revision': ''}, {'policy': {}}):
            with self.subTest(changes=changes):
                self.expect_error('EFFORT_POLICY_INVALID', lambda: self.account.quote_generation(
                    self.user, 'image', legacy | changes))
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.account.quote_generation(
            self.user, 'image', self.image_request(policy_revision='removed-revision')))
        self.assertEqual(self.task_counts(), (0, 0, 0))

    def test_effort_rejects_generation_model_outside_its_domain(self):
        self.config = replace(self.config, efforts=(effort(videos=False),))
        self.reopen()
        self.expect_error('EFFORT_POLICY_MISMATCH', lambda: self.account.quote_generation(
            self.user, 'video', self.video_request()))
        self.assertEqual(self.task_counts(), (0, 0, 0))

    def test_quote_task_and_image_dispatch_preserve_only_server_policy(self):
        task, _, quote = self.submit(self.image_request())
        for key in ('effort_id', 'policy_revision', 'policy'):
            self.assertEqual(quote[key], quote['request'][key])
            self.assertEqual(task['request'][key], quote[key])
        self.assertEqual(task['request']['policy']['agent']['max_steps'], 2)
        self.assertEqual(task['request']['policy']['agent']['pricing_version'], 'fixture-1')
        self.assertNotIn('available', task['request']['policy'])
        self.assertNotIn('reason_code', task['request']['policy'])
        with patch.object(self.account, '_store_binary_outputs'):
            self.account.process_generation_task(self.account.claim_generation_tasks()[0])
        self.assertEqual(self.image2.calls, 1)
        payload = self.image2.payloads[0]
        self.assertEqual(payload['model'], IMAGE_MODEL)
        self.assertFalse({'effort_id', 'policy_revision', 'policy'} & payload.keys())
        self.assertEqual(payload, {'model': IMAGE_MODEL, 'prompt': 'synthetic product',
                                  'size': 'auto', 'quality': 'high', 'n': 1, 'output_format': 'png'})

    def test_video_dispatch_strips_effort_metadata_without_second_submission(self):
        task, payload, _ = self.submit(self.video_request(), kind='video')
        self.account.process_generation_task(self.account.claim_generation_tasks()[0])
        self.assertEqual(self.xiangxin.submit_calls, 1)
        sent = self.xiangxin.payloads[0]
        self.assertEqual(sent['model'], VIDEO_MODEL)
        self.assertFalse({'effort_id', 'policy_revision', 'policy'} & sent.keys())
        restored = self.account.create_generation_task(self.user, 'video', payload, 'generation-1')
        self.assertEqual(restored['id'], task['id'])
        self.assertEqual(restored['status'], 'provider_accepted')
        self.clock[0] += 11
        self.account.process_generation_task(self.account.claim_generation_tasks()[0])
        self.assertEqual(self.xiangxin.submit_calls, 1)

    def test_changed_revision_rejects_quote_before_any_hold(self):
        request = self.image_request()
        quote = self.account.quote_generation(self.user, 'image', request)
        self.config = replace(self.config, efforts=(effort('fixture-policy-2'),))
        self.reopen()
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.account.create_generation_task(
            self.user, 'image', request | {'quote_id': quote['quote_id']}, 'new-task'))
        self.assertEqual(self.task_counts(), (0, 0, 0))
        self.assertEqual(self.account.wallet(self.user)['available_credits'], 100)

    def test_changed_snapshot_with_same_revision_still_invalidates_quote(self):
        request = self.image_request()
        quote = self.account.quote_generation(self.user, 'image', request)
        changed = effort()
        changed['agent']['max_steps'] = 1
        self.config = replace(self.config, efforts=(changed,))
        self.reopen()
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.account.create_generation_task(
            self.user, 'image', request | {'quote_id': quote['quote_id']}, 'new-task'))
        self.assertEqual(self.task_counts(), (0, 0, 0))

    def test_other_domain_outage_preserves_valid_quote_snapshot(self):
        request = self.image_request()
        quote = self.account.quote_generation(self.user, 'image', request)
        self.xiangxin.configured = False
        self.assertEqual(self.account.generation_models()['efforts'][0]['generation']['video_model_ids'], [])
        task = self.account.create_generation_task(
            self.user, 'image', request | {'quote_id': quote['quote_id']}, 'new-task')
        self.assertEqual(task['request']['policy'], quote['policy'])
        self.assertEqual(task['request']['policy']['generation']['video_model_ids'], [VIDEO_MODEL])
        self.assertEqual(self.task_counts(), (1, 1, 1))
        self.assertEqual(self.image2.calls, 0)

    def test_real_configuration_change_is_stale_even_when_public_policy_is_identical(self):
        self.xiangxin.configured = False
        request = self.image_request()
        quote = self.account.quote_generation(self.user, 'image', request)
        self.config = replace(self.config, efforts=(effort(videos=False),))
        self.reopen()
        current = self.account.generation_models()['efforts'][0]
        self.assertEqual({key: value for key, value in current.items()
                          if key not in {'available', 'reason_code'}}, quote['policy'])
        self.expect_error('EFFORT_POLICY_STALE', lambda: self.account.create_generation_task(
            self.user, 'image', request | {'quote_id': quote['quote_id']}, 'new-task'))
        self.assertEqual(self.task_counts(), (0, 0, 0))

    def test_quote_cannot_be_rebound_by_substituting_current_selection_metadata(self):
        request = self.image_request()
        quote = self.account.quote_generation(self.user, 'image', request)
        changed = effort('fixture-policy-2')
        alternate = effort()
        alternate['id'] = 'alternate'
        self.config = replace(self.config, efforts=(changed, alternate))
        self.reopen()
        for selection in ({'policy_revision': 'fixture-policy-2'}, {'effort_id': 'alternate'}):
            with self.subTest(selection=selection):
                self.expect_error('EFFORT_POLICY_STALE', lambda: self.account.create_generation_task(
                    self.user, 'image', request | selection | {'quote_id': quote['quote_id']}, 'new-task'))
        self.assertEqual(self.task_counts(), (0, 0, 0))

    def test_missing_expired_and_regular_parameter_mismatches_keep_quote_error(self):
        request = self.image_request()
        quote = self.account.quote_generation(self.user, 'image', request)
        self.expect_error('QUOTE_MISMATCH', lambda: self.account.create_generation_task(
            self.user, 'image', request | {'quote_id': quote['quote_id'], 'prompt': 'changed'}, 'new-task'))
        self.expect_error('QUOTE_MISMATCH', lambda: self.account.create_generation_task(
            self.user, 'image', request | {'quote_id': 'missing-quote'}, 'new-task'))
        self.clock[0] += 601
        self.config = replace(self.config, efforts=(effort('fixture-policy-2'),))
        self.reopen()
        self.expect_error('QUOTE_MISMATCH', lambda: self.account.create_generation_task(
            self.user, 'image', request | {'quote_id': quote['quote_id']}, 'new-task'))
        self.assertEqual(self.task_counts(), (0, 0, 0))

    def test_unknown_replay_ignores_changed_catalog_policy_and_asset_expiry(self):
        original, payload, _ = self.submit(self.image_request())
        with self.db.transaction() as connection:
            connection.execute("UPDATE generation_tasks SET status='pending_reconcile', "
                               "error_code='SUBMISSION_RESULT_UNKNOWN' WHERE id=?", (original['id'],))
        counts = self.task_counts()
        self.config = replace(self.config, image2_model='replacement-image', efforts=(), model_prices={})
        self.image2.configured = False
        self.reopen()
        self.clock[0] += 10000
        with patch.object(self.account, '_validate_generation_assets', side_effect=AssertionError('revalidated assets')), \
                patch('seecut_server.service.resolve_generation_effort', side_effect=AssertionError('rebound policy')):
            replay = self.account.create_generation_task(self.user, 'image', payload, 'generation-1')
        self.assertEqual(replay['id'], original['id'])
        self.assertEqual(replay['request'], original['request'])
        self.assertEqual(replay['status'], 'pending_reconcile')
        self.assertEqual(self.task_counts(), counts)
        self.assertEqual(self.account.claim_generation_tasks(), [])
        self.assertEqual(self.image2.calls, 0)
        self.assertEqual(self.account.wallet(self.user)['held_credits'], 7)

    def test_legacy_replay_stays_legacy_after_efforts_are_enabled(self):
        self.config = replace(self.config, efforts=())
        self.reopen()
        request = {'model': IMAGE_MODEL, 'prompt': 'old request'}
        original, payload, quote = self.submit(request)
        self.assertIsNone(quote['effort_id'])
        self.assertIsNone(quote['policy_revision'])
        self.assertIsNone(quote['policy'])
        self.assertNotIn('policy', original['request'])
        self.config = replace(self.config, efforts=(effort(),))
        self.reopen()
        replay = self.account.create_generation_task(self.user, 'image', payload, 'generation-1')
        self.assertEqual(replay['id'], original['id'])
        self.assertNotIn('effort_id', replay['request'])
        self.expect_error('EFFORT_POLICY_REQUIRED', lambda: self.account.quote_generation(self.user, 'image', request))
        self.assertEqual(self.task_counts(), (1, 1, 1))

    def test_idempotency_cannot_change_effort_model_or_parameters(self):
        original, payload, _ = self.submit(self.image_request())
        for changes in ({'effort_id': 'different'}, {'policy_revision': 'different'},
                        {'model': 'different'}, {'prompt': 'different'}, {'quantity': 2}):
            with self.subTest(changes=changes):
                self.expect_error('IDEMPOTENCY_CONFLICT', lambda: self.account.create_generation_task(
                    self.user, 'image', payload | changes, 'generation-1'))
        self.assertEqual(self.task_counts(), (1, 1, 1))
        self.assertEqual(self.account.get_generation_task(self.user, original['id'])['request'], original['request'])

    def test_batch_recovery_preserves_all_original_tasks_and_holds(self):
        original, payload, _ = self.submit(self.image_request(quantity=2))
        self.config = replace(self.config, efforts=())
        self.reopen()
        replay = self.account.create_generation_task(self.user, 'image', payload, 'generation-1')
        self.assertEqual([item['id'] for item in replay['tasks']], [item['id'] for item in original['tasks']])
        self.assertEqual(self.task_counts(), (2, 1, 1))
        self.assertEqual(self.account.wallet(self.user)['held_credits'], 14)
        self.assertEqual(self.image2.calls, 0)
