"""One local sidecar lock shared by the assistant worker and manual settlement."""
from contextlib import contextmanager

from .errors import ApiError

try:
    import fcntl
except ImportError:
    fcntl = None


@contextmanager
def execution_lock(path, *, required=True, create=True):
    if fcntl is None:
        if required:
            raise ApiError(503, 'ASSISTANT_EXECUTOR_LOCK_UNAVAILABLE', '本机无法确认执行者已停止')
        yield None  # Existing worker behavior; local writes remain unavailable.
        return
    # A separate file avoids colliding with SQLite's own DB-file locks on macOS.
    path = path.resolve(strict=True)
    lock = path.with_name(path.name + '.assistant-execution.lock')
    if not create and not lock.exists():
        yield True  # Read-only preview; apply always locks and rereads the row.
        return
    with lock.open('a+b' if create else 'rb') as handle:
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            yield False
            return
        try:
            yield True
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
