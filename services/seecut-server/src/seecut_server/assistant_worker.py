from __future__ import annotations

import threading
import hashlib

from .assistant_provider import ProviderFailure
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
        self.service.recover_interrupted()
        self._stop.clear()
        self._thread = threading.Thread(target=self._run, name='seecut-agent-worker', daemon=True)
        self._thread.start()

    def stop(self):
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=3)

    def process_once(self):
        import json
        operation = self.service.claim()
        if operation is None:
            return 0
        operation_id = operation['id']
        service = self.service.execution_service(operation)
        if operation['route_fingerprint'] != service.route_fingerprint():
            service.finish(operation_id, 'failed', {'input': 0, 'output': 0},
                                error={'code': 'ASSISTANT_ROUTE_CHANGED', 'message': 'Agent 服务配置已变更，请重新发起请求'})
            return 1
        try:
            prepared = service.prepare(json.loads(operation['request_json']))
            if prepared.input_token_upper_bound != operation['input_token_upper_bound'] or hashlib.sha256(prepared.payload).hexdigest() != operation['payload_digest']:
                raise ValueError('prepared input changed')
        except Exception:
            service.finish(operation_id, 'failed', {'input': 0, 'output': 0},
                                error={'code': 'ASSISTANT_INPUT_CONTRACT_CHANGED', 'message': 'Agent 输入规则已变更，本次未执行'})
            return 1
        result, count, size = None, 0, 0
        try:
            for event in service.provider.stream(operation_id, prepared,
                                                      lambda: service.cancelled(operation_id)):
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
            service.finish(operation_id, 'succeeded', result.get('usage'), result)
        except ProviderFailure as exc:
            if exc.known_not_executed or exc.usage is not None:
                try:
                    service.finish(operation_id, 'cancelled' if exc.cancelled else 'failed',
                        {'input': 0, 'output': 0} if exc.known_not_executed else exc.usage,
                        error={'code': 'ASSISTANT_PROVIDER_FAILED', 'message': 'Agent 服务未完成本次请求'})
                except Exception:
                    service.unknown(operation_id)
            else:
                service.unknown(operation_id)
        except Exception:
            service.unknown(operation_id)
        return 1

    def _run(self):
        while not self._stop.is_set():
            try:
                processed = self.process_once()
            except Exception:
                processed = 0
            if not processed:
                self._stop.wait(0.25)
