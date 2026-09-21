"""Bounded, duplex byte-stream named pipes for the Windows smoke driver.

One reader and one writer may run concurrently. Each uses its own OVERLAPPED,
event and native buffer; a blocked read never serializes a write. This is a
fixture helper, not an alternative to the product's authenticated IPC client.
"""

import ctypes
from ctypes import wintypes
import math
import os
import threading
import time


if os.name == "nt":
    _api = ctypes.WinDLL("kernel32", use_last_error=True)

    class _Overlapped(ctypes.Structure):
        _fields_ = [
            ("Internal", ctypes.c_size_t),
            ("InternalHigh", ctypes.c_size_t),
            ("Offset", wintypes.DWORD),
            ("OffsetHigh", wintypes.DWORD),
            ("hEvent", wintypes.HANDLE),
        ]

    def _signature(name, result, *arguments):
        function = getattr(_api, name)
        function.restype = result
        function.argtypes = arguments
        return function

    _create = _signature("CreateFileW", wintypes.HANDLE, wintypes.LPCWSTR,
                         wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                         wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE)
    _wait_pipe = _signature("WaitNamedPipeW", wintypes.BOOL,
                            wintypes.LPCWSTR, wintypes.DWORD)
    _event = _signature("CreateEventW", wintypes.HANDLE, ctypes.c_void_p,
                        wintypes.BOOL, wintypes.BOOL, wintypes.LPCWSTR)
    _wait = _signature("WaitForSingleObject", wintypes.DWORD,
                       wintypes.HANDLE, wintypes.DWORD)
    _close = _signature("CloseHandle", wintypes.BOOL, wintypes.HANDLE)
    _allocate = _signature("LocalAlloc", ctypes.c_void_p,
                           wintypes.UINT, ctypes.c_size_t)
    _free = _signature("LocalFree", ctypes.c_void_p, ctypes.c_void_p)
    _read = _signature("ReadFile", wintypes.BOOL, wintypes.HANDLE,
                       ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p,
                       ctypes.POINTER(_Overlapped))
    _write = _signature("WriteFile", wintypes.BOOL, wintypes.HANDLE,
                        ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p,
                        ctypes.POINTER(_Overlapped))
    _result = _signature("GetOverlappedResult", wintypes.BOOL,
                         wintypes.HANDLE, ctypes.POINTER(_Overlapped),
                         ctypes.POINTER(wintypes.DWORD), wintypes.BOOL)
    _cancel = _signature("CancelIoEx", wintypes.BOOL, wintypes.HANDLE,
                         ctypes.POINTER(_Overlapped))


_PENDING = 997
_INCOMPLETE = 996
_ABORTED = 995
_PIPE_BUSY = 231
_NOT_FOUND = 1168
_WAIT_TIMEOUT = 258
_EOF = (109, 232, 233)  # broken pipe, no data, disconnected pipe
_CHUNK = 64 * 1024


def _deadline(seconds):
    if not math.isfinite(seconds) or seconds <= 0:
        raise ValueError("pipe timeout must be finite and positive")
    return time.monotonic() + seconds


def _milliseconds(deadline):
    return min(0xFFFFFFFE, max(0, math.ceil((deadline - time.monotonic()) * 1000)))


class _Operation:
    def __init__(self, size, data):
        self.reading = data is None
        self.done = False
        self.abandoned = False
        self.error = 0
        self.value = None
        # Native storage is deliberately freed only after kernel completion.
        # If cancellation itself fails to complete, Python object destruction
        # must not free an OVERLAPPED or buffer still referenced by the kernel.
        self.allocation = _allocate(0x40, ctypes.sizeof(_Overlapped) + max(1, size))
        if not self.allocation:
            raise ctypes.WinError(ctypes.get_last_error())
        self.overlapped = _Overlapped.from_address(self.allocation)
        self.buffer = self.allocation + ctypes.sizeof(_Overlapped)
        self.overlapped.hEvent = _event(None, True, False, None)
        if not self.overlapped.hEvent:
            error = ctypes.get_last_error()
            _free(self.allocation)
            raise ctypes.WinError(error)
        if data is not None:
            ctypes.memmove(self.buffer, data, size)

    def dispose(self):
        _close(self.overlapped.hEvent)
        _free(self.allocation)


class WindowsSmokePipe:
    """File-like readline/write/flush/close with bounded native I/O.

    A timeout poisons the connection: a partial JSON frame must never be
    retried on it. close() cancels both directions and waits at most two
    seconds. If Windows has not completed cancellation, close raises and
    retains the native allocations/handle until completion or process exit.
    The smoke must treat that exception as a cleanup failure.
    """

    def __init__(self, path, timeout=10, read_timeout=30, write_timeout=5,
                 max_line=1024 * 1024):
        if os.name != "nt":
            raise OSError("WindowsSmokePipe requires Windows")
        path = os.fspath(path)
        if not isinstance(path, str) or not path.startswith("\\\\.\\pipe\\") or "\0" in path:
            raise ValueError("a local Windows named-pipe path is required")
        if not 1 <= max_line <= 4 * 1024 * 1024:
            raise ValueError("pipe line bound must be between 1 byte and 4 MiB")
        _deadline(read_timeout)
        _deadline(write_timeout)
        self._condition = threading.Condition()
        self._reader = threading.Lock()
        self._writer = threading.Lock()
        self._pending = set()
        self._closing = False
        self._buffer = bytearray()
        self._eof = False
        self._read_timeout = read_timeout
        self._write_timeout = write_timeout
        self._max_line = max_line
        self._cancel_error = None
        deadline = _deadline(timeout)
        while True:
            # GENERIC_READ | GENERIC_WRITE; OPEN_EXISTING; FILE_FLAG_OVERLAPPED.
            handle = _create(path, 0xC0000000, 0, None, 3, 0x40000000, None)
            if handle != ctypes.c_void_p(-1).value:
                self._handle = handle
                break
            error = ctypes.get_last_error()
            if error != _PIPE_BUSY:
                raise ctypes.WinError(error)
            remaining = _milliseconds(deadline)
            if not remaining:
                raise TimeoutError("named pipe stayed busy")
            if not _wait_pipe(path, remaining):
                error = ctypes.get_last_error()
                if error == 121:  # ERROR_SEM_TIMEOUT
                    raise TimeoutError("named pipe stayed busy") from None
                if error != _PIPE_BUSY:
                    raise ctypes.WinError(error)

    def _close_handle_locked(self):
        if self._closing and not self._pending and self._handle is not None:
            if not _close(self._handle):
                raise ctypes.WinError(ctypes.get_last_error())
            self._handle = None

    def _complete_locked(self, operation, error=None):
        if operation.done:
            return True
        count = wintypes.DWORD()
        if error is None:
            succeeded = _result(self._handle, ctypes.byref(operation.overlapped),
                                ctypes.byref(count), False)
            error = 0 if succeeded else ctypes.get_last_error()
            if error == _INCOMPLETE:
                return False
        operation.error = error
        operation.value = (ctypes.string_at(operation.buffer, count.value)
                           if operation.reading and not error else count.value)
        operation.done = True
        self._pending.remove(operation)
        operation.dispose()
        self._close_handle_locked()
        self._condition.notify_all()
        return True

    def _cancel_locked(self):
        self._closing = True
        for operation in self._pending:
            if not _cancel(self._handle, ctypes.byref(operation.overlapped)):
                error = ctypes.get_last_error()
                if error != _NOT_FOUND:  # completion can race cancellation
                    self._cancel_error = error
        self._close_handle_locked()

    def _io(self, size, data, deadline):
        with self._condition:
            if self._closing:
                if data is None:
                    return b""
                raise ValueError("pipe is closed")
            operation = _Operation(size, data)
            self._pending.add(operation)
            call = _read if data is None else _write
            succeeded = call(self._handle, operation.buffer, size, None,
                             ctypes.byref(operation.overlapped))
            error = 0 if succeeded else ctypes.get_last_error()
            if error != _PENDING:
                self._complete_locked(operation, None if succeeded else error)
        try:
            if not operation.done:
                waited = _wait(operation.overlapped.hEvent, _milliseconds(deadline))
                if waited == _WAIT_TIMEOUT:
                    raise TimeoutError("named-pipe I/O did not finish")
                if waited != 0:
                    raise ctypes.WinError(ctypes.get_last_error())
                with self._condition:
                    if not self._complete_locked(operation):
                        raise OSError("pipe signalled before I/O completed")
        except BaseException:
            with self._condition:
                self._cancel_locked()
            # CancelIoEx merely requests cancellation. Retain all storage until
            # GetOverlappedResult confirms it; never use its blocking wait form.
            if not operation.done:
                _wait(operation.overlapped.hEvent, 1000)
                with self._condition:
                    if not self._complete_locked(operation):
                        operation.abandoned = True
                        self._condition.notify_all()
            raise
        if operation.error:
            if data is None and (operation.error in _EOF or
                                 (operation.error == _ABORTED and self._closing)):
                return b""
            raise ctypes.WinError(operation.error)
        return operation.value

    def readline(self, timeout=None):
        if not self._reader.acquire(blocking=False):
            raise RuntimeError("only one pipe reader is supported")
        try:
            deadline = _deadline(self._read_timeout if timeout is None else timeout)
            while True:
                end = self._buffer.find(b"\n")
                if end >= 0:
                    result = bytes(self._buffer[:end + 1])
                    del self._buffer[:end + 1]
                    return result
                if len(self._buffer) >= self._max_line:
                    self.close()
                    raise ValueError("named-pipe line exceeds its bound")
                if self._eof:
                    result = bytes(self._buffer)
                    self._buffer.clear()
                    return result
                chunk = self._io(min(_CHUNK, self._max_line - len(self._buffer)),
                                 None, deadline)
                if chunk:
                    self._buffer.extend(chunk)
                else:
                    self._eof = True
        finally:
            self._reader.release()

    def write(self, data, timeout=None):
        if not self._writer.acquire(blocking=False):
            raise RuntimeError("only one pipe writer is supported")
        try:
            view = memoryview(data)
            if view.nbytes > self._max_line:
                raise ValueError("named-pipe write exceeds its bound")
            data = view.tobytes()  # do not borrow a buffer the caller can mutate
            if not data:
                self.flush()
                return 0
            deadline = _deadline(self._write_timeout if timeout is None else timeout)
            return self._io(len(data), data, deadline)
        finally:
            self._writer.release()

    def flush(self):
        # write() waits for its own operation. FlushFileBuffers on a named pipe
        # would instead wait for the peer to read everything, without a deadline.
        with self._condition:
            if self._closing:
                raise ValueError("pipe is closed")

    def close(self):
        deadline = _deadline(2)
        with self._condition:
            self._cancel_locked()
            while self._pending:
                # Only the original waiter may dispose an operation's event.
                # A timed-out waiter explicitly transfers that duty to close.
                for operation in list(self._pending):
                    if operation.abandoned:
                        self._complete_locked(operation)
                if not self._pending:
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    detail = f" (CancelIoEx error {self._cancel_error})" if self._cancel_error else ""
                    raise TimeoutError("pipe cancellation did not finish; native storage retained" + detail)
                self._condition.wait(min(remaining, 0.05))
            self._close_handle_locked()
