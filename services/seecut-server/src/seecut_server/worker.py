from __future__ import annotations

import threading
import time

from .service import SeeCutService


class GenerationWorker:
    """Durable polling worker; provider calls happen outside HTTP handlers."""

    def __init__(self, service: SeeCutService):
        self.service = service
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._last_cleanup = 0.0

    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._thread = threading.Thread(target=self._run, name="seecut-generation-worker", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=3)

    def process_once(self) -> int:
        processed = 0
        for task in self.service.claim_generation_tasks(limit=1):
            self.service.process_generation_task(task)
            processed += 1
        now = time.monotonic()
        if now - self._last_cleanup >= self.service.config.cleanup_interval_seconds:
            self.service.cleanup_expired()
            self._last_cleanup = now
        return processed

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                self.process_once()
            except Exception:
                # The task remains durable and its lease expires for recovery.
                pass
            self._stop.wait(self.service.config.worker_interval_seconds)
