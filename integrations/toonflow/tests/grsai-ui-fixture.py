"""Isolated UI fixture: local account/ledger, synthetic provider, no network calls.

Run with PYTHONPATH=services/seecut-server/src:services/seecut-server/tests.
Login: r1@example.test / synthetic-ui-only. Never use this fixture in production.
"""
import argparse
import json
import os
from dataclasses import replace
from http.server import ThreadingHTTPServer
from pathlib import Path
from unittest.mock import patch
from uuid import uuid4

from seecut_server.app import create_service
from seecut_server.config import Config
from seecut_server.grsai_catalog import catalog
from seecut_server.http import SeeCutHandler
from seecut_server.model_pricing import parse_price_catalog
from seecut_server.ratelimit import InMemoryRateLimiter
from seecut_server.service import SeeCutService
from seecut_server.worker import GenerationWorker
from test_assistant_contract import FakeAssistant
from test_generation_robustness import PNG
from test_service import FakeEmailSender


class SyntheticGrsai:
    configured = True

    def __init__(self):
        self.jobs = {}

    def create(self, request, references):
        identity = 'synthetic-' + uuid4().hex
        self.jobs[identity] = request['model'].startswith(('nano-', 'gpt-'))
        return dict(id=identity, status='running', results=[])

    def result(self, identity):
        return dict(id=identity, status='succeeded' if self.jobs[identity] else 'failed',
                    results=[dict(url='https://fixture.invalid/pixel')] if self.jobs[identity] else [])


class FixtureService(SeeCutService):
    def _download_provider_result(self, url, kind, **kwargs):
        assert url == 'https://fixture.invalid/pixel' and kind == 'image'
        return PNG, 'image/png'


args = argparse.ArgumentParser()
args.add_argument('--data-dir', type=Path, required=True)
args.add_argument('--port', type=int, default=18741)
args.add_argument('--resume', action='store_true', help='Resume only a previously created synthetic fixture')
args = args.parse_args()
if args.resume:
    if not (args.data_dir/'fixture.sqlite').is_file():
        raise SystemExit('Synthetic fixture database missing')
else:
    args.data_dir.mkdir(parents=True, exist_ok=False)
with patch.dict(os.environ, {'SECUT_ENV': 'test', 'SECUT_EXPOSE_TEST_TOKENS': 'true'}, clear=True):
    config = Config.from_env()
models = catalog()
prices = [dict(provider='grsai', model=model['id'], kind=model['kind'], version='synthetic-ui-v1',
               cost_status='verified', source='SYNTHETIC UI FIXTURE - NOT SUPPLIER COST',
               verified_at='2026-10-15T00:00:00Z',
               costs={'media': dict(unit='per_image' if model['kind'] == 'image' else 'per_second',
                                   yuan='0.04', selectors={})}) for model in models]
agent = dict(model_id='default', max_output_tokens=200, max_steps=2, max_turn_credits=10)
efforts = tuple(dict(id=identity, label=label, policy_revision='synthetic-v1', agent=agent,
                    generation={kind+'_model_ids': [m['id'] for m in models if m['kind'] == kind]
                                for kind in ('image', 'video')})
                for identity, label in [('low', '快速'), ('high', '高强度')])
config = replace(config, database_path=args.data_dir/'fixture.sqlite', storage_path=args.data_dir/'storage',
                 public_base_url=f'http://127.0.0.1:{args.port}',
                 assistant_base_url='http://127.0.0.1:1', assistant_model='synthetic-only',
                 assistant_api_key='synthetic-ui-only', assistant_pricing_version='synthetic-v1',
                 assistant_context_window=1000, assistant_max_output_tokens=200,
                 assistant_input_rate=2, assistant_output_rate=5, assistant_max_turn_credits=10,
                 model_cost_pricing_enabled=True, model_price_catalog=parse_price_catalog(json.dumps(prices)),
                 efforts=efforts, worker_interval_seconds=1)
service = create_service(config)
service = FixtureService(config, service.db, emailer=FakeEmailSender(config),
                         assistant_provider=FakeAssistant(), grsai=SyntheticGrsai())
if not args.resume:
    registration = service.register('r1@example.test', 'synthetic-ui-only')
    service.verify_email('r1@example.test', registration['verification_token'])
    with service.db.transaction() as connection:
        connection.execute('UPDATE wallets SET available_credits=2000')
else:
    with service.db.transaction() as connection:
        for row in connection.execute('SELECT upstream_task_id,kind FROM generation_tasks WHERE upstream_task_id IS NOT NULL'):
            service.grsai.jobs[row['upstream_task_id']] = row['kind'] == 'image'
handler = type('FixtureHandler', (SeeCutHandler,), {'service': service, 'log_message': lambda *args: None})
server = ThreadingHTTPServer(('127.0.0.1', args.port), handler)
server.auth_rate_limiter = InMemoryRateLimiter(100, 60)
server.auth_rate_limit_window_seconds = 60
worker = GenerationWorker(service)
worker.start()
print(f'Synthetic UI fixture ready on 127.0.0.1:{args.port}; no provider network.', flush=True)
try:
    server.serve_forever()
finally:
    worker.stop()
    server.server_close()
