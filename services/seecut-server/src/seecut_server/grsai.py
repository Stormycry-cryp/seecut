"""Server-only GrsAI asynchronous media adapter; never retries paid POSTs."""
from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request

from .network import open_no_redirect, read_limited

ORIGINS = frozenset({'https://grsai.dakka.com.cn', 'https://grsaiapi.com'})


class GrsaiError(RuntimeError):
    def __init__(self, code, message, *, definitely_not_submitted=False):
        super().__init__(message)
        self.code, self.message = code, message
        self.retryable = not definitely_not_submitted


class GrsaiClient:
    def __init__(self, config):
        self.config = config

    @property
    def configured(self):
        return bool(self.config.grsai_api_key) and self.config.grsai_base_url in ORIGINS

    def create(self, request, references):
        body = {'model': request['model'], 'prompt': request['prompt'], 'replyType': 'async'}
        if request['model'].startswith('nano-banana'):
            body['aspectRatio'] = request['aspect_ratio']
            if request['model'] != 'nano-banana-2-lite':
                body['imageSize'] = request['size']
        elif request['model'].startswith('gpt-image'):
            body.update(aspectRatio=request['size'], quality=request['quality'])
        else:
            ratio = request['aspect_ratio']
            if request['model'] == 'minimax-h3':
                ratio = {'16:9': 'landscape', '9:16': 'portrait'}[ratio]
            body.update(aspectRatio=ratio, resolution=request['resolution'], duration=request['duration'])
        for kind, field in [('image', 'images'), ('audio', 'audios')]:
            values = [url for media, url in references if media == kind]
            if values:
                body[field] = values
        return self._request('POST', '/v1/api/generate', body)

    def result(self, task_id):
        return self._request('GET', '/v1/api/result?' + urllib.parse.urlencode({'id': task_id}))

    def _request(self, method, path, body=None):
        if not self.configured:
            raise GrsaiError('GRSAI_NOT_CONFIGURED', 'GrsAI 尚未配置', definitely_not_submitted=True)
        request = urllib.request.Request(self.config.grsai_base_url + path, method=method,
            data=None if body is None else json.dumps(body, ensure_ascii=False).encode('utf-8'),
            headers={'Authorization': 'Bearer ' + self.config.grsai_api_key, 'Content-Type': 'application/json', 'Accept': 'application/json'})
        try:
            with open_no_redirect(request, timeout=self.config.provider_timeout_seconds) as response:
                raw = read_limited(response, self.config.max_provider_response_bytes)
        except urllib.error.HTTPError as error:
            # Even an error status may include a task ID. Preserve it. A provider
            # failure is not proof that its charge has been refunded.
            if error.code == 400:
                raw = read_limited(error, self.config.max_provider_response_bytes)
            else:
                raise GrsaiError('GRSAI_RESPONSE_UNKNOWN', '供应商响应待核实，请查询原任务') from None
        except (urllib.error.URLError, TimeoutError, OSError):
            raise GrsaiError('GRSAI_RESPONSE_UNKNOWN', '供应商响应待核实，请查询原任务') from None
        try:
            value = json.loads(raw)
            value = value.get('data', value) if isinstance(value, dict) else None
            if not isinstance(value, dict) or not isinstance(value.get('id'), str) or not 1 <= len(value['id']) <= 256:
                raise ValueError
            urls = value.get('results', [])
            if not isinstance(urls, list) or len(urls) > 16:
                raise ValueError
            return {'id': value['id'], 'status': str(value.get('status', 'running')),
                    'results': [{'url': item['url']} for item in urls if isinstance(item, dict) and isinstance(item.get('url'), str)]}
        except (ValueError, TypeError, UnicodeDecodeError):
            raise GrsaiError('GRSAI_RESPONSE_UNKNOWN', '供应商响应待核实，请查询原任务') from None
