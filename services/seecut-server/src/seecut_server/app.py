from __future__ import annotations

from http.server import ThreadingHTTPServer
from pathlib import Path

from .config import Config
from .db import Database
from .http import SeeCutHandler
from .service import SeeCutService
from .worker import GenerationWorker
from .ratelimit import InMemoryRateLimiter


def create_service(config: Config | None = None) -> SeeCutService:
    active_config = config or Config.from_env()
    package_root = Path(__file__).resolve().parents[2]
    database = Database(active_config.database_path, package_root / "schema.sql")
    database.initialize()
    return SeeCutService(active_config, database)


def main() -> None:
    service = create_service()
    handler = type("ConfiguredSeeCutHandler", (SeeCutHandler,), {"service": service})
    server = ThreadingHTTPServer((service.config.host, service.config.port), handler)
    server.auth_rate_limiter = InMemoryRateLimiter(
        service.config.auth_rate_limit_per_minute,
        service.config.auth_rate_limit_window_seconds,
    )
    server.auth_rate_limit_window_seconds = service.config.auth_rate_limit_window_seconds
    worker = GenerationWorker(service)
    worker.start()
    print(f"SeeCut server listening on http://{service.config.host}:{service.config.port}")
    try:
        server.serve_forever()
    finally:
        worker.stop()
        server.server_close()


if __name__ == "__main__":
    main()
