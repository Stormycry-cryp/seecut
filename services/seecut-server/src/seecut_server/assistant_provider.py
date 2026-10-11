"""Server-owned, single-dispatch OpenAI-compatible streaming transport."""
from __future__ import annotations

import json
from dataclasses import dataclass
import time
import urllib.error
import urllib.parse
import urllib.request
from ipaddress import ip_address

from . import assistant_contract as contract
from .network import open_no_redirect
from .security import new_id


class ProviderFailure(Exception):
    def __init__(self, *, known_not_executed=False, usage=None, cancelled=False):
        super().__init__('Agent provider did not complete')
        self.known_not_executed = known_not_executed
        self.usage = usage
        self.cancelled = cancelled


@dataclass(frozen=True)
class PreparedInference:
    payload: bytes
    input_token_upper_bound: int
    token_contract_id: str


class AssistantProvider:
    # A generic compatible endpoint does not prove tokenizer/model wrapping rules.
    # A verified, model-specific adapter must provide both this identity and prepare.
    token_contract_id = None

    def prepare(self, request):
        raise ValueError('No verified input token contract')

    def __init__(self, config):
        self.config = config
        parsed = urllib.parse.urlsplit(config.assistant_base_url)
        try:
            loopback = ip_address(parsed.hostname or '').is_loopback
        except ValueError:
            loopback = parsed.hostname == 'localhost'
        self.configured = bool(config.assistant_model and config.assistant_api_key and parsed.hostname and
                               (parsed.scheme == 'https' or parsed.scheme == 'http' and loopback) and
                               not parsed.username and not parsed.password and not parsed.query and not parsed.fragment)

    @staticmethod
    def lookup(operation_id):
        # Chat Completions has no authoritative status-by-client-id lookup.
        # Retain UNKNOWN rather than replaying a request or inventing a refund.
        return None

    def payload(self, request):
        messages = [{'role': 'system', 'content': request['system']}]
        for source in request['messages']:
            parts = []
            for part in source['content']:
                if part['type'] == 'text':
                    parts.append({'type': 'text', 'text': part['text']})
                else:
                    parts.append({'type': 'image_url', 'image_url': {'url': f"data:{part['mimeType']};base64,{part['data']}"}})
            item = {'role': source['role'], 'content': parts}
            if source['role'] == 'tool':
                item['tool_call_id'] = source['toolCallId']
                # Tool outputs in the public MCP may contain images. They must retain their data;
                # deployments must select a compatible gateway before enabling the image capability.
            if source.get('toolCalls'):
                item['tool_calls'] = [{'id': call['id'], 'type': 'function', 'function': {
                    'name': call['name'], 'arguments': contract.canonical(call['arguments'])}} for call in source['toolCalls']]
            messages.append(item)
        payload = dict(model=self.config.assistant_model, messages=messages, stream=True,
                       stream_options={'include_usage': True}, max_tokens=request['maxOutputTokens'])
        if request['tools']:
            payload['tools'] = [{'type': 'function', 'function': tool} for tool in request['tools']]
        return contract.canonical(payload).encode('utf-8')

    def stream(self, operation_id, prepared, cancelled):
        if not self.configured or not self.token_contract_id or prepared.token_contract_id != self.token_contract_id:
            raise ProviderFailure(known_not_executed=True)
        if cancelled():
            raise ProviderFailure(known_not_executed=True, cancelled=True)
        upstream = urllib.request.Request(self.config.assistant_base_url + '/chat/completions',
            data=prepared.payload, method='POST', headers={
                'Content-Type': 'application/json', 'Accept': 'text/event-stream',
                'Authorization': 'Bearer ' + self.config.assistant_api_key, 'Idempotency-Key': operation_id})
        started = time.monotonic()
        total, text_parts, calls, usage, finish, ended = 0, [], {}, None, None, False
        try:
            with open_no_redirect(upstream, timeout=min(30, self.config.provider_timeout_seconds)) as response:
                if 'text/event-stream' not in response.headers.get('Content-Type', ''):
                    raise ProviderFailure()
                # Do not drop the connection on cancel: continue the bounded usage drain so
                # accepted work can be billed accurately. The private runtime checks
                # persisted cancellation before delivering results or invoking tools.
                lines = self._lines(response, started)
                while True:
                    if time.monotonic() - started > min(120, self.config.provider_timeout_seconds):
                        raise ProviderFailure()
                    line = next(lines, None)
                    if line is None:
                        break
                    total += len(line)
                    if len(line) > 262144 or total > min(4 * 1024 * 1024, self.config.max_provider_response_bytes):
                        raise ProviderFailure()
                    line = line.strip()
                    if not line.startswith(b'data:'):
                        continue
                    data = line[5:].strip()
                    if data == b'[DONE]':
                        ended = True
                        break
                    chunk = json.loads(data)
                    if chunk.get('error'):
                        raise ProviderFailure()
                    if chunk.get('usage') is not None:
                        source = chunk['usage']
                        usage = contract.usage({'input': source.get('prompt_tokens'), 'output': source.get('completion_tokens')})
                    for choice in chunk.get('choices', []):
                        if choice.get('index') != 0:
                            raise ProviderFailure()
                        if choice.get('finish_reason') is not None:
                            finish = choice['finish_reason']
                        delta = choice.get('delta') or {}
                        if delta.get('content'):
                            part = contract.text(delta['content'])
                            text_parts.append(part)
                            for offset in range(0, len(part), 2048):
                                yield {'type': 'text_delta', 'text': part[offset:offset + 2048]}
                        for tool in delta.get('tool_calls', []):
                            index = contract.integer(tool.get('index'), 0, 15)
                            entry = calls.setdefault(index, {'id': '', 'name': '', 'arguments': ''})
                            if tool.get('id'):
                                entry['id'] = contract.identity(tool['id'])
                            function = tool.get('function') or {}
                            if function.get('name'):
                                entry['name'] += contract.text(function['name'], 128)
                            if function.get('arguments'):
                                fragment = contract.text(function['arguments'], 16384)
                                entry['arguments'] += fragment
                                if entry['id']:
                                    yield {'type': 'tool_delta', 'id': entry['id'], 'text': fragment}
                if not ended or finish not in {'stop', 'tool_calls', 'length'}:
                    raise ProviderFailure()
                final_calls = [{'id': call['id'], 'name': call['name'], 'arguments': json.loads(call['arguments'])} for call in calls.values()]
                result = {'type': 'done', 'message': {'id': new_id('amsg'), 'role': 'assistant',
                    'content': [{'type': 'text', 'text': ''.join(text_parts)}], 'toolCalls': final_calls},
                    'stopReason': 'tools' if final_calls else ('length' if finish == 'length' else 'stop')}
                if usage is not None:
                    result['usage'] = usage
                yield contract.model_event(result)
        except urllib.error.HTTPError as exc:
            exc.close()
            raise ProviderFailure() from None
        except ProviderFailure:
            raise
        except Exception:
            # Only a complete stream with final usage proves cost after invalid output.
            raise ProviderFailure(usage=usage if ended and finish in {'stop', 'tool_calls', 'length'} else None) from None

    def _lines(self, response, started):
        pending = bytearray()
        while True:
            if time.monotonic() - started > min(120, self.config.provider_timeout_seconds):
                raise ProviderFailure()
            # read1 performs at most one underlying read. Unlike readline, a peer
            # dripping bytes without a newline cannot bypass the absolute deadline.
            chunk = response.read1(16384)
            if not chunk:
                if pending:
                    yield bytes(pending)
                return
            pending.extend(chunk)
            while b'\n' in pending:
                line, _, rest = pending.partition(b'\n')
                pending = bytearray(rest)
                yield bytes(line)
            if len(pending) > 262144:
                raise ProviderFailure()


def make_assistant_provider(config, db):
    if config.assistant_model == 'glm-5.3-flash':
        from .assistant_glm import GLMAssistantProvider
        return GLMAssistantProvider(config, db)
    return AssistantProvider(config)
