#!/usr/bin/env python3
"""Disk budget for local build campaigns.

Without flags: a read-only preflight that refuses a build when the caches
are over budget. With `--prune`: remove the Cargo caches of worktrees that
are inactive — no live AgentDocker lease on them, a branch already merged
into origin/main (or gone), and no build running in them. `--prune
--dry-run` only says what would go, with sizes; verify.sh prints that at the
end of every run and never prunes on its own.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

GIB = 1024 ** 3
ROOT = Path(__file__).resolve().parents[1]
# Where a worktree keeps its build output, relative to its root.
CACHE_NAMES = ("target", "fuzz/target")
# Processes that are a build: the command's basename, or its prefix.
BUILD_TOOLS = ("cargo", "rustc", "rustdoc")


class BudgetError(Exception):
    """A storage refusal whose message contains no private paths."""


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("storage limits must be positive GiB")
    return number


def allocated(path):
    """Measure allocated blocks without following links inside a cache."""
    if shutil.which("du"):
        return int(subprocess.check_output(["du", "-sk", str(path)], text=True, stderr=subprocess.PIPE).split()[0]) * 1024
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
            raise BudgetError("cannot locate a cache filesystem")
        path = path.parent
    return path


def parse_worktrees(porcelain):
    """Records of `git worktree list --porcelain -z`: path, head, branch (None when detached)."""
    records, record = [], None
    for field in porcelain.split(b"\0"):
        if field.startswith(b"worktree "):
            record = {"path": Path(os.fsdecode(field[9:])), "head": None, "branch": None}
            records.append(record)
        elif record is None:
            continue
        elif field.startswith(b"HEAD "):
            record["head"] = os.fsdecode(field[5:])
        elif field.startswith(b"branch refs/heads/"):
            record["branch"] = os.fsdecode(field[18:])
    return records


def measure(minimum_free_gib=None):
    """The current target directory, the registered worktrees and every cache's size.

    With a minimum, a filesystem below it is refused before anything is measured.
    """
    if minimum_free_gib is not None and shutil.disk_usage(ROOT).free < minimum_free_gib * GIB:
        raise BudgetError(f"less than {minimum_free_gib} GiB free; clean inactive build output before compiling")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=ROOT, text=True, stderr=subprocess.PIPE))
    current = Path(metadata["target_directory"]).resolve()
    records = parse_worktrees(subprocess.check_output(["git", "worktree", "list", "--porcelain", "-z"], cwd=ROOT, stderr=subprocess.PIPE))
    candidates = {current, *(record["path"] / name for record in records for name in CACHE_NAMES)}
    caches, devices = {}, set()
    for candidate in sorted(candidates):
        path = candidate.resolve()
        if path in caches:
            continue
        parent = existing_parent(path)
        device = parent.stat().st_dev
        if device not in devices:
            if minimum_free_gib is not None and shutil.disk_usage(parent).free < minimum_free_gib * GIB:
                raise BudgetError(f"a cache filesystem has less than {minimum_free_gib} GiB free")
            devices.add(device)
        if path.is_dir():
            caches[path] = allocated(path)
    return current, records, caches


def inspect(args):
    # Refuse known low space before even invoking Cargo metadata.
    current, _, caches = measure(args.minimum_free_gib)
    current_bytes = caches.get(current, 0)
    total = sum(caches.values())
    if current_bytes > args.max_current_gib * GIB:
        raise BudgetError(f"current Cargo cache uses {current_bytes / GIB:.1f} GiB (limit {args.max_current_gib}); preserve reports and clean it when idle")
    if total > args.max_total_gib * GIB:
        raise BudgetError(f"registered worktree caches use {total / GIB:.1f} GiB (limit {args.max_total_gib}); preserve reports and clean inactive caches")
    return {"current_cache_gib": round(current_bytes / GIB, 2),
            "registered_cache_gib": round(total / GIB, 2), "minimum_free_gib": args.minimum_free_gib,
            "max_current_gib": args.max_current_gib, "max_total_gib": args.max_total_gib}


# --- pruning -----------------------------------------------------------------

def parse_lease_resources(table):
    """The RESOURCE column of `agentdocker leases`, spaces in a resource included."""
    lines = table.splitlines()
    if not lines:
        return set()
    header = lines[0]
    start, end = header.find("RESOURCE"), header.find("HOLDER")
    resources = set()
    for line in lines[1:]:
        if not line.strip():
            continue
        if start >= 0 and end > start:
            resources.add(line[start:end].strip())
        else:
            fields = line.split()
            if len(fields) >= 2:
                resources.add(fields[1])
    return resources


def lease_resources():
    """Every live lease's resource, or None when no daemon answers.

    The machine's own daemon is asked, as verify.sh asks it: a session's
    private or test daemon knows nothing of the worktrees other sessions hold.
    No daemon is ever started for this.
    """
    env = {key: value for key, value in os.environ.items()
           if key not in ("AGENTDOCKER_HOME", "AGENTDOCKER_SOCKET", "AGENTDOCKER_AGENT_ID")}
    env["AGENTDOCKER_NO_AUTOSTART"] = "1"
    try:
        return parse_lease_resources(subprocess.check_output(
            ["agentdocker", "leases"], text=True, stderr=subprocess.PIPE, env=env, timeout=60))
    except (OSError, subprocess.SubprocessError):
        return None


def is_build_tool(command):
    name = os.path.basename(command.split(None, 1)[0]) if command.strip() else ""
    return any(name == tool or name.startswith(tool + "-") for tool in BUILD_TOOLS)


def process_table():
    """Running processes as (pid, command line, cwd); cwd is looked up for build tools only."""
    try:
        listing = subprocess.check_output(["ps", "-e", "-ww", "-o", "pid=,command="], text=True, stderr=subprocess.PIPE)
    except (OSError, subprocess.SubprocessError):
        return None
    processes = []
    for line in listing.splitlines():
        parts = line.strip().split(None, 1)
        if not parts or not parts[0].isdigit():
            continue
        processes.append([int(parts[0]), parts[1] if len(parts) > 1 else "", None])
    builders = [entry for entry in processes if is_build_tool(entry[1]) and entry[0] != os.getpid()]
    if builders:
        cwds = working_directories([entry[0] for entry in builders])
        for entry in builders:
            entry[2] = cwds.get(entry[0])
    return [tuple(entry) for entry in processes]


def working_directories(pids):
    """pid -> cwd for the pids whose cwd can be read: /proc on Linux, lsof elsewhere."""
    found = {}
    if Path("/proc").is_dir():
        for pid in pids:
            try:
                found[pid] = Path(os.readlink(f"/proc/{pid}/cwd"))
            except OSError:
                continue
        return found
    if shutil.which("lsof"):
        try:
            output = subprocess.check_output(
                ["lsof", "-a", "-d", "cwd", "-Fn", "-p", ",".join(str(pid) for pid in pids)],
                text=True, stderr=subprocess.DEVNULL, timeout=60)
        except (OSError, subprocess.SubprocessError) as error:
            output = getattr(error, "output", None) or ""
        pid = None
        for line in output.splitlines():
            if line.startswith("p"):
                pid = int(line[1:])
            elif line.startswith("n") and pid is not None:
                found[pid] = Path(line[1:])
    return found


def within(path, base):
    """path is base or under it."""
    try:
        Path(path).relative_to(base)
    except ValueError:
        return False
    return True


def mentions(command, path):
    """The command line names the path or something under it, never a longer sibling."""
    return re.search(re.escape(str(path)) + r"(?=$|[/\s\"'=:,;])", command) is not None


def build_lock_held(cache):
    """Cargo holds `<profile>/.cargo-lock` while it builds there."""
    try:
        import fcntl
    except ImportError:
        return False
    for lock in [*cache.glob("*/.cargo-lock"), *cache.glob("*/*/.cargo-lock")]:
        try:
            with open(lock, "rb") as handle:
                try:
                    fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except OSError:
                    return True
                fcntl.flock(handle, fcntl.LOCK_UN)
        except OSError:
            continue
    return False


class Branches:
    """What git says about a worktree's branch against origin/main."""

    def __init__(self, root=ROOT):
        self.root = root
        self.main_tip = self.rev_parse("origin/main")

    def rev_parse(self, ref):
        try:
            return subprocess.check_output(["git", "rev-parse", "--verify", "--quiet", ref + "^{commit}"],
                                           cwd=self.root, text=True, stderr=subprocess.DEVNULL).strip() or None
        except subprocess.CalledProcessError:
            return None

    def state(self, record):
        """merged, current (at origin/main's tip), unmerged, absent or unknown."""
        if self.main_tip is None:
            return "unknown"
        if record["branch"] is not None:
            tip = self.rev_parse("refs/heads/" + record["branch"])
            if tip is None:
                return "absent"
        else:
            tip = record["head"]
        if tip is None:
            return "unknown"
        if tip == self.main_tip:
            return "current"
        result = subprocess.run(["git", "merge-base", "--is-ancestor", tip, self.main_tip],
                                cwd=self.root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return "merged" if result.returncode == 0 else "unmerged"


def plan_prune(records, caches, *, current_root, current_target, leases, processes, branch_state, lock_held):
    """Decide each worktree's caches: (prunable, kept), both lists of
    (worktree path, [(cache path, bytes)], reason).

    `leases` is the set of live lease resources, or None when no daemon
    answers — then every worktree may be active and nothing is prunable.
    `processes` is (pid, command line, cwd or None) per running process, or
    None when they cannot be listed. `branch_state(record)` and
    `lock_held(cache)` are asked only once the cheaper checks pass.
    """
    prunable, kept = [], []
    main_root = records[0]["path"].resolve() if records else None
    for record in records:
        root = record["path"].resolve()
        owned = [(cache, caches.get(cache, 0)) for cache in (root / name for name in CACHE_NAMES)
                 if cache in caches or cache.is_symlink()]
        if not owned:
            continue
        label = record["branch"] or (record["head"] or "detached")[:12]
        reason = None
        if root == current_root or any(cache == current_target for cache, _ in owned):
            reason = "the current worktree"
        elif root == main_root:
            reason = "the project's main checkout"
        elif leases is None:
            reason = "no daemon answers; every worktree may be active"
        elif processes is None:
            reason = "processes cannot be listed; every worktree may be busy"
        else:
            held = sorted(resource for resource in leases
                          if (resource.startswith("path:") and (within(resource[5:], root) or within(root, resource[5:])))
                          or (record["branch"] is not None and resource == "branch:" + record["branch"]))
            if held:
                reason = f"lease {held[0]} is live"
        if reason is None:
            for pid, command, cwd in processes:
                if mentions(command, root) or (cwd is not None and within(cwd, root)):
                    name = os.path.basename(command.split(None, 1)[0]) if command.strip() else "?"
                    reason = f"process {pid} ({name}) runs in it"
                    break
        if reason is None:
            state = branch_state(record)
            if state == "unmerged":
                reason = f"branch {label} is not merged into origin/main"
            elif state == "current":
                reason = f"{label} is at origin/main's tip: current, not a leftover"
            elif state == "unknown":
                reason = "origin/main is unknown here; nothing counts as merged"
        if reason is None:
            links = [cache for cache, _ in owned if cache.is_symlink()]
            if links:
                reason = f"{links[0].name} is a symbolic link; its target may be shared"
            elif any(lock_held(cache) for cache, _ in owned):
                reason = "a Cargo build holds its lock"
        if reason is not None:
            kept.append((root, owned, reason))
            continue
        why = ({"merged": f"branch {label} is merged into origin/main",
                "absent": f"branch {label} no longer exists"}[state] + "; no lease; no build running")
        prunable.append((root, owned, why))
    return prunable, kept


def describe(entries, verb):
    lines = []
    for root, owned, reason in entries:
        size = sum(size for _, size in owned)
        names = ", ".join(str(cache.relative_to(root)) for cache, _ in owned)
        lines.append(f"  {verb} {size / GIB:6.2f} GiB  {root}  ({names}): {reason}")
    return lines


def prune(args):
    current, records, caches = measure()
    total = sum(caches.values())
    print(f"build storage: registered worktree caches use {total / GIB:.1f} GiB "
          f"(current {caches.get(current, 0) / GIB:.1f} GiB; limit {args.max_total_gib})")
    leases = lease_resources()
    processes = process_table()
    branches = Branches()
    prunable, kept = plan_prune(records, caches, current_root=ROOT.resolve(), current_target=current,
                                leases=leases, processes=processes, branch_state=branches.state, lock_held=build_lock_held)
    if leases is None:
        print("no daemon answers: every worktree may be active; nothing is prunable")
    if args.dry_run:
        print("prunable (dry run; nothing removed):" if prunable else "prunable: nothing")
        print("\n".join(describe(prunable, "would remove")))
    else:
        print("pruning:" if prunable else "pruning: nothing")
        failed = 0
        for root, owned, reason in prunable:
            for cache, size in owned:
                try:
                    shutil.rmtree(cache)
                except OSError as error:
                    failed += 1
                    print(f"  cannot remove {cache}: {type(error).__name__}", file=sys.stderr)
                    continue
                print(f"  removed {size / GIB:6.2f} GiB  {cache}: {reason}")
        freed = sum(size for _, owned, _ in prunable for _, size in owned)
        print(f"freed {freed / GIB:.1f} GiB" + (f"; {failed} cache(s) could not be removed" if failed else ""))
        if failed:
            return 1
    if kept:
        print("kept:")
        print("\n".join(describe(kept, "keeping     ")))
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--minimum-free-gib", type=positive,
                        default=os.environ.get("AGENTDOCKER_BUILD_MIN_FREE_GIB", "5" if os.environ.get("CI") == "true" else "20"))
    parser.add_argument("--max-current-gib", type=positive, default=os.environ.get("AGENTDOCKER_BUILD_MAX_CURRENT_GIB", "12"))
    parser.add_argument("--max-total-gib", type=positive, default=os.environ.get("AGENTDOCKER_BUILD_MAX_TOTAL_GIB", "40"))
    parser.add_argument("--prune", action="store_true",
                        help="remove the caches of inactive worktrees (not this one's): no live lease, branch merged into origin/main or gone, no build running")
    parser.add_argument("--dry-run", action="store_true", help="with --prune: list what would go, with sizes, and remove nothing")
    args = parser.parse_args()
    if args.dry_run and not args.prune:
        parser.error("--dry-run goes with --prune")
    try:
        if args.prune:
            return prune(args)
        print(json.dumps(inspect(args), sort_keys=True))
    except BudgetError as error:
        print(f"build storage: {error}; no build started", file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        # OS exceptions and subprocess diagnostics can contain private checkout
        # names. Raw verification logs remain private; the budget report is safe
        # to publish on its own.
        print(f"build storage: cannot measure caches ({type(error).__name__}); no build started", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
