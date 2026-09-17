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

MAX_BYTES = 1024 * 1024
MAX_FDS = 4096
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


def inspect(pid, proc=Path("/proc")):
    """Classify a stable owned-process socket snapshot, refusing unknowns.

    The open proc directory anchors all reads to one process generation. See
    https://docs.kernel.org/filesystems/proc.html#process-specific-subdirectories
    A dead process's open proc descriptors cannot refer to a reused PID.
    """
    root_fd = os.open(proc / str(pid), os.O_RDONLY | os.O_DIRECTORY)
    try:
        for _ in range(3):
            namespace = os.readlink("ns/net", dir_fd=root_fd)
            before = socket_descriptors(root_fd)
            tcp = read_table(root_fd, "tcp", 9)
            tcp |= read_table(root_fd, "tcp6", 9, optional=True)
            other = read_table(root_fd, "unix", 6)
            for name, index in [("udp", 9), ("udp6", 9), ("raw", 9),
                                ("raw6", 9), ("netlink", 9), ("packet", 8)]:
                other |= read_table(root_fd, name, index, optional=True)
            after = socket_descriptors(root_fd)
            seen = {inode for _, inode in before | after}
            if seen & tcp:
                return {"tcp": True, "socket_count": len(seen)}
            if namespace != os.readlink("ns/net", dir_fd=root_fd):
                raise ValueError("network namespace changed during observation")
            if before == after and seen <= other:
                return {"tcp": False, "socket_count": len(seen)}
        raise ValueError("socket descriptors changed or could not be classified")
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
