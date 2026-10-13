"""Stable keyset history: real spend/topups/refunds, without reservation noise."""
from test_assistant_contract import AssistantFixture


class AccountHistoryPagesTest(AssistantFixture):
    def setUp(self):
        super().setUp()
        with self.db.transaction() as c:
            for index in range(207):
                kind = ('purchase', 'capture', 'release', 'hold')[index % 4]
                c.execute('''INSERT INTO ledger_entries
                    (id,user_id,kind,delta_available,delta_held,reference_type,reference_id,idempotency_key,created_at)
                    VALUES(?,?,?,?,?,?,?,?,?)''', (f'entry-{index:04}', self.user, kind, 0, 0, 'fixture', f'ref-{index}', f'key-{index}', 1000))

    def test_same_timestamp_pages_have_no_duplicates_or_omissions(self):
        cursor, found = '', []
        while True:
            page = self.account.wallet_ledger(self.user, cursor=cursor, limit=19)
            found.extend(row['id'] for row in page['items'])
            if page['next_cursor'] is None: break
            cursor = page['next_cursor']
        expected = [f'entry-{index:04}' for index in reversed(range(207)) if index % 4 != 3]
        self.assertEqual(found, expected)
        self.assertEqual(len(found), len(set(found)))

    def test_categories_and_balance_refresh_do_not_change_history(self):
        for category, kind in [('spend', 'capture'), ('topup', 'purchase'), ('refund', 'release')]:
            first = self.account.wallet_ledger(self.user, category=category, limit=20)
            second = self.account.wallet_ledger(self.user, category=category, limit=20, cursor=first['next_cursor'])
            self.assertTrue(all(row['kind'] == kind for row in first['items'] + second['items']))
            self.assertFalse({row['id'] for row in first['items']} & {row['id'] for row in second['items']})
            balance = self.account.wallet(self.user, include_history=False)
            self.assertEqual(balance['ledger'], [])
            self.assertEqual(self.account.wallet_ledger(self.user, category=category, limit=20, cursor=first['next_cursor']), second)
        self.expect_error('INVALID_HISTORY_CURSOR', lambda: self.account.wallet_ledger(self.user, cursor='not-a-page'))
        self.expect_error('INVALID_HISTORY_CATEGORY', lambda: self.account.wallet_ledger(self.user, category='hold'))

    def test_more_than_one_hundred_orders_are_read_without_purchase(self):
        with self.db.transaction() as c:
            plan = 'history-plan'
            c.execute("INSERT INTO plans VALUES(?, '历史充值', 100, 10, 1, 1000)",(plan,))
            for index in range(123):
                c.execute("INSERT INTO orders(id,user_id,plan_id,provider,amount_fen,credits,status,paid_at,created_at,updated_at) VALUES(?,?,?,'alipay',100,10,'paid',1000,1000,1000)",
                    (f'order-{index:04}',self.user,plan))
        before = self.account.wallet(self.user)
        cursor, found = '', []
        while True:
            page = self.account.orders(self.user,cursor=cursor,limit=20)
            found.extend(row['id'] for row in page['items'])
            self.assertTrue(all(row['credits']==10 and row['paid_at']==1000 for row in page['items']))
            if not page['next_cursor']: break
            cursor=page['next_cursor']
        self.assertEqual(found,[f'order-{index:04}' for index in reversed(range(123))])
        self.assertEqual(self.account.wallet(self.user),before)
