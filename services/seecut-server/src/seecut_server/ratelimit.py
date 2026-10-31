from __future__ import annotations

import threading
import time


class InMemoryRateLimiter:
    """Bounded fixed-window limiter for low-volume account endpoints."""

    def __init__(self, limit: int, window_seconds: int, max_keys: int = 4096):
        self.limit = max(1, limit)
        self.window_seconds = max(1, window_seconds)
        self.max_keys = max(16, max_keys)
        self._lock = threading.Lock()
        self._buckets: dict[str, tuple[int, int]] = {}

    def allow(self, key: str, now: float | None = None) -> bool:
        current = int(time.time() if now is None else now)
        with self._lock:
            if len(self._buckets) >= self.max_keys and key not in self._buckets:
                cutoff = current - self.window_seconds
                self._buckets = {
                    item_key: value
                    for item_key, value in self._buckets.items()
                    if value[0] > cutoff
                }
                if len(self._buckets) >= self.max_keys:
                    oldest = min(self._buckets, key=lambda item_key: self._buckets[item_key][0])
                    del self._buckets[oldest]
            start, count = self._buckets.get(key, (current, 0))
            if current - start >= self.window_seconds:
                start, count = current, 0
            if count >= self.limit:
                self._buckets[key] = (start, count)
                return False
            self._buckets[key] = (start, count + 1)
            return True
