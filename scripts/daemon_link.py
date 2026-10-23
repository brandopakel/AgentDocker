#!/usr/bin/env python3
"""Give a directory's agentdocker executable its daemon name.

Invoked as agentd, the agentdocker executable is the daemon. Packages ship
agentd beside it as a hard link; this makes that link in a build directory,
replacing a separate agentd an earlier build left there. Where the filesystem
cannot link, a copy of the same bytes serves as well.
"""
import os
from pathlib import Path
import shutil
import sys


def link(directory):
    """Make agentd (agentd.exe beside agentdocker.exe) the same program as
    agentdocker in directory; return "link" or "copy" for what it is."""
    directory = Path(directory)
    suffix = ".exe" if (directory / "agentdocker.exe").is_file() else ""
    cli, daemon = directory / f"agentdocker{suffix}", directory / f"agentd{suffix}"
    if not cli.is_file() or cli.is_symlink():
        raise FileNotFoundError(f"no agentdocker executable in {directory}")
    if daemon.is_file() and not daemon.is_symlink() and os.path.samefile(cli, daemon):
        return "link"
    staged = directory / f".agentd{suffix}.staged"
    staged.unlink(missing_ok=True)
    try:
        os.link(cli, staged)
        kind = "link"
    except OSError:
        shutil.copy2(cli, staged)
        kind = "copy"
    os.replace(staged, daemon)
    return kind


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: daemon_link.py DIRECTORY")
    print(link(sys.argv[1]))
