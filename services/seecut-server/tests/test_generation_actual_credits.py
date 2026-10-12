"""Actual image charges come from committed capture entries, never quotes or holds."""
from test_generation_request_recovery import GenerationRequestRecoveryTest
from test_generation_robustness import PNG


class GenerationActualCreditsTest(GenerationRequestRecoveryTest):
    # Run this class's tests explicitly to avoid repeating inherited recovery tests.
    def test_success_reports_actual_capture_and_repeated_reads_are_read_only(self):
        original, _ = self.submit()
        self.assertEqual(original['captured_credits'], 0)
        self.account._store_binary_outputs(original['id'], [PNG], ['image/png'])
        before = self.business_snapshot()
        for _ in range(3):
            current = self.account.get_generation_task_by_request(self.user, 'flow-original-key')
            self.assertEqual(current['captured_credits'], 7)
            self.assertEqual(current['status'], 'succeeded')
        self.assertEqual(self.business_snapshot(), before)
        self.assertEqual(self.account.wallet(self.user)['held_credits'], 0)

    def test_failed_release_and_unknown_reservation_are_not_actual_charges(self):
        failed, _ = self.submit(key='failed')
        self.account._fail_and_release(failed['id'], 'FAKE_FAILURE', 'synthetic failure')
        unknown, _ = self.submit(key='unknown')
        with self.db.transaction() as connection:
            connection.execute("UPDATE generation_tasks SET status='pending_reconcile' WHERE id=?",
                               (unknown['id'],))
        before = self.business_snapshot()
        for key in ('failed', 'unknown'):
            current = self.account.get_generation_task_by_request(self.user, key)
            self.assertEqual(current['captured_credits'], 0)
            self.assertEqual(current['quoted_credits'], 7)
        self.assertEqual(self.account.wallet(self.user)['held_credits'], 7)
        self.assertEqual(self.business_snapshot(), before)

    def test_batch_task_views_distinguish_captured_and_still_held_amounts(self):
        original, _ = self.submit(quantity=2)
        self.account._store_binary_outputs(original['tasks'][0]['id'], [PNG], ['image/png'])
        before = self.business_snapshot()
        for _ in range(3):
            tasks = self.account.get_generation_task_by_request(self.user, 'flow-original-key')['tasks']
            self.assertEqual([task['captured_credits'] for task in tasks], [7, 0])
        self.assertEqual(self.business_snapshot(), before)
        self.assertEqual(self.account.wallet(self.user)['held_credits'], 7)
