"""Private Unix endpoint in an owned engine volume; only framed CLI stdio leaves it."""
import json
import os
from pathlib import Path
import queue
import socket
import stat
import sys
import threading
import uuid

LIMIT = 1024 * 1024
SLOTS = threading.BoundedSemaphore(32)
LOCK = threading.Lock()
OUTPUT = threading.Lock()
PENDING = {}
STOP = threading.Event()


def emit(message):
    with OUTPUT:
        sys.stdout.write(json.dumps(message, separators=(',', ':')) + '\n')
        sys.stdout.flush()


def client(stream):
    identity = uuid.uuid4().hex
    replies = queue.Queue(maxsize=1)
    with LOCK:
        PENDING[identity] = replies
    try:
        stream.settimeout(30)
        reader = stream.makefile('rb')
        # The restricted protocol authenticates, executes one operation and closes.
        for _ in range(2):
            raw = reader.readline(LIMIT + 1)
            if not raw or len(raw) > LIMIT or not raw.endswith(b'\n'):
                break
            emit({'id': identity, 'frame': raw.decode('utf-8')})
            response = replies.get(timeout=30)
            if response is None:
                break
            stream.sendall(response.encode('utf-8'))
    except (OSError, ValueError, queue.Empty):
        pass
    finally:
        with LOCK:
            PENDING.pop(identity, None)
        stream.close()
        SLOTS.release()
        emit({'id': identity, 'close': True})


def accept(listener):
    while not STOP.is_set():
        try:
            stream, _ = listener.accept()
        except socket.timeout:
            continue
        if not SLOTS.acquire(blocking=False):
            stream.close()
            continue
        threading.Thread(target=client, args=(stream,), daemon=True).start()


endpoint = Path('/run/agentdocker/endpoint.sock')
if endpoint.exists():
    if not stat.S_ISSOCK(endpoint.lstat().st_mode):
        raise RuntimeError('endpoint was replaced by a non-socket')
    endpoint.unlink()
listener = socket.socket(socket.AF_UNIX)
listener.bind(str(endpoint))
os.chmod(endpoint, 0o600)
listener.listen(32)
listener.settimeout(1)
threading.Thread(target=accept, args=(listener,), daemon=True).start()
emit({'ready': True})
try:
    while True:
        raw = sys.stdin.buffer.readline(8 * LIMIT + 1)
        if not raw:
            break
        if len(raw) > 8 * LIMIT or not raw.endswith(b'\n'):
            break
        message = json.loads(raw)
        frame = message.get('frame')
        if frame is not None and (not isinstance(frame, str) or len(frame.encode()) > LIMIT or not frame.endswith('\n')):
            break
        with LOCK:
            replies = PENDING.get(message.get('id'))
        if replies is not None:
            try:
                replies.put_nowait(frame)
            except queue.Full:
                # A terminal close may race the reply already queued for this peer.
                if frame is not None:
                    raise RuntimeError('duplicate relay response')
finally:
    STOP.set()
    listener.close()
