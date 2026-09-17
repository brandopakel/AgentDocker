#!/usr/bin/env python3
"""Bounded Linux socket observation for owned graphical-test processes.

Run in a subprocess with an outer timeout. Socket inodes must be classified;
missing evidence is never a successful no-TCP observation. Like lsof polling,
this cannot see a socket that opens and closes between samples.
"""
import json
import os
from pathlib import Path
import re
import sys
import time

MAX_BYTES = 1024 * 1024
MAX_FDS = 4096
MAX_SNAPSHOTS = 8
SOCKET = re.compile(r"socket:\[(\d+)\]")


def read_table(root_fd, name, index, optional=False):
    """Read one kernel table with a byte cap, retaining only socket inodes."""
    try:
        fd = os.open("net/" + name, os.O_RDONLY, dir_fd=root_fd)
    except FileNotFoundError:
        if optional:
            return set()
        raise
    with os.fdopen(fd, "rb") as stream:
        data = stream.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError("socket table exceeds observation budget")
    rows = data.splitlines()
    if not rows or b"inode" not in rows[0].lower():
        raise ValueError("unrecognized socket table header")
    inodes = set()
    for row in rows[1:]:
        fields = row.split()
        if len(fields) <= index or not fields[index].isascii() or not fields[index].isdigit():
            raise ValueError("unrecognized socket table row")
        inodes.add(fields[index].decode("ascii"))
    return inodes


def socket_descriptors(root_fd):
    """Capture descriptor numbers and socket inodes without statting mounts."""
    fd = os.open("fd", os.O_RDONLY | os.O_DIRECTORY, dir_fd=root_fd)
    try:
        result = set()
        with os.scandir(fd) as entries:
            for count, entry in enumerate(entries, 1):
                if count > MAX_FDS:
                    raise ValueError("descriptor count exceeds observation budget")
                try:
                    target = os.readlink(entry.name, dir_fd=fd)
                except FileNotFoundError:
                    # A closing descriptor is checked by the second snapshot.
                    continue
                match = SOCKET.fullmatch(target)
                if match:
                    result.add((entry.name, match[1]))
        return result
    finally:
        os.close(fd)


def socket_tables(root_fd):
    tcp = read_table(root_fd, "tcp", 9)
    tcp |= read_table(root_fd, "tcp6", 9, optional=True)
    other = read_table(root_fd, "unix", 6)
    for name, index in [("udp", 9), ("udp6", 9), ("raw", 9),
                        ("raw6", 9), ("netlink", 9), ("packet", 8)]:
        other |= read_table(root_fd, name, index, optional=True)
    return tcp, other


def inspect(pid, proc=Path("/proc")):
    """Bracket observed descriptors with kernel tables, refusing unknowns.

    The open proc directory anchors all reads to one process generation. See
    https://docs.kernel.org/filesystems/proc.html#process-specific-subdirectories
    A dead process's open proc descriptors cannot refer to a reused PID.
    Descriptor churn is expected during RPCs: classify every inode we observed,
    rather than requiring the process to keep an identical descriptor set.
    """
    root_fd = os.open(proc / str(pid), os.O_RDONLY | os.O_DIRECTORY)
    try:
        unknown_count = 0
        for attempt in range(MAX_SNAPSHOTS):
            namespace = os.readlink("ns/net", dir_fd=root_fd)
            before_tcp, before_other = socket_tables(root_fd)
            seen = {inode for _, inode in socket_descriptors(root_fd)}
            after_tcp, after_other = socket_tables(root_fd)
            if seen & (before_tcp | after_tcp):
                return {"tcp": True, "socket_count": len(seen)}
            if namespace != os.readlink("ns/net", dir_fd=root_fd):
                raise ValueError("network namespace changed during observation")
            unknown_count = len(seen - (before_other | after_other))
            if not unknown_count:
                return {"tcp": False, "socket_count": len(seen)}
            if attempt + 1 < MAX_SNAPSHOTS:
                time.sleep(0.001)
        raise ValueError(f"socket inodes could not be classified: {unknown_count} unknown after {MAX_SNAPSHOTS} samples")
    finally:
        os.close(root_fd)


if __name__ == "__main__":
    try:
        result = inspect(int(sys.argv[1]))
    except (OSError, ValueError, IndexError, UnicodeError) as error:
        # Kernel paths, addresses and unrelated process data are not output.
        print(json.dumps({"error": type(error).__name__, "detail": str(error) if isinstance(error, ValueError) else "kernel observation unavailable"}), file=sys.stderr)
        sys.exit(2)
    print(json.dumps(result))
