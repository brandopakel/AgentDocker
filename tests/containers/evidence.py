"""Failure evidence helpers; importing them starts no daemon or engine."""
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import time


def reject_launch(response, inspect_record, remember_record):
    """Retain the primary launch failure even if no agent was registered."""
    evidence = {"launch_response": response}
    try:
        record = inspect_record()
        if record.get("container"):
            remember_record(record)
    except Exception as error:
        evidence["cleanup_lookup_error"] = str(error)
    raise AssertionError(json.dumps(evidence, sort_keys=True))


def capture_tail(command, output_path, *, limit=2 * 1024 * 1024, timeout=30):
    """Retain a bounded tail, draining noisy output until exit or a fixed deadline."""
    if limit < 1 or timeout <= 0:
        raise ValueError("positive output and time limits required")
    tail = bytearray()
    total = 0
    timed_out = False
    deadline = time.monotonic() + timeout
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               start_new_session=True)
    try:
        with selectors.DefaultSelector() as ready:
            ready.register(process.stdout, selectors.EVENT_READ)
            while ready.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    timed_out = True
                    break
                if not ready.select(remaining):
                    timed_out = True
                    break
                data = os.read(process.stdout.fileno(), 64 * 1024)
                if not data:
                    ready.unregister(process.stdout)
                    break
                total += len(data)
                tail.extend(data)
                del tail[:-limit]
            if not timed_out:
                try:
                    process.wait(timeout=max(0, deadline - time.monotonic()))
                except subprocess.TimeoutExpired:
                    timed_out = True
    finally:
        # A timed-out/failed read can leave an SSH helper holding the pipe.
        # Kill before reaping the leader, while its group identity is reserved.
        if process.returncode is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        process.stdout.close()
    Path(output_path).write_bytes(tail)
    return {"exit": process.returncode, "timed_out": timed_out,
            "bytes_read": total, "bytes_retained": len(tail), "truncated": total > limit}


def retain_container_logs(engine, owned, result_path, *, limit=2 * 1024 * 1024, timeout=30):
    """Place container logs beside CI's result, and preserve secondary errors."""
    records, errors = [], []
    result = Path(result_path)
    try:
        result.parent.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        return records, [str(error)]
    for index, target in enumerate(owned):
        # An ordinal avoids treating a transport-returned identifier as a path.
        output = result.with_suffix(f".container-{index}.log")
        try:
            evidence = capture_tail([engine, "container", "logs", target], output,
                                    limit=limit, timeout=timeout)
            records.append({"container": target, "file": output.name, **evidence})
        except (OSError, subprocess.SubprocessError) as error:
            errors.append(str(error))
    return records, errors
