"""Loopback-only real SeeCut HTTP service with disposable DB and synthetic login."""
import json
import sys
from pathlib import Path
from http.server import ThreadingHTTPServer
import threading

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "services/seecut-server/tests"))
from test_flow_projects import FlowFixture
from seecut_server.http import SeeCutHandler

fixture = FlowFixture()
fixture.setUp()
class Handler(SeeCutHandler):
    service = fixture.service
    def log_message(self, *_args): pass
server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
print(json.dumps({"port": server.server_port}), flush=True)
try:
    sys.stdin.readline()
finally:
    server.shutdown()
    with fixture.db.connect() as connection:
        counts = {table: connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
                  for table in ("generation_tasks", "generation_billings", "ledger_entries", "flow_snapshot_requests", "flow_snapshots")}
    print(json.dumps({"finalCounts": counts}), flush=True)
    fixture.doCleanups()
