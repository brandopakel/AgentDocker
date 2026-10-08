#!/usr/bin/env python3
"""Observe synthetic ConPTY Ctrl-] bytes and advertised Win32 keyboard transport.

Uses only private Python consoles and invented input, not an AgentDocker
process or account. This diagnoses fixture transport, not product acceptance.
"""
import argparse
import ctypes
import datetime
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import threading
import time


def child(vt_input):
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.GetStdHandle.argtypes = [wintypes.DWORD]
    kernel.GetStdHandle.restype = wintypes.HANDLE
    kernel.GetConsoleMode.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel.SetConsoleMode.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.ReadConsoleW.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD,
                                   ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
    handle = kernel.GetStdHandle(-10)
    mode = wintypes.DWORD()
    assert kernel.GetConsoleMode(handle, ctypes.byref(mode)), ctypes.get_last_error()
    saved = mode.value
    active = saved & ~(1 | 2 | 4 | 0x200)
    if vt_input:
        active |= 0x200
    assert kernel.SetConsoleMode(handle, active), ctypes.get_last_error()
    try:
        print('#READY#' + json.dumps({'saved': saved, 'active': active}), flush=True)
        while True:
            buffer = (ctypes.c_uint16 * 512)()
            read = wintypes.DWORD()
            assert kernel.ReadConsoleW(handle, buffer, 512, ctypes.byref(read), None), ctypes.get_last_error()
            values = list(buffer[:read.value])
            print('#INPUT#' + json.dumps(values), flush=True)
            if ord('!') in values:
                break
    finally:
        assert kernel.SetConsoleMode(handle, saved), ctypes.get_last_error()
        assert kernel.GetConsoleMode(handle, ctypes.byref(mode)) and mode.value == saved
        print('#RESTORED#', flush=True)


def observe(vt_input):
    import psutil
    from winpty import PtyProcess
    from winpty.enums import Backend
    terminal = PtyProcess.spawn([sys.executable, str(Path(__file__).resolve()), '--child'] +
                               (['--vt-input'] if vt_input else []), dimensions=(40, 160), backend=Backend.ConPTY)
    process = psutil.Process(terminal.pid)
    report = {'vt_input': vt_input, 'pid': process.pid, 'birth': process.create_time(), 'reader_errors': []}
    output = []; closing = threading.Event()
    def read():
        try:
            while terminal.isalive():
                output.append(terminal.read(65536))
                assert sum(map(len, output)) < 1024 * 1024
        except EOFError:
            pass
        except Exception as error:
            if not closing.is_set():
                report['reader_errors'].append(str(error))
    reader = threading.Thread(target=read, daemon=True); reader.start()
    def text():
        return re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', ''.join(output))
    def values():
        return [unit for row in re.findall(r'#INPUT#(\[[\d, ]*\])', text()) for unit in json.loads(row)]
    def wait(predicate, seconds=10):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(.05)
        raise TimeoutError('bounded console transport observation timed out')
    try:
        wait(lambda: '#READY#' in text())
        report['win32_input_advertised'] = '\x1b[?9001h' in ''.join(output)
        report['mode'] = json.loads(re.search(r'#READY#(\{[^\r\n]*\})', text())[1])
        terminal.write('a'); wait(lambda: 97 in values())
        terminal.write('\x1d'); time.sleep(.3)
        terminal.write('b'); wait(lambda: 98 in values())
        first = values(); report['raw_control_units'] = first[first.index(97)+1:first.index(98)]
        # Microsoft's advertised win32-input-mode uses Vk;Sc;Uc;Kd;Cs;Rc.
        # VK_OEM_6 / scan 27, U+001D, left Ctrl, key-down then key-up.
        terminal.write('\x1b[221;27;29;1;8;1_\x1b[221;27;29;0;8;1_')
        time.sleep(.3); terminal.write('c'); wait(lambda: 99 in values())
        second = values(); report['win32_control_units'] = second[second.index(98)+1:second.index(99)]
        terminal.write('!'); wait(lambda: not terminal.isalive())
        wait(lambda: '#RESTORED#' in text(), 2)
        assert not process.is_running() and not report['reader_errors']
        report['mode_restored'] = True; report['native_process_retired'] = True
        # Retain both transport outcomes; only the advertised transport is
        # required here. The extracted product trial remains a separate gate.
        if report['win32_input_advertised']:
            assert report['win32_control_units'] == [29], report['win32_control_units']
        else:
            assert report['raw_control_units'] == [29], report['raw_control_units']
        report['result'] = 'observed_expected_advertised_transport'
    except BaseException as error:
        report.update(result='failed', error=f'{type(error).__name__}: {error}')
    finally:
        if process.is_running():
            report['forced_cleanup'] = True; process.kill(); process.wait(timeout=10)
            report['result'] = 'failed'
        closing.set(); terminal.pty.cancel_io()
        try:
            terminal.fileobj.shutdown(socket.SHUT_RDWR)
        except OSError as error:
            if error.winerror not in (10038, 10057, 10058):
                raise
        terminal.fileobj.close(); terminal._server.close(); terminal._thread.join(timeout=2)
        reader.join(timeout=2)
        assert not reader.is_alive() and not terminal._thread.is_alive()
        terminal.closed = True; terminal.pty = None
        report['output'] = ''.join(output)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--child', action='store_true')
    parser.add_argument('--vt-input', action='store_true')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('native Windows is required')
    if args.child:
        child(args.vt_input); return 0
    assert args.output is not None and not args.output.exists()
    report = {'scope': __doc__, 'at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
              'cases': [observe(False), observe(True)]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')
    print(json.dumps(report))
    return any(v['result'] != 'observed_expected_advertised_transport' for v in report['cases'])


if __name__ == '__main__':
    raise SystemExit(main())
