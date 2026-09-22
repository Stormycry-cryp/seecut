from __future__ import annotations

import unittest

from seecut_server.http import _rate_limit_client_ip


class RateLimitClientIpTest(unittest.TestCase):
    def test_loopback_proxy_uses_valid_real_ip(self):
        self.assertEqual(
            _rate_limit_client_ip("127.0.0.1", ["203.0.113.18"]),
            "203.0.113.18",
        )
        self.assertEqual(
            _rate_limit_client_ip("::1", ["2001:db8::18"]),
            "2001:db8::18",
        )
        self.assertEqual(
            _rate_limit_client_ip("::ffff:127.0.0.1", ["203.0.113.18"]),
            "203.0.113.18",
        )

    def test_direct_client_cannot_override_peer_ip(self):
        self.assertEqual(
            _rate_limit_client_ip("198.51.100.7", ["203.0.113.18"]),
            "198.51.100.7",
        )

    def test_loopback_proxy_rejects_missing_invalid_or_duplicate_header(self):
        self.assertEqual(_rate_limit_client_ip("127.0.0.1", []), "127.0.0.1")
        self.assertEqual(
            _rate_limit_client_ip("127.0.0.1", ["203.0.113.18, 198.51.100.7"]),
            "127.0.0.1",
        )
        self.assertEqual(
            _rate_limit_client_ip("127.0.0.1", [" 203.0.113.18"]),
            "127.0.0.1",
        )
        self.assertEqual(
            _rate_limit_client_ip("127.0.0.1", ["203.0.113.18", "198.51.100.7"]),
            "127.0.0.1",
        )

    def test_invalid_peer_falls_back_to_unknown(self):
        self.assertEqual(
            _rate_limit_client_ip("not-an-ip", ["203.0.113.18"]),
            "unknown",
        )


if __name__ == "__main__":
    unittest.main()
