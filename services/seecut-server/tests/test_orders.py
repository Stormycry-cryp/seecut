from __future__ import annotations

import base64
import http.client
import json
import subprocess
import threading
import unittest
import urllib.parse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch

from seecut_server.db import Database
from seecut_server.http import SeeCutHandler
from seecut_server.service import ApiError, SeeCutService
import test_service as service_fixture
from test_security_boundaries import FakeResponse


TEST_KEYS = Path(__file__).parent / "fixtures" / "orders-rsa2-test-only"


class OrderTest(unittest.TestCase):
    def setUp(self):
        service_fixture.ServiceTest.setUp(self)
        self.addCleanup(self.temp.cleanup)
        self.clock = [1_700_000_000]
        self.config = replace(
            self.service.config,
            alipay_app_id="test-app",
            alipay_seller_id="test-seller",
            alipay_notify_url="https://merchant.example.test/api/payments/alipay/notify",
            alipay_gateway="https://alipay.example.test/gateway.do",
            alipay_public_key_path=str(TEST_KEYS / "public.pem"),
            alipay_merchant_private_key_path=str(TEST_KEYS / "private.pem"),
        )
        self.service = self.reopen()
        self.user, self.token = self.create_user("payer@example.test")
        with self.service.db.transaction() as connection:
            for plan_id, price, credits in (("plan-1", 100, 100), ("plan-2", 200, 250)):
                connection.execute(
                    "INSERT INTO plans(id,name,price_fen,credits,active,created_at) VALUES(?,?,?,?,1,?)",
                    (plan_id, plan_id, price, credits, self.clock[0]),
                )

    def reopen(self):
        database = getattr(self, "service", None).db
        database.initialize()
        return SeeCutService(self.config, database, emailer=self.emailer, clock=lambda: self.clock[0])

    def create_user(self, email):
        return service_fixture.ServiceTest.create_user(self, email)

    def create(self, key="intent-1", plan="plan-1", user=None):
        return self.service.create_order(user or self.user, plan, key)

    def sign(self, content):
        # This is the committed synthetic test key, never merchant material.
        signature = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", str(TEST_KEYS / "private.pem")],
            input=content.encode(), capture_output=True, timeout=5, check=True,
        ).stdout
        return base64.b64encode(signature).decode()

    def notification(self, order, status="TRADE_SUCCESS", **changes):
        fields = {
            "app_id": "test-app", "seller_id": "test-seller", "out_trade_no": order["id"],
            "trade_no": "trade-" + order["id"], "trade_status": status,
            "total_amount": "1.00", "sign_type": "RSA2",
        }
        fields.update(changes)
        canonical = "&".join(f"{key}={fields[key]}" for key in sorted(fields) if key != "sign_type" and fields[key])
        fields["sign"] = self.sign(canonical)
        return fields

    def query_response(self, order, status="TRADE_SUCCESS", **changes):
        result = {
            "code": "10000", "app_id": "test-app", "seller_id": "test-seller",
            "out_trade_no": order["id"], "trade_no": "trade-" + order["id"],
            "trade_status": status, "total_amount": "1.00",
        }
        result.update(changes)
        # Verify the actual raw signed subobject, including whitespace.
        signed = json.dumps(result, ensure_ascii=False, indent=2)
        return ('{"alipay_trade_query_response":' + signed + ',"sign":' + json.dumps(self.sign(signed)) + ',"sign_type":"RSA2"}').encode()

    def query(self, order, status="TRADE_SUCCESS", **changes):
        body = self.query_response(order, status, **changes)
        with patch("seecut_server.service.open_no_redirect", return_value=FakeResponse(body)):
            return self.service.refresh_order(self.user, order["id"])

    def assert_purchase(self, credits=100):
        wallet = self.service.wallet(self.user)
        self.assertEqual((wallet["available_credits"], wallet["held_credits"]), (credits, 0))
        purchases = [row for row in wallet["ledger"] if row["kind"] == "purchase"]
        self.assertEqual(len(purchases), 1)
        self.assertEqual(purchases[0]["delta_available"], credits)

    def test_same_intent_replays_snapshot_without_extending_deadline(self):
        original = self.create()
        self.clock[0] += 60
        with self.service.db.transaction() as connection:
            connection.execute("UPDATE plans SET price_fen=999,credits=999,active=0 WHERE id='plan-1'")
        replay = self.create()
        self.assertEqual((replay["id"], replay["amount_fen"], replay["credits"], replay["expires_at"]),
                         (original["id"], 100, 100, original["expires_at"]))
        self.assertEqual(replay["payment_action"]["expires_at"], original["expires_at"])
        first_biz = json.loads(urllib.parse.parse_qs(urllib.parse.urlsplit(original["payment_action"]["url"]).query)["biz_content"][0])
        second_biz = json.loads(urllib.parse.parse_qs(urllib.parse.urlsplit(replay["payment_action"]["url"]).query)["biz_content"][0])
        self.assertEqual(first_biz["time_expire"], "2023-11-15 06:43:20")
        self.assertEqual(first_biz["time_expire"], second_biz["time_expire"])
        self.assertNotIn("timeout_express", first_biz)
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)

    def test_same_key_different_plan_is_conflict(self):
        self.create()
        with self.assertRaises(ApiError) as caught:
            self.create(plan="plan-2")
        self.assertEqual((caught.exception.status, caught.exception.code), (409, "ORDER_IDEMPOTENCY_CONFLICT"))

    def test_intent_and_order_reads_are_account_scoped(self):
        original = self.create()
        other, _ = self.create_user("other@example.test")
        other_order = self.create(user=other)
        self.assertNotEqual(original["id"], other_order["id"])
        for operation in (self.service.order, self.service.refresh_order):
            with self.assertRaises(ApiError) as caught:
                operation(other, original["id"])
            self.assertEqual(caught.exception.status, 404)
        self.assertEqual([row["id"] for row in self.service.orders(other)["items"]], [other_order["id"]])

    def test_concurrent_same_intent_creates_one_order(self):
        with ThreadPoolExecutor(max_workers=6) as pool:
            orders = list(pool.map(lambda _: self.create(), range(6)))
        self.assertEqual(len({order["id"] for order in orders}), 1)
        with self.service.db.connect() as connection:
            self.assertEqual(connection.execute("SELECT count(*) FROM orders").fetchone()[0], 1)
            self.assertEqual(connection.execute("SELECT count(*) FROM order_idempotency").fetchone()[0], 1)

    def test_missing_invalid_key_never_creates_order(self):
        for key in ("", " " * 3, "x" * 129):
            with self.assertRaises(ApiError) as caught:
                self.create(key=key)
            self.assertEqual(caught.exception.code, "INVALID_IDEMPOTENCY_KEY")
        with self.service.db.connect() as connection:
            self.assertEqual(connection.execute("SELECT count(*) FROM orders").fetchone()[0], 0)

    def test_signing_failure_retries_existing_order_after_restart(self):
        with patch("seecut_server.service.subprocess.run", side_effect=OSError("test signer down")):
            with self.assertRaises(ApiError) as caught:
                self.create()
        self.assertEqual(caught.exception.code, "ALIPAY_SIGNING_UNAVAILABLE")
        with self.service.db.connect() as connection:
            original = dict(connection.execute("SELECT * FROM orders").fetchone())
        self.clock[0] += 45
        self.service = self.reopen()
        recovered = self.create()
        self.assertEqual((recovered["id"], recovered["expires_at"]), (original["id"], original["expires_at"]))

    def test_expired_replay_after_restart_has_no_new_payment_action_or_local_close(self):
        original = self.create()
        self.clock[0] = original["expires_at"]
        self.service = self.reopen()
        with patch.object(self.service, "_alipay_page_url", side_effect=AssertionError("must not renew")):
            replay = self.create()
        self.assertEqual((replay["id"], replay["status"], replay["expires_at"]),
                         (original["id"], "pending", original["expires_at"]))
        self.assertNotIn("payment_action", replay)
        self.assertEqual(self.service.orders(self.user)["items"][0]["expires_at"], original["expires_at"])

    def test_legacy_migration_preserves_unknown_deadline_and_balance(self):
        original = self.create()
        with self.service.db.connect() as connection:
            connection.execute("ALTER TABLE orders DROP COLUMN expires_at")
        self.service = self.reopen()
        row = self.service.order(self.user, original["id"])
        self.assertIsNone(row["expires_at"])
        self.assertEqual(row["status"], "pending")
        self.assertNotIn("payment_action", self.create())
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)
        self.service = self.reopen()
        self.assertIsNone(self.service.order(self.user, original["id"])["expires_at"])

    def test_actual_rsa2_notify_signature_accepts_original_rejects_tampering(self):
        order = self.create()
        fields = self.notification(order)
        self.assertTrue(self.service._verify_alipay_signature(fields, fields["sign"]))
        tampered = {**fields, "total_amount": "9.99"}
        self.assertFalse(self.service._verify_alipay_signature(tampered, fields["sign"]))
        with self.assertRaises(ApiError) as caught:
            self.service.process_alipay_notification(tampered)
        self.assertEqual(caught.exception.code, "INVALID_PAYMENT_SIGNATURE")
        with self.assertRaises(ApiError):
            self.service.process_alipay_notification({**fields, "sign_type": "RSA"})
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)
        self.service.process_alipay_notification(fields)
        self.assert_purchase()

    def test_correctly_signed_wrong_app_seller_amount_or_order_rejected(self):
        order = self.create()
        for changes, code in (({"app_id": "other"}, "PAYMENT_APP_MISMATCH"),
                              ({"seller_id": "other"}, "PAYMENT_SELLER_MISMATCH"),
                              ({"total_amount": "2.00"}, "PAYMENT_ORDER_MISMATCH"),
                              ({"out_trade_no": "missing"}, "PAYMENT_ORDER_MISMATCH")):
            with self.subTest(changes=changes), self.assertRaises(ApiError) as caught:
                self.service.process_alipay_notification(self.notification(order, **changes))
            self.assertEqual(caught.exception.code, code)
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)

    def test_actual_query_signature_rejects_tampered_response(self):
        order = self.create()
        body = self.query_response(order).replace(b'"total_amount": "1.00"', b'"total_amount": "2.00"')
        with patch("seecut_server.service.open_no_redirect", return_value=FakeResponse(body)):
            with self.assertRaises(ApiError) as caught:
                self.service.refresh_order(self.user, order["id"])
        self.assertEqual(caught.exception.code, "ALIPAY_QUERY_SIGNATURE_INVALID")
        self.assertEqual(self.query(order)["status"], "paid")
        self.assert_purchase()

    def test_signed_query_wrong_identity_or_amount_rejected(self):
        order = self.create()
        for changes in ({"app_id": "other"}, {"seller_id": "other"}, {"total_amount": "2.00"}, {"out_trade_no": "other"}):
            with self.subTest(changes=changes), self.assertRaises(ApiError) as caught:
                self.query(order, **changes)
            self.assertEqual(caught.exception.code, "PAYMENT_ORDER_MISMATCH")
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)

    def test_signed_business_failure_or_unknown_status_is_http_unresolved(self):
        order = self.create()
        self.clock[0] = order["expires_at"] + 1
        handler = type("QuietQueryHandler", (SeeCutHandler,), {"service": self.service, "log_message": lambda *a: None})
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True)
        thread.start()
        try:
            cases = [
                {"code": "40004", "sub_code": "ACQ.TRADE_NOT_EXIST", "trade_status": status}
                for status in ("WAIT_BUYER_PAY", "TRADE_SUCCESS", "TRADE_CLOSED")
            ] + [{"trade_status": status} for status in ("UNKNOWN", None, {}, [])]
            for changes in cases:
                with self.subTest(changes=changes):
                    connection = http.client.HTTPConnection(*server.server_address, timeout=3)
                    try:
                        body = self.query_response(order, **changes)
                        with patch("seecut_server.service.open_no_redirect", return_value=FakeResponse(body)):
                            connection.request("POST", f"/api/orders/{order['id']}/refresh", "{}",
                                               {"Content-Type": "application/json", "Authorization": "Bearer " + self.token})
                            response = connection.getresponse()
                            result = json.loads(response.read())
                        self.assertEqual(response.status, 502)
                        self.assertEqual(result["error"]["code"], "ALIPAY_QUERY_UNRESOLVED")
                        self.assertNotIn("payment_status_verified", result)
                    finally:
                        connection.close()
                    self.assertEqual(self.service.order(self.user, order["id"])["status"], "pending")
                    self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)
                    self.assertEqual(self.service.wallet(self.user)["ledger"], [])
                    with self.service.db.connect() as connection:
                        self.assertEqual(connection.execute("SELECT count(*) FROM orders").fetchone()[0], 1)
                        self.assertEqual(connection.execute("SELECT count(*) FROM payment_events").fetchone()[0], 0)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(2)

    def test_valid_wait_query_confirms_pending_even_after_expiry_without_mutation(self):
        order = self.create()
        for now in (self.clock[0], order["expires_at"] + 1):
            with self.subTest(now=now):
                self.clock[0] = now
                result = self.query(order, "WAIT_BUYER_PAY")
                self.assertIs(result["payment_status_verified"], True)
                self.assertEqual((result["id"], result["status"], result["expires_at"]),
                                 (order["id"], "pending", order["expires_at"]))
                self.assertNotIn("payment_action", result)
                self.assertNotIn("payment_status_verified", self.service.order(self.user, order["id"]))
                self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)
                self.assertEqual(self.service.wallet(self.user)["ledger"], [])
                with self.service.db.connect() as connection:
                    self.assertEqual(connection.execute("SELECT count(*) FROM orders").fetchone()[0], 1)
                    self.assertEqual(connection.execute("SELECT count(*) FROM payment_events").fetchone()[0], 0)

    def test_signed_wait_query_wrong_identity_or_amount_rejected(self):
        order = self.create()
        for changes in ({"app_id": "other"}, {"seller_id": "other"}, {"app_id": ""}, {"seller_id": ""},
                        {"total_amount": "2.00"}, {"out_trade_no": "other"}):
            with self.subTest(changes=changes), self.assertRaises(ApiError) as caught:
                self.query(order, "WAIT_BUYER_PAY", **changes)
            self.assertEqual((caught.exception.status, caught.exception.code), (400, "PAYMENT_ORDER_MISMATCH"))
        self.assertEqual(self.service.order(self.user, order["id"])["status"], "pending")
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)

    def test_signed_wait_query_requires_valid_trade_number_and_finite_amount(self):
        order = self.create()
        for changes in ({"trade_no": ""}, {"trade_no": " "}, {"trade_no": None}, {"trade_no": 123},
                        {"total_amount": "NaN"}, {"total_amount": "Infinity"}, {"total_amount": "1.001"}):
            with self.subTest(changes=changes), self.assertRaises(ApiError) as caught:
                self.query(order, "WAIT_BUYER_PAY", **changes)
            self.assertEqual((caught.exception.status, caught.exception.code), (502, "ALIPAY_QUERY_INVALID"))
        self.assertEqual(self.service.order(self.user, order["id"])["status"], "pending")
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)

    def test_signed_query_must_match_existing_provider_trade_number(self):
        order = self.create()
        with self.service.db.transaction() as connection:
            connection.execute("UPDATE orders SET provider_trade_no=? WHERE id=?", ("known-trade", order["id"]))
        for status in ("WAIT_BUYER_PAY", "TRADE_SUCCESS", "TRADE_CLOSED"):
            with self.subTest(status=status), self.assertRaises(ApiError) as caught:
                self.query(order, status, trade_no="other-trade")
            self.assertEqual(caught.exception.code, "PAYMENT_ORDER_MISMATCH")
        self.assertEqual(self.service.order(self.user, order["id"])["status"], "pending")
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)

    def test_verified_closed_notification_persists_and_late_success_credits_once(self):
        order = self.create()
        self.clock[0] = order["expires_at"] + 1
        closed = self.notification(order, "TRADE_CLOSED")
        self.service.process_alipay_notification(closed)
        self.service.process_alipay_notification(closed)
        self.service = self.reopen()
        self.assertEqual(self.service.order(self.user, order["id"])["status"], "closed")
        self.assertNotIn("payment_action", self.create())
        self.assertEqual(self.service.wallet(self.user)["available_credits"], 0)
        paid = self.notification(order)
        self.service.process_alipay_notification(paid)
        self.service.process_alipay_notification(paid)
        self.service.process_alipay_notification(closed)
        self.assertEqual(self.service.order(self.user, order["id"])["status"], "paid")
        self.assert_purchase()

    def test_verified_closed_query_persists_and_paid_never_downgrades(self):
        order = self.create()
        closed = self.query(order, "TRADE_CLOSED")
        self.assertEqual(closed["status"], "closed")
        self.assertIs(closed["payment_status_verified"], True)
        paid = self.query(order)
        self.assertEqual(paid["status"], "paid")
        self.assertIs(paid["payment_status_verified"], True)
        with patch("seecut_server.service.open_no_redirect", side_effect=AssertionError("paid is final")):
            cached = self.service.refresh_order(self.user, order["id"])
            self.assertEqual(cached["status"], "paid")
            self.assertNotIn("payment_status_verified", cached)
        self.assert_purchase()

    def test_unverified_or_mismatched_close_cannot_change_order(self):
        order = self.create()
        bad = {**self.notification(order, "TRADE_CLOSED"), "total_amount": "2.00"}
        with self.assertRaises(ApiError):
            self.service.process_alipay_notification(bad)
        with self.assertRaises(ApiError):
            self.query(order, "TRADE_CLOSED", total_amount="2.00")
        self.assertEqual(self.service.order(self.user, order["id"])["status"], "pending")

    def test_concurrent_callback_query_and_close_credit_once(self):
        order = self.create()
        success = self.notification(order)
        closed = self.notification(order, "TRADE_CLOSED")
        body = self.query_response(order)
        start = threading.Barrier(3)

        def action(number):
            start.wait(timeout=5)
            if number == 0:
                return self.service.refresh_order(self.user, order["id"])
            return self.service.process_alipay_notification(success if number == 1 else closed)

        with patch("seecut_server.service.open_no_redirect", side_effect=lambda *a, **k: FakeResponse(body)):
            with ThreadPoolExecutor(max_workers=3) as pool:
                list(pool.map(action, range(3)))
        self.assertEqual(self.service.order(self.user, order["id"])["status"], "paid")
        self.assert_purchase()

    def test_http_requires_key_and_replays_original_order(self):
        handler = type("QuietOrderHandler", (SeeCutHandler,), {"service": self.service, "log_message": lambda *a: None})
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True)
        thread.start()

        def request(key, plan="plan-1"):
            headers = {"Content-Type": "application/json", "Authorization": "Bearer " + self.token}
            if key is not None:
                headers["Idempotency-Key"] = key
            connection = http.client.HTTPConnection(*server.server_address, timeout=3)
            try:
                connection.request("POST", "/api/orders", json.dumps({"plan_id": plan}), headers)
                response = connection.getresponse()
                return response.status, json.loads(response.read())
            finally:
                connection.close()

        try:
            self.assertEqual(request(None)[0], 400)
            status, order = request("http-intent")
            self.assertEqual(status, 201)
            self.assertEqual(request("http-intent")[1]["id"], order["id"])
            self.assertEqual(request("http-intent", "plan-2")[0], 409)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(2)


if __name__ == "__main__":
    unittest.main()
