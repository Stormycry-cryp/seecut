"""Domestic GLM wire adapter over the existing durable Agent operation protocol.

Flash tokenizer support is not documented yet. Production stays unavailable until
an independently verified, server-owned input counter is supplied in code.
"""
from __future__ import annotations

import json
import time
import urllib.error
import urllib.parse
import urllib.request

from . import assistant_contract as contract
from .assistant_provider import AssistantProvider, PreparedInference, PreparedUsageInference, ProviderUsage, ProviderFailure
from .model_pricing import billing_mode
from .network import open_no_redirect
from .security import new_id

MODEL = 'glm-5.3-flash'
DOMESTIC_BASE = 'https://open.bigmodel.cn/api/paas/v4'


class GLMAssistantProvider(AssistantProvider):
    def __init__(self, config, db, *, input_counter=None, token_contract_id=None, usage_contract_id=None):
        super().__init__(config)
        self.db = db
        self.counter = input_counter
        self.token_contract_id = token_contract_id if callable(input_counter) and isinstance(token_contract_id, str) and contract.IDENTITY.fullmatch(token_contract_id) else None
        self.billing_mode = billing_mode(config.assistant_billing_mode)
        self.usage_contract_id = usage_contract_id if (
            self.billing_mode == 'provider_usage_capped_v1' and isinstance(usage_contract_id, str)
            and contract.IDENTITY.fullmatch(usage_contract_id)) else None
        parsed = urllib.parse.urlsplit(config.assistant_base_url)
        local_fixture = config.env == 'test' and parsed.hostname in {'127.0.0.1', 'localhost'} and parsed.scheme == 'http'
        self.configured = self.configured and config.assistant_model == MODEL and not config.assistant_images and (config.assistant_base_url == DOMESTIC_BASE or local_fixture)

    def prepare_scoped(self, request, scope):
        if not self.configured or not (self.usage_contract_id or self.token_contract_id) or not isinstance(scope, dict):
            raise ValueError('GLM input contract unavailable')
        if set(scope) != {'user_id', 'budget_id', 'client_request_id'}:
            raise ValueError('GLM input scope unavailable')
        for value in scope.values():
            contract.identity(value)
        payload = json.loads(super().payload(request))
        for message in payload['messages']:
            if isinstance(message['content'], list):
                if any(part['type'] != 'text' for part in message['content']):
                    raise ValueError('GLM image input unavailable')
                message['content'] = ''.join(part['text'] for part in message['content'])
        payload.pop('stream_options', None)
        payload.update(thinking={'type': 'enabled', 'clear_thinking': True}, tool_stream=True)
        payload['request_id'] = 'seecut-' + contract.digest(scope | {'request': request})[:56]
        last_user = max((index for index, item in enumerate(request['messages']) if item['role'] == 'user'), default=-1)
        for index, source in enumerate(request['messages']):
            if source['role'] != 'assistant' or not source.get('toolCalls') or index < last_user:
                continue
            with self.db.connect() as connection:
                row = connection.execute('''SELECT g.* FROM assistant_glm_messages g
                    JOIN assistant_operations o ON o.id=g.operation_id
                    WHERE g.message_id=? AND o.user_id=? AND o.budget_id=? AND g.model=?
                    AND o.state='succeeded' AND o.billing_state IN ('captured','released')''',
                    (source['id'], scope['user_id'], scope['budget_id'], MODEL)).fetchone()
            if row is None or row['message_digest'] != contract.digest(source):
                raise ValueError('GLM tool context unavailable')
            payload['messages'][index + 1]['reasoning_content'] = row['reasoning']
        frozen = contract.canonical(payload).encode()
        if self.billing_mode == 'provider_usage_capped_v1':
            if not self.usage_contract_id:
                raise ValueError('GLM provider usage contract unavailable')
            return PreparedUsageInference(frozen, self.usage_contract_id)
        count = self.counter(frozen)
        contract.integer(count, 0, self.config.assistant_context_window)
        return PreparedInference(frozen, count, self.token_contract_id)

    @staticmethod
    def _usage(source):
        value = contract.usage({'input': source.get('prompt_tokens'), 'output': source.get('completion_tokens')})
        if 'total_tokens' in source and contract.integer(source['total_tokens'], 0, 2_000_000) != value['input'] + value['output']:
            raise ValueError('GLM usage mismatch')
        return value

    @staticmethod
    def _provider_usage(source):
        from copy import deepcopy
        classified = None
        try:
            total = GLMAssistantProvider._usage(source)
            details = source.get('prompt_tokens_details')
            if not isinstance(details, dict) or 'cached_tokens' not in details:
                raise ValueError('Cache classification missing')
            cached = contract.integer(details['cached_tokens'], 0, total['input'])
            classified = dict(input=total['input'], cached_input=cached, output=total['output'])
        except Exception:
            pass
        return ProviderUsage(deepcopy(source), classified)

    def _chunks(self, response, started):
        content_type = response.headers.get('Content-Type', '').lower()
        if 'text/event-stream' in content_type:
            total = 0
            for line in self._lines(response, started):
                total += len(line)
                if len(line) > 262144 or total > min(4 * 1024 * 1024, self.config.max_provider_response_bytes):
                    raise ProviderFailure()
                line = line.strip()
                if not line.startswith(b'data:'):
                    continue
                data = line[5:].strip()
                if data == b'[DONE]':
                    yield None
                    return
                yield json.loads(data)
        elif 'application/json' in content_type:
            body = bytearray()
            while True:
                if time.monotonic() - started > min(120, self.config.provider_timeout_seconds):
                    raise ProviderFailure()
                chunk = response.read1(16384)
                if not chunk:
                    break
                body.extend(chunk)
                if len(body) > min(4 * 1024 * 1024, self.config.max_provider_response_bytes):
                    raise ProviderFailure()
            value = json.loads(body)
            choices = value.get('choices')
            if not isinstance(choices, list) or len(choices) != 1:
                raise ProviderFailure()
            for choice in choices:
                message = choice.get('message')
                if not isinstance(message, dict):
                    raise ProviderFailure()
                if 'tool_calls' in message:
                    message = {**message, 'tool_calls': [{**tool, 'index': index} for index, tool in enumerate(message['tool_calls'])]}
                yield {**value, 'choices': [{**choice, 'delta': message}]}
            yield None
        else:
            raise ProviderFailure()

    def stream(self, operation_id, prepared, cancelled):
        verified = self.billing_mode == 'verified_input_v1' and isinstance(prepared, PreparedInference) and prepared.token_contract_id == self.token_contract_id and self.token_contract_id
        capped = self.billing_mode == 'provider_usage_capped_v1' and isinstance(prepared, PreparedUsageInference) and prepared.usage_contract_id == self.usage_contract_id and self.usage_contract_id
        if not self.configured or not (verified or capped):
            raise ProviderFailure(known_not_executed=True)
        if cancelled():
            raise ProviderFailure(known_not_executed=True, cancelled=True)
        started = time.monotonic()
        usage, raw_usage, ended, finish, text, reasoning, calls = None, None, False, None, [], [], {}
        reasoning_bytes = 0
        upstream = urllib.request.Request(self.config.assistant_base_url + '/chat/completions', data=prepared.payload,
            method='POST', headers={'Content-Type': 'application/json', 'Accept': 'text/event-stream',
                                   'Authorization': 'Bearer ' + self.config.assistant_api_key})
        try:
            with open_no_redirect(upstream, timeout=min(30, self.config.provider_timeout_seconds)) as response:
                if response.status != 200:
                    raise ProviderFailure()
                for chunk in self._chunks(response, started):
                    if time.monotonic() - started > min(120, self.config.provider_timeout_seconds):
                        raise ProviderFailure()
                    if chunk is None:
                        ended = True
                        break
                    if not isinstance(chunk, dict) or chunk.get('error'):
                        raise ProviderFailure()
                    if chunk.get('usage') is not None:
                        raw_usage = chunk['usage']
                        if capped:
                            # Preserve even malformed/interrupted reports. This
                            # observation is never a confirmed billable amount;
                            # classification is emitted only after stream end.
                            from copy import deepcopy
                            yield ProviderUsage(deepcopy(raw_usage), None)
                        usage = self._usage(chunk['usage'])
                    choices = chunk.get('choices', [])
                    if not isinstance(choices, list) or len(choices) > 1:
                        raise ProviderFailure()
                    for choice in choices:
                        if choice.get('index') != 0 or finish is not None and choice.get('delta'):
                            raise ProviderFailure()
                        if choice.get('finish_reason') is not None:
                            finish = choice['finish_reason']
                        delta = choice.get('delta') or {}
                        if delta.get('reasoning_content'):
                            part = contract.text(delta['reasoning_content'])
                            reasoning.append(part)
                            reasoning_bytes += len(part.encode())
                            if reasoning_bytes > 131072:
                                raise ProviderFailure()
                        if delta.get('content'):
                            part = contract.text(delta['content']); text.append(part)
                            if not cancelled():
                                for offset in range(0, len(part), 2048):
                                    yield {'type': 'text_delta', 'text': part[offset:offset + 2048]}
                        for tool in delta.get('tool_calls', []):
                            index = contract.integer(tool.get('index'), 0, 15)
                            entry = calls.setdefault(index, {'id': '', 'name': '', 'arguments': ''})
                            if tool.get('id'):
                                identity = contract.identity(tool['id'])
                                if entry['id'] and entry['id'] != identity:
                                    raise ProviderFailure()
                                entry['id'] = identity
                            function = tool.get('function') or {}
                            entry['name'] += contract.text(function.get('name', ''), 128)
                            fragment = contract.text(function.get('arguments', ''), 16384)
                            entry['arguments'] += fragment
                            if len(entry['arguments'].encode()) > 16384 or len(entry['name'].encode()) > 128:
                                raise ProviderFailure()
                            if fragment and entry['id'] and not cancelled():
                                yield {'type': 'tool_delta', 'id': entry['id'], 'text': fragment}
                if not ended or usage is None:
                    raise ProviderFailure()
                if capped:
                    yield self._provider_usage(raw_usage)
                if cancelled():
                    raise ProviderFailure(usage=usage, cancelled=True)
                if finish in {'sensitive', 'network_error', 'model_context_window_exceeded'}:
                    raise ProviderFailure(usage=usage)
                if finish not in {'stop', 'tool_calls', 'length'}:
                    raise ProviderFailure()
                message = {'id': new_id('amsg'), 'role': 'assistant', 'content': [{'type': 'text', 'text': ''.join(text)}],
                    'toolCalls': [{'id': call['id'], 'name': call['name'], 'arguments': json.loads(call['arguments'])} for call in calls.values()]}
                allowed_tools = {tool['function']['name'] for tool in json.loads(prepared.payload).get('tools', [])}
                if any(call['name'] not in allowed_tools for call in calls.values()):
                    raise ValueError('GLM returned an unrequested tool')
                if bool(calls) != (finish == 'tool_calls'):
                    raise ValueError('GLM finish reason mismatch')
                result = contract.model_event({'type': 'done', 'message': message, 'usage': usage,
                    'stopReason': 'tools' if calls else 'length' if finish == 'length' else 'stop'})
                if calls:
                    with self.db.transaction(immediate=True) as connection:
                        connection.execute('INSERT INTO assistant_glm_messages VALUES(?,?,?,?,?)',
                            (message['id'], operation_id, MODEL, contract.digest(message), ''.join(reasoning)))
                yield result
        except urllib.error.HTTPError as error:
            # Only documented pre-inference errors prove zero model cost.
            known = False
            try:
                value = json.loads(error.read(16385))
                code = str(value.get('error', {}).get('code'))
                known = (error.code == 401 and code in {'1000','1001','1003','1005'}) or (error.code == 403 and code == '1220') or (error.code == 400 and code in {'1210','1211','1212','1213','1214','1215','1221','1222','1261'}) or (error.code == 429 and code in {'1113','1302','1305','1308','1309','1310','1311','1313','1314','1315','1316','1317','1318','1319','1320','1321'})
            except Exception:
                pass
            finally:
                error.close()
            raise ProviderFailure(known_not_executed=known) from None
        except ProviderFailure:
            raise
        except Exception:
            raise ProviderFailure(usage=usage if ended and finish in {'stop','tool_calls','length'} else None) from None
