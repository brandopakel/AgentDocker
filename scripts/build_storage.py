#!/usr/bin/env python3
"""Read-only disk budget check before starting local build campaigns."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

GIB = 1024 ** 3
ROOT = Path(__file__).resolve().parents[1]


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("storage limits must be positive GiB")
    return number


def allocated(path):
    """Measure allocated blocks without following links inside a cache."""
    if shutil.which("du"):
        return int(subprocess.check_output(["du", "-sk", str(path)], text=True).split()[0]) * 1024
    total, seen = 0, set()
    def walk_error(error):
        raise error

    for directory, directories, files in os.walk(path, followlinks=False, onerror=walk_error):
        # Include the root and directory entries, including untraversed symlinks.
        # A visited child directory appears again as a root: count its inode once.
        for entry in [Path(directory), *(Path(directory) / name for name in directories + files)]:
            metadata = entry.lstat()
            identity = (metadata.st_dev, metadata.st_ino)
            if identity not in seen:
                seen.add(identity)
                total += getattr(metadata, "st_blocks", (metadata.st_size + 511) // 512) * 512
    return total


def existing_parent(path):
    while not path.exists():
        if path == path.parent:
            raise ValueError(f"cannot locate filesystem for {path}")
        path = path.parent
    return path


def inspect(args):
    # Refuse known low space before even invoking Cargo metadata.
    if shutil.disk_usage(ROOT).free < args.minimum_free_gib * GIB:
        raise ValueError(f"less than {args.minimum_free_gib} GiB free; clean inactive build output before compiling")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=ROOT, text=True))
    current = Path(metadata["target_directory"]).resolve()
    worktrees = subprocess.check_output(["git", "worktree", "list", "--porcelain", "-z"], cwd=ROOT)
    roots = [Path(os.fsdecode(field[9:])) for field in worktrees.split(b"\0") if field.startswith(b"worktree ")]
    candidates = {current, *(root / name for root in roots for name in ["target", "fuzz/target"])}
    caches, devices = {}, set()
    for candidate in sorted(candidates):
        path = candidate.resolve()
        if path in caches:
            continue
        parent = existing_parent(path)
        device = parent.stat().st_dev
        if device not in devices:
            if shutil.disk_usage(parent).free < args.minimum_free_gib * GIB:
                raise ValueError(f"less than {args.minimum_free_gib} GiB free on the filesystem containing {path}")
            devices.add(device)
        if path.is_dir():
            caches[path] = allocated(path)
    current_bytes = caches.get(current, 0)
    total = sum(caches.values())
    if current_bytes > args.max_current_gib * GIB:
        raise ValueError(f"current Cargo cache uses {current_bytes / GIB:.1f} GiB (limit {args.max_current_gib}); preserve reports and clean it when idle")
    if total > args.max_total_gib * GIB:
        raise ValueError(f"registered worktree caches use {total / GIB:.1f} GiB (limit {args.max_total_gib}); preserve reports and clean inactive caches")
    return {"current_target": str(current), "current_cache_gib": round(current_bytes / GIB, 2),
            "registered_cache_gib": round(total / GIB, 2), "minimum_free_gib": args.minimum_free_gib,
            "max_current_gib": args.max_current_gib, "max_total_gib": args.max_total_gib}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--minimum-free-gib", type=positive,
                        default=os.environ.get("AGENTDOCKER_BUILD_MIN_FREE_GIB", "5" if os.environ.get("CI") == "true" else "20"))
    parser.add_argument("--max-current-gib", type=positive, default=os.environ.get("AGENTDOCKER_BUILD_MAX_CURRENT_GIB", "12"))
    parser.add_argument("--max-total-gib", type=positive, default=os.environ.get("AGENTDOCKER_BUILD_MAX_TOTAL_GIB", "40"))
    args = parser.parse_args()
    try:
        print(json.dumps(inspect(args), sort_keys=True))
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"build storage: {error}; no build started", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
