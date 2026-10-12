from __future__ import annotations

import threading
import hashlib

from .assistant_provider import ProviderFailure, ProviderUsage, PreparedUsageInference
from .assistant_execution_lock import execution_lock
from . import assistant_contract as contract


class AssistantWorker:
    """One bounded model execution at a time; dispatching is durable before I/O."""
    def __init__(self, service):
        self.service = service
        self._stop = threading.Event()
        self._thread = None

    def start(self):
        if self._thread and self._thread.is_alive():
            return
        self._stop.clear()
        self._thread = threading.Thread(target=self._run, name='seecut-agent-worker', daemon=True)
        self._thread.start()

    def stop(self):
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=3)

    def process_once(self):
        with execution_lock(self.service.db.path, required=False) as acquired:
            if acquired is False:
                return 0
            return self._process_once()

    def _process_once(self):
        import json
        operation = self.service.claim()
        if operation is None:
            return 0
        operation_id = operation['id']
        try:
            service = self.service.execution_service(operation)
        except Exception:
            self.service.unknown(operation_id)
            return 1
        if operation['route_fingerprint'] != service.route_fingerprint():
            service.finish(operation_id, 'failed', {'input': 0, 'output': 0},
                                error={'code': 'ASSISTANT_ROUTE_CHANGED', 'message': 'Agent 服务配置已变更，请重新发起请求'}, known_not_executed=True)
            return 1
        try:
            if operation['billing_mode'] == 'provider_usage_capped_v1':
                frozen = service.billing_contract
                if frozen is None or not isinstance(operation['prepared_payload'], bytes):
                    raise ValueError('frozen provider wire missing')
                prepared = PreparedUsageInference(operation['prepared_payload'], frozen['usage_contract_id'])
            else:
                prepared = service.prepare(json.loads(operation['request_json']), user=operation['user_id'], budget_id=operation['budget_id'], client_request_id=operation['client_request_id'])
            if (operation['billing_mode'] != 'provider_usage_capped_v1' and prepared.input_token_upper_bound != operation['input_token_upper_bound']) or hashlib.sha256(prepared.payload).hexdigest() != operation['payload_digest']:
                raise ValueError('prepared input changed')
        except Exception:
            service.finish(operation_id, 'failed', {'input': 0, 'output': 0},
                                error={'code': 'ASSISTANT_INPUT_CONTRACT_CHANGED', 'message': 'Agent 输入规则已变更，本次未执行'}, known_not_executed=True)
            return 1
        result, evidence, count, size = None, None, 0, 0
        try:
            for event in service.provider.stream(operation_id, prepared,
                                                      lambda: service.cancelled(operation_id)):
                if isinstance(event, ProviderUsage):
                    service.observe_provider_usage(operation_id, event)
                    evidence = event
                    continue
                contract.model_event(event)
                count += 1
                size += len(contract.canonical(event).encode())
                if count > 8192 or size > 4 * 1024 * 1024 or result is not None:
                    raise ProviderFailure()
                if event['type'] == 'done':
                    result = event
                else:
                    service.append_model(operation_id, event)
            if result is None:
                raise ProviderFailure()
            service.finish(operation_id, 'succeeded', result.get('usage'), result, provider_usage=evidence)
        except ProviderFailure as exc:
            if exc.known_not_executed or exc.usage is not None:
                try:
                    service.finish(operation_id, 'cancelled' if exc.cancelled else 'failed',
                        {'input': 0, 'output': 0} if exc.known_not_executed else exc.usage,
                        error={'code': 'ASSISTANT_PROVIDER_FAILED', 'message': 'Agent 服务未完成本次请求'},
                        provider_usage=evidence, known_not_executed=exc.known_not_executed)
                except Exception:
                    service.unknown(operation_id)
            else:
                service.unknown(operation_id)
        except Exception:
            service.unknown(operation_id)
        return 1

    def _run(self):
        recovered = False
        while not self._stop.is_set():
            try:
                with execution_lock(self.service.db.path, required=False) as acquired:
                    if acquired is False:
                        processed = 0
                    else:
                        if not recovered:
                            self.service.recover_interrupted()
                            recovered = True
                        processed = self._process_once()
            except Exception:
                processed = 0
            if not processed:
                self._stop.wait(0.25)
