from __future__ import annotations

import urllib.request


class NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Provider and payment requests must never carry state across redirects."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_NO_REDIRECT_OPENER = urllib.request.build_opener(NoRedirectHandler)


def open_no_redirect(request: urllib.request.Request, timeout: float):
    return _NO_REDIRECT_OPENER.open(request, timeout=timeout)


def read_limited(response, limit: int) -> bytes:
    chunks: list[bytes] = []
    total = 0
    while True:
        chunk = response.read(min(1024 * 1024, limit - total + 1))
        if not chunk:
            break
        chunks.append(chunk)
        total += len(chunk)
        if total > limit:
            raise ValueError("response exceeds configured limit")
    return b"".join(chunks)
