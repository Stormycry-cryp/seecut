"""Local, evidence-based UNKNOWN resolution against disposable databases only."""
from __future__ import annotations

import copy
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import threading
from contextlib import ExitStack, contextmanager
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from seecut_server.assistant_local_resolution import LocalResolution
from seecut_server.assistant_provider import AssistantProvider
from seecut_server.config import Config
from seecut_server.errors import ApiError
from test_assistant_contract import AssistantFixture, FakeAssistant, done


class AssistantLocalResolutionTest(AssistantFixture):
    def unknown_operation(self, *, pending=False):
        operation = self.create()
        with self.db.transaction(immediate=True) as connection:
            connection.execute(
                """UPDATE assistant_operations SET state=?, billing_state=?, version=version+1,
                   error_json=? WHERE id=?""",
                ('failed' if pending else 'outcome_unknown',
                 'settlement_pending' if pending else 'held',
                 '{"code":"ASSISTANT_OUTCOME_UNKNOWN","message":"fixture unresolved"}',
                 operation['operation_id']))
        return self.service.get(self.user, operation['operation_id'])

    def resolution_body(self, operation, **changes):
        body = dict(user_id=self.user, operation_id=operation['operation_id'],
                    expected_version=operation['version'], resolution_id='resolution:case-1',
                    operator='qa-operator', reason='Reviewed synthetic provider evidence',
                    outcome='confirmed_not_executed', evidence_confirmed=True,
                    evidence_reference={'id': 'ticket:case-1', 'sha256': 'a' * 64})
        body.update(changes)
        return body

    def resolve(self, body, *, apply=False):
        # Fixture acceptance is local SQL. Resolution must never prepare,
        # redispatch or query a provider, or read production configuration.
        with ExitStack() as stack:
            for provider in (AssistantProvider, FakeAssistant):
                for method in ('prepare', 'stream', 'lookup'):
                    stack.enter_context(patch.object(provider, method, side_effect=AssertionError(
                        'local resolution must not invoke upstream ' + method)))
            stack.enter_context(patch.object(Config, 'from_env', side_effect=AssertionError(
                'local resolution must not load environment credentials')))
            stack.enter_context(patch('urllib.request.urlopen', side_effect=AssertionError(
                'local resolution must not make network requests')))
            return LocalResolution(self.db, clock=lambda: self.clock[0]).resolve(body, apply=apply)

    def database_rows(self):
        with self.db.connect() as connection:
            names = [row[0] for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
            return {name: [dict(row) for row in connection.execute(
                'SELECT * FROM "' + name.replace('"', '""') + '" ORDER BY rowid')]
                    for name in names}

    def files(self):
        root = Path(self.temp.name)
        return {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in root.rglob('*') if path.is_file()}

    def audit(self):
        with self.db.connect() as connection:
            return [dict(row) for row in connection.execute(
                'SELECT * FROM assistant_resolutions ORDER BY resolution_id')]

    def test_default_dry_run_unknown_has_no_database_or_file_changes(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation, outcome='unknown')
        body.pop('evidence_reference')
        body.pop('evidence_confirmed')
        before, files = self.database_rows(), self.files()
        result = self.resolve(body)
        self.assertEqual((result['status'], result['applied']), ('unresolved', False))
        self.assertEqual(self.database_rows(), before)
        self.assertEqual(self.files(), files)
        self.wallet(97, 3)

    def test_dry_run_with_sufficient_evidence_is_ready_and_does_not_write(self):
        operation = self.unknown_operation()
        before, files = self.database_rows(), self.files()
        result = self.resolve(self.resolution_body(operation))
        self.assertEqual((result['status'], result['applied']), ('ready', False))
        self.assertEqual(self.database_rows(), before)
        self.assertEqual(self.files(), files)
        self.assertEqual(self.audit(), [])

    def test_apply_unknown_or_missing_human_evidence_is_rejected_without_writes(self):
        operation = self.unknown_operation()
        valid = self.resolution_body(operation)
        variants = [valid | {'outcome': 'unknown'},
                    {key: value for key, value in valid.items() if key != 'evidence_reference'},
                    {key: value for key, value in valid.items() if key != 'evidence_confirmed'},
                    valid | {'evidence_confirmed': False}]
        before = self.database_rows()
        for body in variants:
            with self.subTest(body=body):
                self.expect_error('ASSISTANT_RESOLUTION_EVIDENCE_REQUIRED',
                                  lambda: self.resolve(body, apply=True))
                self.assertEqual(self.database_rows(), before)
        self.wallet(97, 3)

    def test_version_conflict_preserves_operation_hold_ledger_and_audit(self):
        operation = self.unknown_operation()
        before = self.database_rows()
        body = self.resolution_body(operation, expected_version=operation['version'] - 1)
        self.expect_error('ASSISTANT_RESOLUTION_VERSION_CONFLICT',
                          lambda: self.resolve(body, apply=True))
        self.assertEqual(self.database_rows(), before)
        self.wallet(97, 3)

    def test_wrong_user_and_missing_operation_are_not_found(self):
        operation = self.unknown_operation()
        before = self.database_rows()
        for change in ({'user_id': self.other}, {'operation_id': 'aop_missing'}):
            with self.subTest(change=change):
                error = self.expect_error('ASSISTANT_OPERATION_NOT_FOUND',
                    lambda: self.resolve(self.resolution_body(operation, **change), apply=True))
                self.assertEqual(error.status, 404)
                self.assertEqual(self.database_rows(), before)

    def test_confirmed_not_executed_releases_once_and_records_local_declaration(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation)
        result = self.resolve(body, apply=True)
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((result['status'], result['applied']), ('resolved', True))
        self.assertEqual((final['state'], final['billing_state']), ('failed', 'released'))
        self.assertEqual((final['captured_credits'], final['released_credits']), (0, 3))
        self.assertEqual(final['usage'], {'input': 0, 'output': 0})
        self.assertGreater(final['version'], operation['version'])
        rows = self.wallet(100, 0)
        self.assertEqual([row['kind'] for row in rows], ['hold', 'release'])
        records = self.audit()
        self.assertEqual(len(records), 1)
        record = records[0]
        self.assertEqual(record['resolution_id'], body['resolution_id'])
        self.assertEqual((record['user_id'], record['operation_id']),
                         (self.user, operation['operation_id']))
        self.assertEqual((record['operator'], record['reason']), (body['operator'], body['reason']))
        self.assertEqual(record['operator_authentication'], 'local_declaration')
        self.assertEqual(json.loads(record['evidence_reference_json']), body['evidence_reference'])
        self.assertEqual(json.loads(record['before_json']), result['before'])
        self.assertEqual(json.loads(record['after_json']), result['after'])
        self.assertEqual(json.loads(record['result_json']), result)
        self.assertEqual(record['created_at'], self.clock[0])
        self.assertRegex(record['input_digest'], r'^[0-9a-f]{64}$')
        summary = json.loads(record['input_summary_json'])
        self.assertNotIn('access_token', summary)
        self.assertNotIn('api_key', summary)
        self.assertNotIn(self.token, json.dumps(record))

    def test_resolution_id_exact_replay_returns_saved_result_and_never_recharges(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation)
        result = self.resolve(body, apply=True)
        before = self.database_rows()
        self.clock[0] += 100
        self.assertEqual(self.resolve(copy.deepcopy(body), apply=True), result)
        self.assertEqual(self.database_rows(), before)
        self.assertEqual(len(self.wallet(100, 0)), 2)
        self.assertEqual(len(self.audit()), 1)

    def test_resolution_id_changed_input_is_a_conflict_even_after_settlement(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation)
        self.resolve(body, apply=True)
        before = self.database_rows()
        changes = ({'reason': 'different reason'}, {'operator': 'other-operator'},
                   {'expected_version': body['expected_version'] + 1},
                   {'evidence_reference': {'id': 'ticket:case-2', 'sha256': 'b' * 64}})
        for change in changes:
            with self.subTest(change=change):
                self.expect_error('ASSISTANT_RESOLUTION_ID_CONFLICT',
                    lambda: self.resolve(body | change, apply=True))
                self.assertEqual(self.database_rows(), before)

    def test_explicit_default_fields_replay_the_same_normalized_resolution(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation)
        result = self.resolve(body, apply=True)
        before = self.database_rows()
        explicit = body | {'terminal_state': 'failed', 'usage': {'input': 0, 'output': 0}}
        self.assertEqual(self.resolve(explicit, apply=True), result)
        self.assertEqual(self.database_rows(), before)

    def test_resolution_id_is_global_and_cannot_be_retargeted_to_another_user(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation)
        self.resolve(body, apply=True)
        before = self.database_rows()
        self.expect_error('ASSISTANT_RESOLUTION_ID_CONFLICT',
                          lambda: self.resolve(body | {'user_id': self.other}, apply=True))
        self.assertEqual(self.database_rows(), before)

    def test_already_settled_operation_cannot_receive_a_second_resolution_charge(self):
        operation = self.unknown_operation()
        self.resolve(self.resolution_body(operation), apply=True)
        final = self.service.get(self.user, operation['operation_id'])
        before = self.database_rows()
        with self.assertRaises(ApiError):
            self.resolve(self.resolution_body(final, resolution_id='resolution:case-2'), apply=True)
        self.assertEqual(self.database_rows(), before)
        self.wallet(100, 0)

    def test_audit_insert_failure_rolls_back_state_events_hold_wallet_and_ledger(self):
        operation = self.unknown_operation()
        with self.db.transaction(immediate=True) as connection:
            connection.execute("""CREATE TRIGGER fail_resolution_audit
                BEFORE INSERT ON assistant_resolutions BEGIN
                SELECT RAISE(ABORT, 'synthetic audit failure'); END""")
        before = self.database_rows()
        with self.assertRaises(sqlite3.DatabaseError):
            self.resolve(self.resolution_body(operation), apply=True)
        self.assertEqual(self.database_rows(), before)
        self.assertEqual(self.audit(), [])
        self.wallet(97, 3)

    def test_confirmed_usage_uses_original_frozen_price_and_model_ownership(self):
        operation = self.unknown_operation()
        with self.db.transaction(immediate=True) as connection:
            connection.execute('UPDATE assistant_turn_budgets SET model_id=? WHERE id=?',
                               ('retired-synthetic-model', operation['budget_id']))
        # A subsequent configuration change cannot reprice the accepted hold.
        self.service.config = replace(self.config, assistant_input_rate=9000,
                                      assistant_output_rate=9000, assistant_model='different-model')
        body = self.resolution_body(operation, outcome='confirmed_usage', terminal_state='cancelled',
                                    usage={'input': 100, 'output': 50})
        result = self.resolve(body, apply=True)
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual(result['status'], 'resolved')
        self.assertEqual((final['state'], final['billing_state']), ('cancelled', 'captured'))
        self.assertEqual((final['captured_credits'], final['released_credits']), (1, 2))
        self.assertEqual(final['usage'], body['usage'])
        self.assertEqual([row['kind'] for row in self.wallet(99, 0)], ['hold', 'capture', 'release'])
        before = self.database_rows()
        self.assertEqual(self.resolve(body, apply=True), result)
        self.assertEqual(self.database_rows(), before)

    def test_existing_settlement_pending_can_be_resolved_without_provider_calls(self):
        operation = self.unknown_operation(pending=True)
        result = self.resolve(self.resolution_body(operation, outcome='confirmed_usage',
            terminal_state='failed', usage={'input': 100, 'output': 0}), apply=True)
        self.assertEqual(result['status'], 'resolved')
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((final['state'], final['billing_state']), ('failed', 'captured'))
        self.wallet(99, 0)
        self.assertEqual(self.provider.calls, [])
        self.assertEqual(self.provider.lookups, [])

    def test_prior_nonzero_pending_usage_cannot_be_zero_released_as_not_executed(self):
        operation = self.unknown_operation(pending=True)
        with self.db.transaction(immediate=True) as connection:
            connection.execute('UPDATE assistant_operations SET usage_json=?,version=version+1 WHERE id=?',
                               (json.dumps({'input': 100, 'output': 0}), operation['operation_id']))
        operation = self.service.get(self.user, operation['operation_id'])
        before = self.database_rows()
        self.expect_error('ASSISTANT_RESOLUTION_EVIDENCE_CONFLICT',
            lambda: self.resolve(self.resolution_body(operation), apply=True))
        self.assertEqual(self.database_rows(), before)
        self.assertEqual([row['kind'] for row in self.wallet(97, 3)], ['hold'])
        self.assertEqual(self.audit(), [])

    def test_prior_done_conflicts_and_invalid_prior_evidence_preserve_all_records(self):
        operation = self.unknown_operation(pending=True)
        prior_usage, prior_result = {'input': 100, 'output': 50}, done()
        with self.db.transaction(immediate=True) as connection:
            connection.execute("""UPDATE assistant_operations SET state='succeeded', usage_json=?,
                result_json=?,version=version+1 WHERE id=?""",
                (json.dumps(prior_usage), json.dumps(prior_result), operation['operation_id']))
        operation = self.service.get(self.user, operation['operation_id'])
        different_result = copy.deepcopy(prior_result)
        different_result['message']['content'][0]['text'] = 'different synthetic done'
        contradictory = [self.resolution_body(operation),
            self.resolution_body(operation, outcome='confirmed_usage', terminal_state='failed',
                                 usage=prior_usage),
            self.resolution_body(operation, outcome='confirmed_usage', terminal_state='cancelled',
                                 usage=prior_usage),
            self.resolution_body(operation, outcome='confirmed_usage', terminal_state='succeeded',
                                 usage=prior_usage, result=different_result)]
        before = self.database_rows()
        for index, body in enumerate(contradictory):
            with self.subTest(contradiction=index):
                self.expect_error('ASSISTANT_RESOLUTION_EVIDENCE_CONFLICT',
                                  lambda: self.resolve(body, apply=True))
                self.assertEqual(self.database_rows(), before)

        invalid_prior = [
            ('{invalid-json', None),
            (json.dumps({'input': 'invalid', 'output': 0}), None),
            (json.dumps(prior_usage), '{invalid-json'),
            (json.dumps(prior_usage), json.dumps({'type': 'done', 'stopReason': 'stop'}))]
        for usage_json, result_json in invalid_prior:
            with self.subTest(usage_json=usage_json, result_json=result_json):
                # Keep raw invalid historical evidence intact; do not decode it
                # through AssistantService.get or reinterpret it as zero usage.
                with self.db.transaction(immediate=True) as connection:
                    connection.execute("""UPDATE assistant_operations SET state='failed', usage_json=?,
                        result_json=?,version=version+1 WHERE id=?""",
                        (usage_json, result_json, operation['operation_id']))
                    version = connection.execute('SELECT version FROM assistant_operations WHERE id=?',
                                                 (operation['operation_id'],)).fetchone()[0]
                operation = operation | {'version': version}
                body = self.resolution_body(operation)
                before, files = self.database_rows(), self.files()
                preview = self.resolve(body)
                self.assertEqual((preview['status'], preview['applied'], preview['reason']),
                                 ('unresolved', False, 'prior_evidence_invalid'))
                self.assertEqual(self.database_rows(), before)
                self.assertEqual(self.files(), files)
                self.expect_error('ASSISTANT_RESOLUTION_PRIOR_EVIDENCE_INVALID',
                                  lambda: self.resolve(body, apply=True))
                self.assertEqual(self.database_rows(), before)
        self.assertEqual([row['kind'] for row in self.wallet(97, 3)], ['hold'])
        self.assertEqual(self.audit(), [])

    def test_matching_persisted_usage_and_done_settle_at_frozen_price_and_replay_once(self):
        operation = self.unknown_operation(pending=True)
        prior_usage, prior_result = {'input': 100, 'output': 50}, done(tool=True)
        with self.db.transaction(immediate=True) as connection:
            connection.execute("""UPDATE assistant_operations SET state='succeeded', usage_json=?,
                result_json=?,version=version+1 WHERE id=?""",
                (json.dumps(prior_usage), json.dumps(prior_result), operation['operation_id']))
        operation = self.service.get(self.user, operation['operation_id'])
        self.service.config = replace(self.config, assistant_input_rate=9000, assistant_output_rate=9000)
        body = self.resolution_body(operation, outcome='confirmed_usage', terminal_state='succeeded',
                                    usage=prior_usage, result=prior_result)
        result = self.resolve(body, apply=True)
        self.assertEqual((result['status'], result['applied']), ('resolved', True))
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((final['state'], final['billing_state']), ('succeeded', 'captured'))
        self.assertEqual((final['captured_credits'], final['released_credits']), (1, 2))
        self.assertEqual((final['usage'], final['result']), (prior_usage, prior_result))
        self.assertEqual([row['kind'] for row in self.wallet(99, 0)], ['hold', 'capture', 'release'])
        self.assertEqual(len(self.audit()), 1)
        before = self.database_rows()
        self.clock[0] += 100
        self.assertEqual(self.resolve(copy.deepcopy(body), apply=True), result)
        self.assertEqual(self.database_rows(), before)
        events = self.service.events(self.user, operation['operation_id'], 0)['events']
        self.assertEqual(sum(event['type'] == 'model' and event['payload']['type'] == 'done'
                             for event in events), 1)

    def test_over_limit_usage_remains_pending_and_preserves_entire_hold(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation, outcome='confirmed_usage', terminal_state='failed',
                                    usage={'input': 100, 'output': 201})
        result = self.resolve(body, apply=True)
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((result['status'], result['applied']), ('unresolved', True))
        self.assertEqual(final['billing_state'], 'settlement_pending')
        self.assertEqual(final['error']['code'], 'ASSISTANT_USAGE_UNCONFIRMED')
        self.assertEqual((final['captured_credits'], final['released_credits']), (0, 0))
        self.assertEqual([row['kind'] for row in self.wallet(97, 3)], ['hold'])
        self.assertEqual(len(self.audit()), 1)
        before = self.database_rows()
        self.assertEqual(self.resolve(body, apply=True), result)
        self.assertEqual(self.database_rows(), before)

    def test_succeeded_requires_complete_done_and_matching_usage(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation, outcome='confirmed_usage', terminal_state='succeeded',
                                    usage={'input': 100, 'output': 50})
        invalid_results = [None, {'type': 'text_delta', 'text': 'not a complete result'},
                           done() | {'usage': {'input': 99, 'output': 50}},
                           {'type': 'done', 'message': {'id': 'reply', 'role': 'assistant'},
                            'stopReason': 'stop'}]
        before = self.database_rows()
        for result in invalid_results:
            with self.subTest(result=result):
                request = body if result is None else body | {'result': result}
                self.expect_error('ASSISTANT_RESOLUTION_RESULT_INVALID',
                                  lambda: self.resolve(request, apply=True))
                self.assertEqual(self.database_rows(), before)
        self.wallet(97, 3)

    def test_done_cannot_introduce_a_tool_absent_from_the_original_request(self):
        operation = self.unknown_operation()
        result = done(tool=True)
        # "project" is allowed globally, but this operation requested context only.
        result['message']['toolCalls'][0]['name'] = 'project'
        body = self.resolution_body(operation, outcome='confirmed_usage', terminal_state='succeeded',
                                    usage={'input': 100, 'output': 50}, result=result)
        before = self.database_rows()
        self.expect_error('ASSISTANT_RESOLUTION_RESULT_INVALID',
                          lambda: self.resolve(body, apply=True))
        self.assertEqual(self.database_rows(), before)

    def test_evidence_reference_accepts_identifiers_and_digests_not_logs_or_secrets(self):
        operation = self.unknown_operation()
        invalid = [
            {'id': 'https://fixture.invalid/log?access_token=synthetic-secret', 'sha256': 'a' * 64},
            {'id': 'Bearer synthetic-secret from a full log', 'sha256': 'a' * 64},
            {'id': 'ticket:case-1', 'sha256': 'not-a-sha256'},
            {'id': 'ticket:case-1', 'sha256': 'a' * 64, 'api_key': 'synthetic-secret'}]
        before = self.database_rows()
        for reference in invalid:
            with self.subTest(reference=reference):
                with self.assertRaises(ApiError) as caught:
                    self.resolve(self.resolution_body(operation, evidence_reference=reference), apply=True)
                self.assertNotIn('synthetic-secret', caught.exception.message)
                self.assertEqual(self.database_rows(), before)

    def test_valid_complete_done_succeeds_and_is_published_once(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation, outcome='confirmed_usage', terminal_state='succeeded',
                                    usage={'input': 100, 'output': 50}, result=done(tool=True))
        result = self.resolve(body, apply=True)
        self.assertEqual(result['status'], 'resolved')
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((final['state'], final['result']), ('succeeded', body['result']))
        self.wallet(99, 0)
        self.assertEqual(self.resolve(body, apply=True), result)
        events = self.service.events(self.user, operation['operation_id'], 0)['events']
        self.assertEqual(sum(event['type'] == 'model' and event['payload']['type'] == 'done'
                             for event in events), 1)

    def test_complete_done_without_usage_can_settle_explicitly_confirmed_usage(self):
        operation = self.unknown_operation()
        body = self.resolution_body(operation, outcome='confirmed_usage', terminal_state='succeeded',
                                    usage={'input': 100, 'output': 50}, result=done(usage=False))
        result = self.resolve(body, apply=True)
        self.assertEqual((result['status'], result['applied']), ('resolved', True))
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((final['state'], final['result']), ('succeeded', body['result']))
        self.assertEqual(final['usage'], body['usage'])
        self.wallet(99, 0)
        before = self.database_rows()
        self.assertEqual(self.resolve(body, apply=True), result)
        self.assertEqual(self.database_rows(), before)

    def test_queued_or_running_operation_is_not_manually_resolved(self):
        operation = self.create()
        before = self.database_rows()
        with self.assertRaises(ApiError):
            self.resolve(self.resolution_body(operation), apply=True)
        self.assertEqual(self.database_rows(), before)
        with self.db.transaction(immediate=True) as connection:
            connection.execute("UPDATE assistant_operations SET state='running' WHERE id=?",
                               (operation['operation_id'],))
        before = self.database_rows()
        with self.assertRaises(ApiError):
            self.resolve(self.resolution_body(operation), apply=True)
        self.assertEqual(self.database_rows(), before)
        self.wallet(97, 3)

    def test_active_execution_lock_refuses_apply_without_any_accounting_change(self):
        from seecut_server.assistant_execution_lock import execution_lock
        operation = self.unknown_operation()
        before = self.database_rows()
        with execution_lock(self.db.path, required=True) as acquired:
            self.assertTrue(acquired)
            self.expect_error('ASSISTANT_EXECUTOR_ACTIVE',
                lambda: self.resolve(self.resolution_body(operation), apply=True))
        self.assertEqual(self.database_rows(), before)
        self.wallet(97, 3)

    def test_real_worker_holds_execution_lock_during_provider_dispatch(self):
        operation = self.unknown_operation()
        budget = self.service.turn_budget(self.other, {
            'client_request_id': 'other-budget', 'local_session_id': 'other-session',
            'turn_id': 'other-turn'}, 'other-budget')
        request = self.body('other-step', budget=budget, session='other-session', turn='other-turn')
        quote = self.service.quote(self.other, request, 'other-quote')
        self.service.create(self.other, request | {'quote_id': quote['quote_id']}, 'other-create')
        entered, release = threading.Event(), threading.Event()
        errors = []

        def executing(operation_id, cancelled):
            entered.set()
            if not release.wait(5):
                raise AssertionError('fixture worker was not released')

        def process():
            try:
                self.worker.process_once()
            except Exception as error:
                errors.append(error)

        self.provider.on_stream = executing
        thread = threading.Thread(target=process)
        thread.start()
        try:
            self.assertTrue(entered.wait(3), 'fixture worker did not enter stream')
            before = self.database_rows()
            self.expect_error('ASSISTANT_EXECUTOR_ACTIVE',
                lambda: self.resolve(self.resolution_body(operation), apply=True))
            self.assertEqual(self.database_rows(), before)
        finally:
            release.set()
            thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.wallet(97, 3)

    def test_startup_recovery_holds_execution_lock_before_worker_start(self):
        operation = self.unknown_operation()
        budget = self.service.turn_budget(self.other, {
            'client_request_id': 'other-budget', 'local_session_id': 'other-session',
            'turn_id': 'other-turn'}, 'other-budget')
        request = self.body('other-step', budget=budget, session='other-session', turn='other-turn')
        quote = self.service.quote(self.other, request, 'other-quote')
        queued = self.service.create(self.other, request | {'quote_id': quote['quote_id']}, 'other-create')[0]
        entered, release, dispatched = threading.Event(), threading.Event(), threading.Event()
        recover = self.service.recover_interrupted

        def recovering():
            entered.set()
            if not release.wait(5):
                raise AssertionError('fixture startup was not released')
            return recover()

        self.provider.on_stream = lambda operation_id, cancelled: dispatched.set()
        with patch.object(self.service, 'recover_interrupted', side_effect=recovering) as recovering_call:
            self.worker.start()
            try:
                self.assertTrue(entered.wait(3), 'fixture startup did not enter recovery')
                self.assertTrue(self.worker._thread.is_alive())
                self.assertEqual(self.provider.calls, [])
                self.assertFalse(dispatched.is_set())
                self.assertEqual(self.service.get(self.other, queued['operation_id'])['state'], 'queued')
                before = self.database_rows()
                self.expect_error('ASSISTANT_EXECUTOR_ACTIVE',
                    lambda: self.resolve(self.resolution_body(operation), apply=True))
                self.assertEqual(self.database_rows(), before)
                release.set()
                self.assertTrue(dispatched.wait(3), 'fixture worker did not dispatch after recovery')
            finally:
                release.set()
                self.worker.stop()
            self.assertEqual(recovering_call.call_count, 1)
        self.assertFalse(self.worker._thread.is_alive())
        self.assertEqual(self.provider.calls, [queued['operation_id']])
        self.assertEqual(self.service.get(self.other, queued['operation_id'])['state'], 'succeeded')
        self.wallet(97, 3)

    def test_worker_retries_temporary_startup_lock_contention_then_recovers_and_dispatches_once(self):
        from seecut_server.assistant_execution_lock import execution_lock
        operation = self.create()
        busy, dispatched = threading.Event(), threading.Event()

        @contextmanager
        def observed_lock(*args, **kwargs):
            with execution_lock(*args, **kwargs) as acquired:
                if acquired is False:
                    busy.set()
                yield acquired

        self.provider.on_stream = lambda operation_id, cancelled: dispatched.set()
        with patch('seecut_server.assistant_worker.execution_lock', new=observed_lock), \
                patch.object(self.service, 'recover_interrupted', wraps=self.service.recover_interrupted) as recover:
            try:
                with execution_lock(self.db.path, required=True) as acquired:
                    self.assertTrue(acquired)
                    self.worker.start()
                    self.assertTrue(busy.wait(3), 'fixture worker did not encounter the held lock')
                    self.assertTrue(self.worker._thread.is_alive())
                    self.assertEqual(recover.call_count, 0)
                    self.assertEqual(self.provider.calls, [])
                    self.assertEqual(self.service.get(self.user, operation['operation_id'])['state'], 'queued')
                self.assertTrue(dispatched.wait(3), 'fixture worker did not retry after the lock was released')
            finally:
                self.worker.stop()
            self.assertEqual(recover.call_count, 1)
        self.assertFalse(self.worker._thread.is_alive())
        self.assertEqual(self.provider.calls, [operation['operation_id']])
        self.assertEqual(self.service.get(self.user, operation['operation_id'])['state'], 'succeeded')
        self.wallet(99, 0)

    def test_apply_fails_closed_when_os_execution_lock_is_unavailable(self):
        operation = self.unknown_operation()
        before, files = self.database_rows(), self.files()
        with patch('seecut_server.assistant_execution_lock.fcntl', None):
            self.expect_error('ASSISTANT_EXECUTOR_LOCK_UNAVAILABLE',
                lambda: self.resolve(self.resolution_body(operation), apply=True))
        self.assertEqual(self.database_rows(), before)
        self.assertEqual(self.files(), files)

    def cli(self, operation=None, *extra, database=None):
        command = [sys.executable, '-B', '-m', 'seecut_server.assistant_local_resolution',
                   '--database', str(database or self.db.path), '--user-id', self.user,
                   '--operation-id', operation['operation_id'] if operation else 'aop_missing',
                   '--expected-version', str(operation['version'] if operation else 1),
                   '--resolution-id', 'resolution:cli-1', '--operator', 'qa-operator',
                   '--reason', 'Reviewed synthetic provider evidence']
        command.extend(extra)
        source = Path(__file__).resolve().parents[1] / 'src'
        return subprocess.run(command, capture_output=True, text=True, timeout=15,
                              env={'PATH': os.defpath, 'PYTHONPATH': str(source),
                                   'PYTHONDONTWRITEBYTECODE': '1'})

    def test_cli_defaults_to_dry_run_and_does_not_load_environment_credentials(self):
        operation = self.unknown_operation()
        before, files = self.database_rows(), self.files()
        completed = self.cli(operation)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual(result['mode'], 'dry_run')
        self.assertEqual((result['result']['status'], result['result']['applied']),
                         ('unresolved', False))
        self.assertEqual(self.database_rows(), before)
        self.assertEqual(self.files(), files)

    def test_cli_explicit_apply_resolves_only_the_specified_disposable_operation(self):
        operation = self.unknown_operation()
        completed = self.cli(operation, '--outcome', 'confirmed_not_executed',
            '--evidence-reference', 'ticket:cli-1', '--evidence-sha256', 'a' * 64,
            '--confirm-evidence', '--apply')
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual(result['mode'], 'apply')
        self.assertEqual((result['result']['status'], result['result']['applied']), ('resolved', True))
        self.wallet(100, 0)
        self.assertEqual(len(self.audit()), 1)
        with self.db.connect() as connection:
            other = connection.execute('SELECT * FROM wallets WHERE user_id=?', (self.other,)).fetchone()
        self.assertEqual((other['available_credits'], other['held_credits']), (100, 0))

    def test_cli_missing_database_does_not_create_file_or_parent_directory(self):
        absent = Path(self.temp.name) / 'absent-parent' / 'missing.db'
        before = self.files()
        for extra in ((), ('--apply',)):
            with self.subTest(extra=extra):
                completed = self.cli(None, *extra, database=absent)
                self.assertEqual(completed.returncode, 2)
                result = json.loads(completed.stdout)
                self.assertEqual(result['status'], 'unresolved')
                self.assertIn('code', result)
                self.assertFalse(absent.parent.exists())
                self.assertEqual(self.files(), before)

    def test_cli_succeeded_accepts_only_a_complete_done_from_result_file(self):
        operation = self.unknown_operation()
        result_path = Path(self.temp.name) / 'synthetic-done.json'
        result_path.write_text(json.dumps(done()), encoding='utf-8')
        completed = self.cli(operation, '--outcome', 'confirmed_usage',
            '--terminal-state', 'succeeded', '--input-tokens', '100', '--output-tokens', '50',
            '--result-file', str(result_path), '--evidence-reference', 'ticket:cli-1',
            '--evidence-sha256', 'a' * 64, '--confirm-evidence', '--apply')
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual(result['result']['status'], 'resolved')
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual((final['state'], final['result']), ('succeeded', done()))
        self.wallet(99, 0)

    def test_cli_over_limit_usage_exits_unresolved_with_audit_and_hold_preserved(self):
        operation = self.unknown_operation()
        completed = self.cli(operation, '--outcome', 'confirmed_usage',
            '--terminal-state', 'failed', '--input-tokens', '100', '--output-tokens', '201',
            '--evidence-reference', 'ticket:cli-1', '--evidence-sha256', 'a' * 64,
            '--confirm-evidence', '--apply')
        self.assertEqual(completed.returncode, 3, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual(result['mode'], 'apply')
        self.assertEqual((result['result']['status'], result['result']['applied']), ('unresolved', True))
        final = self.service.get(self.user, operation['operation_id'])
        self.assertEqual(final['billing_state'], 'settlement_pending')
        self.assertEqual([row['kind'] for row in self.wallet(97, 3)], ['hold'])
        self.assertEqual(len(self.audit()), 1)

    def test_cli_refuses_apply_without_evidence_and_reports_structured_error(self):
        operation = self.unknown_operation()
        before = self.database_rows()
        completed = self.cli(operation, '--outcome', 'confirmed_not_executed', '--apply')
        self.assertEqual(completed.returncode, 2, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual((result['status'], result['code']),
                         ('unresolved', 'ASSISTANT_RESOLUTION_EVIDENCE_REQUIRED'))
        self.assertEqual(self.database_rows(), before)
        self.wallet(97, 3)
