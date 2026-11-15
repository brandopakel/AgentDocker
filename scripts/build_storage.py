#!/usr/bin/env python3
"""Disk budget for local build campaigns.

Without flags: a read-only preflight that refuses a build when the caches
are over budget. With `--prune`: remove the Cargo caches of worktrees that
are inactive — no live AgentDocker lease on them, a branch already merged
into origin/main (or gone), and no build running in them. `--prune
--dry-run` only says what would go, with sizes; verify.sh prints that at the
end of every run and never prunes on its own. With `--seed-from <worktree>`:
start this checkout's cache from another's, bringing the builds of the
packages that come from elsewhere and nothing built from a checkout's own
files, which this checkout builds itself.
"""
import argparse
from collections import namedtuple
from contextlib import contextmanager, nullcontext
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
# The slot one local build campaign holds at a time; a real prune holds it
# too, so no build starts under a cache while it goes.
CAMPAIGN = "task:local-cargo-campaign"

# One running process as the prune sees it: `places` are the paths the kernel
# knows it by (working directory, executable); `blind` says one of the two
# could not be read, so nothing can rule the process out of any worktree.
Process = namedtuple("Process", "pid command places blind", defaults=(False,))
# What must be known of a process before it is ruled out of a worktree, as
# lsof names them: where it works, and what it runs.
REQUIRED_PLACES = ("cwd", "txt")


class BudgetError(Exception):
    """A storage refusal whose message contains no private paths."""


class PruneRefused(Exception):
    """A prune that cannot be made safe; nothing was removed."""


class SeedRefused(Exception):
    """A seed that cannot be made safe; nothing of it is left."""


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


def cargo_target_directory():
    """Where Cargo puts this checkout's build output."""
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=ROOT, text=True, stderr=subprocess.PIPE))
    return Path(metadata["target_directory"]).resolve()


def registered_worktrees():
    """The repository's checkouts as git lists them, the main one first."""
    return parse_worktrees(subprocess.check_output(["git", "worktree", "list", "--porcelain", "-z"], cwd=ROOT, stderr=subprocess.PIPE))


def measure(minimum_free_gib=None):
    """The current target directory, the registered worktrees and every cache's size.

    With a minimum, a filesystem below it is refused before anything is measured.
    """
    if minimum_free_gib is not None and shutil.disk_usage(ROOT).free < minimum_free_gib * GIB:
        raise BudgetError(f"less than {minimum_free_gib} GiB free; clean inactive build output before compiling")
    current = cargo_target_directory()
    records = registered_worktrees()
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


def cli_environment():
    """The environment the machine's own daemon is asked in, as verify.sh asks
    it: a session's private or test daemon knows nothing of the worktrees
    other sessions hold, and no daemon is ever started for this."""
    env = {key: value for key, value in os.environ.items()
           if key not in ("AGENTDOCKER_HOME", "AGENTDOCKER_SOCKET", "AGENTDOCKER_AGENT_ID")}
    env["AGENTDOCKER_NO_AUTOSTART"] = "1"
    return env


def cli(*args, timeout=60):
    """One agentdocker call's stdout; raises CalledProcessError with its stderr."""
    return subprocess.check_output(["agentdocker", *args], text=True, stderr=subprocess.PIPE,
                                   env=cli_environment(), timeout=timeout)


def lease_resources():
    """Every live lease's resource, or None when no daemon answers."""
    try:
        return parse_lease_resources(cli("leases"))
    except (OSError, subprocess.SubprocessError):
        return None


def process_table():
    """This user's live processes as `Process` records: the places are the
    paths the kernel knows a process by — its working directory and the
    files it executes — since a command started as `target/debug/app` or
    `python3 server.py` names no worktree on its command line, and an
    absolute interpreter (`/usr/bin/python3 server.py`) says where the
    interpreter is, not where its script runs. A live process whose working
    directory or executable cannot be read is `blind`, whatever its command
    line names. Another user's processes and zombies do not build in this
    user's worktrees and are left out; so is a pid that has exited by the
    time its places are read. None when the processes or their places
    cannot be read at all: then every worktree may be busy.
    """
    try:
        listing = subprocess.check_output(["ps", "-e", "-ww", "-o", "pid=,uid=,stat=,command="],
                                          text=True, stderr=subprocess.PIPE, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return None
    processes = []
    for line in listing.splitlines():
        parts = line.split(None, 3)
        if len(parts) < 3 or not parts[0].isdigit() or not parts[1].isdigit():
            continue
        pid, uid, stat = int(parts[0]), int(parts[1]), parts[2]
        if pid == os.getpid() or uid != os.getuid() or "Z" in stat:
            continue
        processes.append((pid, parts[3] if len(parts) > 3 else ""))
    read = process_places([pid for pid, _ in processes])
    if read is None:
        return None
    places, unread, exited = read
    return [Process(pid, command, tuple(places.get(pid, ())), pid in unread)
            for pid, command in processes if pid not in exited]


def chunked(items, size=100):
    return [items[start:start + size] for start in range(0, len(items), size)]


def process_places(pids, proc=Path("/proc")):
    """What the kernel says of each pid's places: (places, unread, exited) —
    pid -> the paths it works in and executes; the live pids whose working
    directory or executable could not be read; the pids that have exited or
    are zombies. /proc on Linux. Elsewhere one `lsof -d cwd,txt` scan of
    every process; a pid it passed without both a cwd and a txt record is
    asked about again on its own, since the scan reads processes one at a
    time while they start and end, and a pid still missing one then is
    unread unless it has ended. None when neither /proc nor lsof can be
    asked at all."""
    places, unread, exited = {}, set(), set()
    if proc.is_dir():
        for pid in pids:
            missing = False
            for name in ("cwd", "exe"):
                try:
                    target = os.readlink(proc / str(pid) / name)
                except OSError:
                    missing = True
                    continue
                if target.endswith(" (deleted)"):
                    target = target[:-len(" (deleted)")]
                places.setdefault(pid, []).append(Path(target))
            if missing:
                (exited if proc_ended(proc, pid) else unread).add(pid)
        return places, unread, exited
    if not shutil.which("lsof"):
        return None
    listing = lsof_records()
    if listing is None:
        return None
    seen = {}
    read_lsof(listing, places, seen)
    incomplete = [pid for pid in pids if not set(REQUIRED_PLACES) <= seen.get(pid, set())]
    for chunk in chunked(incomplete):
        again = lsof_records("-a", "-p", ",".join(str(pid) for pid in chunk))
        if again is not None:
            read_lsof(again, places, seen)
    missing = [pid for pid in incomplete if not set(REQUIRED_PLACES) <= seen.get(pid, set())]
    gone = ended(missing)
    for pid in missing:
        (exited if pid in gone else unread).add(pid)
    return {pid: list(dict.fromkeys(found)) for pid, found in places.items()}, unread, exited


def proc_ended(proc, pid):
    """The pid is gone from /proc, or is a zombie there: nothing left of it
    works anywhere. A pid whose state cannot be read is taken as alive."""
    try:
        stat = (proc / str(pid) / "stat").read_text()
    except OSError:
        return not (proc / str(pid)).exists()
    # The command name is in parentheses and may hold either; the state follows the last.
    fields = stat.rpartition(")")[2].split()
    return bool(fields) and fields[0] in ("Z", "X", "x")


def lsof_records(*selection):
    """`lsof -F pfn` records of the working directories and executables of
    the processes `selection` names (every process when empty), or None
    when lsof cannot be run. lsof exits 1 when it could not read some
    process; what it read is listed all the same, and a process it cannot
    read at all gets no record."""
    try:
        return subprocess.check_output(["lsof", *selection, "-d", ",".join(REQUIRED_PLACES), "-F", "pfn"],
                                       text=True, stderr=subprocess.DEVNULL, timeout=120)
    except subprocess.CalledProcessError as error:
        return error.output or ""
    except (OSError, subprocess.SubprocessError):
        return None


def read_lsof(listing, places, seen):
    """Fold `lsof -F pfn` records into places (pid -> paths) and seen (pid ->
    the descriptors read, cwd and txt told apart by their `f` record). A
    name that is not a path, or one under no descriptor, reads nothing."""
    pid = descriptor = None
    for line in listing.splitlines():
        tag, value = line[:1], line[1:]
        if tag == "p":
            pid, descriptor = (int(value) if value.isdigit() else None), None
        elif tag == "f":
            descriptor = value
        elif tag == "n":
            # One name per descriptor: a second one belongs to none.
            if pid is not None and descriptor in REQUIRED_PLACES and value.startswith("/"):
                seen.setdefault(pid, set()).add(descriptor)
                places.setdefault(pid, []).append(Path(value))
            descriptor = None


def ended(pids):
    """The pids among these that have exited, or are zombies: nothing left of
    them works anywhere. Existence is the kernel's answer to signal 0; ps is
    asked only whether a pid that exists is a zombie, since a `ps -p` list
    with one pid it refuses answers for none. A pid that cannot be told is
    taken as alive."""
    gone, present = set(), []
    for pid in pids:
        if pid <= 0:
            present.append(pid)
            continue
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            gone.add(pid)
            continue
        except OSError:
            pass
        present.append(pid)
    for chunk in chunked([pid for pid in present if pid > 0]):
        try:
            answer = subprocess.run(["ps", "-o", "pid=,stat=", "-p", ",".join(str(pid) for pid in chunk)],
                                    capture_output=True, text=True, timeout=60).stdout
        except (OSError, subprocess.SubprocessError):
            continue
        for line in answer.splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[0].isdigit() and "Z" in parts[1]:
                gone.add(int(parts[0]))
    return gone


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
        try:
            self.main_tip = self.rev_parse("origin/main")
        except subprocess.TimeoutExpired:
            self.main_tip = None

    def rev_parse(self, ref):
        """The commit `ref` names, or None when there is none. A git that
        does not answer raises: it says nothing about the ref."""
        try:
            return subprocess.check_output(["git", "rev-parse", "--verify", "--quiet", ref + "^{commit}"],
                                           cwd=self.root, text=True, stderr=subprocess.DEVNULL, timeout=60).strip() or None
        except subprocess.CalledProcessError:
            return None

    def state(self, record):
        """merged, current (at origin/main's tip), unmerged, absent or
        unknown; unknown too when git does not answer, so a hung git never
        reads as a branch that is gone."""
        if self.main_tip is None:
            return "unknown"
        try:
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
                                    cwd=self.root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
        except subprocess.TimeoutExpired:
            return "unknown"
        return "merged" if result.returncode == 0 else "unmerged"


def owned_caches(record, caches):
    root = record["path"].resolve()
    return [(cache, caches.get(cache, 0)) for cache in (root / name for name in CACHE_NAMES)
            if cache in caches or cache.is_symlink()]


def decide(record, owned, *, current_root, current_target, main_root, leases, processes, branch_state, lock_held):
    """Why this worktree's caches stay, or (None, why they may go).

    `leases` is the set of live lease resources, or None when no daemon
    answers — then every worktree may be active. `processes` is a `Process`
    (pid, command line, places, blind) per running process, or None when
    they cannot be listed — then every worktree may be busy; a blind
    process keeps every worktree. `branch_state(record)` and
    `lock_held(cache)` are asked only once the cheaper checks pass.
    """
    root = record["path"].resolve()
    label = record["branch"] or (record["head"] or "detached")[:12]
    if root == current_root or any(cache == current_target for cache, _ in owned):
        return "the current worktree", None
    if root == main_root:
        return "the project's main checkout", None
    if leases is None:
        return "no daemon answers; every worktree may be active", None
    if processes is None:
        return "processes cannot be listed; every worktree may be busy", None
    held = sorted(resource for resource in leases
                  if (resource.startswith("path:") and (within(resource[5:], root) or within(root, resource[5:])))
                  or (record["branch"] is not None and resource == "branch:" + record["branch"]))
    if held:
        return f"lease {held[0]} is live", None
    for entry in processes:
        pid, command, places, blind = Process(*entry)
        name = os.path.basename(command.split(None, 1)[0]) if command.strip() else "?"
        if mentions(command, root) or any(within(place, root) for place in places):
            return f"process {pid} ({name}) runs in it", None
        if blind:
            return f"process {pid} ({name}) runs somewhere that cannot be read; it may be here", None
    state = branch_state(record)
    if state == "unmerged":
        return f"branch {label} is not merged into origin/main", None
    if state == "current":
        return f"{label} is at origin/main's tip: current, not a leftover", None
    if state == "unknown":
        return "origin/main is unknown here; nothing counts as merged", None
    links = [cache for cache, _ in owned if cache.is_symlink()]
    if links:
        return f"{links[0].name} is a symbolic link; its target may be shared", None
    if any(lock_held(cache) for cache, _ in owned):
        return "a Cargo build holds its lock", None
    return None, ({"merged": f"branch {label} is merged into origin/main",
                   "absent": f"branch {label} no longer exists"}[state] + "; no lease; no build running")


def plan_prune(records, caches, *, current_root, current_target, leases, processes, branch_state, lock_held):
    """Decide each worktree's caches: (prunable, kept), both lists of
    (worktree path, [(cache path, bytes)], reason). See `decide`."""
    prunable, kept = [], []
    main_root = records[0]["path"].resolve() if records else None
    for record in records:
        owned = owned_caches(record, caches)
        if not owned:
            continue
        reason, why = decide(record, owned, current_root=current_root, current_target=current_target, main_root=main_root,
                             leases=leases, processes=processes, branch_state=branch_state, lock_held=lock_held)
        if reason is not None:
            kept.append((record["path"].resolve(), owned, reason))
        else:
            prunable.append((record["path"].resolve(), owned, why))
    return prunable, kept


# --- seeding -----------------------------------------------------------------

# The profile directory the local gates build in: a seed brings that one's.
SEED_PROFILE = "debug"
# What Cargo puts on a target directory it makes so that backups pass it by.
BACKUP_EXCLUSION = "com.apple.metadata:com_apple_backup_excludeItem"


def lock_packages(root):
    """What `root`'s Cargo.lock builds, by name: (own, foreign). A package
    with no `source` is built from the checkout's own files, a workspace
    member or a path dependency. Every other comes from a registry or a git
    repository and is the same wherever it is built."""
    lock = root / "Cargo.lock"
    try:
        import tomllib
        with open(lock, "rb") as handle:
            packages = tomllib.load(handle).get("package", [])
        named = [(package["name"], "source" in package) for package in packages]
    except ImportError:
        raise SeedRefused("reading Cargo.lock needs Python 3.11 or later")
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        raise SeedRefused(f"{lock} cannot be read ({type(error).__name__}), so which packages are a checkout's own cannot be told")
    own = {name for name, foreign in named if not foreign}
    return own, {name for name, foreign in named if foreign} - own


@contextmanager
def build_held_off(profile, *, make=False):
    """Cargo's own lock on a profile directory, held so that no build runs
    there meanwhile. Cargo holds `<profile>/.cargo-lock` for as long as it
    builds (shared where it files output by package, exclusively before
    that), so taking it exclusively is refused while a build runs, and a
    build that starts while it is held waits for it. `make` is for a
    directory Cargo has not built in yet."""
    try:
        import fcntl
    except ImportError:
        raise SeedRefused("this system has no file lock to hold a Cargo build off with")
    try:
        handle = open(profile / ".cargo-lock", "ab" if make else "rb")
    except OSError as error:
        raise SeedRefused(f"{profile} has no Cargo lock to hold ({type(error).__name__}): it is not a directory Cargo built in")
    with handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SeedRefused(f"a Cargo build is running in {profile}; seed once it has ended")
        except OSError as error:
            raise SeedRefused(f"the Cargo lock of {profile} cannot be held ({type(error).__name__})")
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def copy_units(sources, into):
    """Copy each directory into `into` under its own name, times and modes
    kept: copy-on-write where `cp` and the filesystem have it (`-c` on
    macOS, `--reflink=auto` with GNU cp), an ordinary copy otherwise."""
    command = {"darwin": ["cp", "-c", "-R", "-p"], "linux": ["cp", "-R", "-p", "--reflink=auto"]}.get(sys.platform)
    for chunk in chunked(sources, 50):
        if command is not None:
            try:
                done = subprocess.run([*command, *(str(source) for source in chunk), str(into)],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
            except OSError:
                done = False
            if done:
                continue
            # A cp without that option, or a filesystem that refuses it: what
            # it left of these goes, and they and the rest are copied plainly.
            command = None
            for source in chunk:
                shutil.rmtree(into / source.name, ignore_errors=True)
        for source in chunk:
            shutil.copytree(source, into / source.name, symlinks=True)


def mark_as_cache(made, like):
    """Give a target directory the seed made what Cargo gives one it makes,
    as `like` has them: the tag that says it is a cache, and on macOS the
    mark that keeps backups off it. Cargo adds neither to a directory it
    finds already there. Whatever cannot be given is left out."""
    try:
        if (like / "CACHEDIR.TAG").is_file() and not (made / "CACHEDIR.TAG").exists():
            shutil.copy2(like / "CACHEDIR.TAG", made / "CACHEDIR.TAG")
    except OSError:
        pass
    if sys.platform != "darwin" or not shutil.which("xattr"):
        return
    try:
        value = subprocess.run(["xattr", "-px", BACKUP_EXCLUSION, str(like)], capture_output=True, text=True, timeout=30)
        if value.returncode == 0 and value.stdout.split():
            subprocess.run(["xattr", "-wx", BACKUP_EXCLUSION, "".join(value.stdout.split()), str(made)],
                           capture_output=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        pass


def seed_cache(source_root, source_target, dest_root, dest_target, *, copy=copy_units):
    """Start the cache `dest_target` of the checkout `dest_root` from the
    cache `source_target` of the checkout `source_root`.

    Cargo files a profile's output by package (`<profile>/build/<package>/`)
    and tells whether a unit is fresh by times alone, in folders whose names
    do not depend on where the checkout is. A unit built from one checkout's
    files, copied beside another checkout whose files are older than it, is
    therefore taken as that checkout's own and never rebuilt. So a seed
    brings only the folders of packages the new checkout's Cargo.lock takes
    from elsewhere, and of those none that either checkout builds from its
    own files; incremental state, binaries and everything else are left for
    the new checkout to build. Cargo's lock is held on both profile
    directories while the copy is made, so nothing is copied from under a
    build and none starts on a cache half made.

    Returns (brought, own, others): the packages brought, the packages left
    out as a checkout's own, and how many other folders were left out.
    """
    source_profile, dest_profile = source_target / SEED_PROFILE, dest_target / SEED_PROFILE
    source_build, dest_build = source_profile / "build", dest_profile / "build"
    for path in (source_target, source_profile, source_build, dest_target, dest_profile, dest_build):
        if path.is_symlink():
            raise SeedRefused(f"{path} is a symbolic link; what it names may be shared")
    if not source_build.is_dir():
        raise SeedRefused(f"{source_target} has no {SEED_PROFILE} build to seed from")
    source_own, _ = lock_packages(source_root)
    dest_own, foreign = lock_packages(dest_root)
    own = source_own | dest_own
    with build_held_off(source_profile):
        names = sorted(entry.name for entry in source_build.iterdir() if entry.is_dir() and not entry.is_symlink())
        brought = [name for name in names if name in foreign and name not in own]
        left_own = [name for name in names if name in own]
        if not brought:
            raise SeedRefused(f"{source_build} holds no build of a package this checkout takes from elsewhere")
        made_target = not dest_target.exists()
        dest_profile.mkdir(parents=True, exist_ok=True)
        if made_target:
            mark_as_cache(dest_target, source_target)
        with build_held_off(dest_profile, make=True):
            # Looked at under the lock: a build that ended a moment ago has
            # left its output, and nothing is copied in beside it.
            if dest_build.exists() and (not dest_build.is_dir() or any(dest_build.iterdir())):
                raise SeedRefused(f"{dest_target} holds build output already; a seed is for a checkout that has built nothing")
            dest_build.mkdir(exist_ok=True)
            try:
                copy([source_build / name for name in brought], dest_build)
            except (OSError, subprocess.SubprocessError, shutil.Error) as error:
                shutil.rmtree(dest_build, ignore_errors=True)
                raise SeedRefused(f"the copy failed ({type(error).__name__}), and what it had made is removed")
    return brought, left_own, len(names) - len(brought) - len(left_own)


def seed(args):
    """`--seed-from`: this checkout's cache started from a registered worktree's."""
    dest_root = ROOT.resolve()
    named = Path(args.seed_from).expanduser()
    source_root = (named if named.is_absolute() else Path.cwd() / named).resolve()
    if source_root not in {record["path"].resolve() for record in registered_worktrees()}:
        raise SeedRefused(f"{source_root} is not a checkout of this repository as `git worktree list` names them")
    if source_root == dest_root:
        raise SeedRefused("that is this checkout; name another, whose cache is warm")
    source_target, dest_target = source_root / "target", cargo_target_directory()
    if source_target.resolve() == dest_target:
        raise SeedRefused("both checkouts build in one target directory; there is nothing to bring")
    if shutil.disk_usage(existing_parent(dest_target)).free < args.minimum_free_gib * GIB:
        raise SeedRefused(f"less than {args.minimum_free_gib} GiB is free where the cache would go")
    brought, own, others = seed_cache(source_root, source_target, dest_root, dest_target)
    try:
        size = f" ({allocated(dest_target / SEED_PROFILE / 'build') / GIB:.2f} GiB)"
    except (OSError, ValueError, subprocess.SubprocessError):
        size = ""
    print(f"build storage: seeded {dest_target / SEED_PROFILE} from {source_target / SEED_PROFILE}")
    print(f"  brought: the builds of {len(brought)} packages that come from elsewhere{size}, times kept, "
          "copy-on-write where the filesystem has it")
    print("  left for this checkout to build: "
          + (f"the checkouts' own packages ({', '.join(own)}), " if own else "")
          + "incremental state, binaries"
          + (f", and {others} folders this checkout's Cargo.lock does not name" if others else ""))
    return 0


class Campaign:
    """The build slot, held for a real prune as verify.sh holds it for a run:
    as the session when the daemon knows it, as a transient agent of this
    run otherwise, and released on the way out. A lease the caller already
    holds (AGENTDOCKER_CAMPAIGN_LEASE=off) is not taken again. A daemon that
    will not give the slot gives no prune."""

    def __init__(self, note):
        self.note = note
        self.lease = None
        self.agent = os.environ.get("AGENTDOCKER_AGENT_ID") or None
        self.transient = None

    def __enter__(self):
        if os.environ.get("AGENTDOCKER_CAMPAIGN_LEASE", "on") == "off":
            return self
        wait = os.environ.get("AGENTDOCKER_CAMPAIGN_WAIT", "600")
        try:
            self.lease = self.claim(wait)
        except subprocess.CalledProcessError as error:
            text = (error.stderr or error.output or "").strip()
            if not any(mark in text for mark in ("give --as", "specify the sender", "--as <AGENT>", "no agent matches")):
                raise PruneRefused(f"{CAMPAIGN} is held by another campaign; nothing pruned ({text.splitlines()[0] if text else error})")
            try:
                self.agent = self.transient = cli("register", "--name", f"prune-{os.getpid()}", "--runtime", "custom",
                                                  "--pid", str(os.getpid()), "--workdir", str(ROOT),
                                                  "--label", "campaign=build_storage.py").strip()
                self.lease = self.claim(wait)
            except subprocess.CalledProcessError as again:
                detail = (again.stderr or again.output or "").strip().splitlines()
                self.__exit__(None, None, None)
                raise PruneRefused(f"cannot hold {CAMPAIGN}; nothing pruned ({detail[0] if detail else again})")
        except (OSError, subprocess.SubprocessError) as error:
            raise PruneRefused(f"no daemon answers for {CAMPAIGN}; nothing pruned ({type(error).__name__})")
        return self

    def claim(self, wait):
        return cli("claim", CAMPAIGN, *(["--as", self.agent] if self.agent else []), "--ttl", "1800",
                   "--wait", wait, "--note", self.note, timeout=int(wait) + 60).strip()

    def __exit__(self, *_):
        as_agent = ["--as", self.agent] if self.agent else []
        if self.lease:
            try:
                cli("release", self.lease, *as_agent, "--summary", "build_storage.py --prune finished")
            except (OSError, subprocess.SubprocessError):
                pass
        if self.transient:
            try:
                cli("deregister", "--as", self.transient)
            except (OSError, subprocess.SubprocessError):
                pass
        self.lease = self.transient = None


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
    for pid, command, _, blind in (Process(*entry) for entry in processes or ()):
        if blind:
            name = os.path.basename(command.split(None, 1)[0]) if command.strip() else "?"
            print(f"process {pid} ({name}) cannot be placed: its working directory or executable cannot be read, "
                  "so it may be in any worktree; nothing is prunable while it runs")
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
        failed, freed = 0, 0
        by_root = {record["path"].resolve(): record for record in records}
        main_root = records[0]["path"].resolve() if records else None
        # Nothing is held between the plan and the removals, so each worktree
        # is judged again, on fresh leases and processes, right before its
        # own caches go; a build slot is held meanwhile so no verify.sh run
        # starts under a cache on its way out.
        with (Campaign(f"build_storage.py --prune in {ROOT}") if prunable else nullcontext()):
            for root, owned, reason in prunable:
                kept_now, _ = decide(by_root[root], owned, current_root=ROOT.resolve(), current_target=current, main_root=main_root,
                                     leases=lease_resources(), processes=process_table(), branch_state=branches.state,
                                     lock_held=build_lock_held)
                if kept_now is not None:
                    print(f"  skipped {sum(size for _, size in owned) / GIB:6.2f} GiB  {root}: {kept_now} (changed since the plan)")
                    kept.append((root, owned, kept_now))
                    continue
                for cache, size in owned:
                    try:
                        shutil.rmtree(cache)
                    except OSError as error:
                        failed += 1
                        print(f"  cannot remove {cache}: {type(error).__name__}", file=sys.stderr)
                        continue
                    freed += size
                    print(f"  removed {size / GIB:6.2f} GiB  {cache}: {reason}")
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
    parser.add_argument("--seed-from", metavar="WORKTREE",
                        help="start this checkout's debug cache from that worktree's: the builds of the packages that come from "
                             "elsewhere, never what is built from a checkout's own files; refused while a build runs in either")
    args = parser.parse_args()
    if args.dry_run and not args.prune:
        parser.error("--dry-run goes with --prune")
    if args.seed_from is not None and args.prune:
        parser.error("--seed-from and --prune are separate runs")
    try:
        if args.seed_from is not None:
            return seed(args)
        if args.prune:
            return prune(args)
        print(json.dumps(inspect(args), sort_keys=True))
    except BudgetError as error:
        print(f"build storage: {error}; no build started", file=sys.stderr)
        return 1
    except PruneRefused as error:
        print(f"build storage: {error}", file=sys.stderr)
        return 1
    except SeedRefused as error:
        print(f"build storage: {error}; nothing was seeded", file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        if args.seed_from is not None:
            print(f"build storage: cannot seed the cache ({type(error).__name__}); nothing was seeded", file=sys.stderr)
            return 1
        # OS exceptions and subprocess diagnostics can contain private checkout
        # names. Raw verification logs remain private; the budget report is safe
        # to publish on its own.
        print(f"build storage: cannot measure caches ({type(error).__name__}); no build started", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
