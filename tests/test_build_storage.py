"""A failed resource preflight must stop the build before Cargo runs, and
`--prune` removes only the caches of worktrees nothing is using."""
import importlib.util
import io
import json
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch
from contextlib import redirect_stderr, redirect_stdout

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("build_storage", ROOT / "scripts/build_storage.py")
STORAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STORAGE)


class BuildStorage(unittest.TestCase):
    def test_report_does_not_publish_private_target_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "private-user-project"
            root.mkdir()
            target = root / "custom-cache"
            target.mkdir()
            args = SimpleNamespace(minimum_free_gib=1, max_current_gib=12, max_total_gib=40)
            with patch.object(STORAGE, "ROOT", root), \
                    patch.object(STORAGE.shutil, "disk_usage", return_value=SimpleNamespace(free=100 * STORAGE.GIB)), \
                    patch.object(STORAGE.subprocess, "check_output", side_effect=[json.dumps({"target_directory":str(target)}), b""]), \
                    patch.object(STORAGE, "allocated", return_value=4096):
                report = STORAGE.inspect(args)
            self.assertNotIn(str(root), json.dumps(report))
            self.assertNotIn("current_target", report)

    def test_failed_measurement_does_not_publish_private_oserror_path(self):
        error = OSError(13, "permission denied", "/private-user-project/custom-cache")
        captured = io.StringIO()
        with patch.object(STORAGE.sys, "argv", ["build_storage.py"]), \
                patch.object(STORAGE, "inspect", side_effect=error), redirect_stderr(captured):
            self.assertEqual(STORAGE.main(), 1)
        self.assertNotIn("private-user-project", captured.getvalue())
        self.assertIn("no build started", captured.getvalue())

    def test_fallback_counts_directory_blocks_and_deduplicates_hardlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for number in range(50):
                (root / str(number)).mkdir()
            payload = root / "0/payload"
            payload.write_bytes(b"data")
            os.link(payload, root / "1/payload")
            original_lstat = Path.lstat

            def allocated_metadata(path):
                metadata = original_lstat(path)
                # Model a filesystem allocating 4 KiB per inode, including
                # empty directories. APFS can report zero for tiny directories.
                return SimpleNamespace(st_dev=metadata.st_dev, st_ino=metadata.st_ino,
                                       st_size=metadata.st_size, st_blocks=8)

            with patch.object(STORAGE.shutil, "which", return_value=None), \
                    patch.object(Path, "lstat", allocated_metadata):
                self.assertEqual(STORAGE.allocated(root), 52 * 4096)

    def test_low_space_refuses_verification_before_cargo(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / "cargo-called"
            cargo = root / "cargo"
            cargo.write_text('#!/bin/sh\ntouch "$BUILD_TEST_MARKER"\nexit 99\n')
            cargo.chmod(0o700)
            result = subprocess.run(["bash", str(ROOT / "scripts/verify.sh"), "check"],
                                    cwd=ROOT, capture_output=True, text=True, timeout=30,
                                    env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                                         "BUILD_TEST_MARKER": str(marker), "AGENTDOCKER_BUILD_MIN_FREE_GIB": str(1 << 63)})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("GiB free", result.stderr)
            self.assertFalse(marker.exists(), "resource refusal must happen before Cargo")


def record(path, branch=None, head="a" * 40):
    return {"path": Path(path), "head": head, "branch": branch}


class PruneSelection(unittest.TestCase):
    """plan_prune on fake inputs: a worktree's caches go only when no lease,
    no process and no unmerged branch speaks for it."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.records = [record(self.base / "main", "main")]
        self.caches = {}
        self.states = {}
        self.locked = set()

    def worktree(self, name, branch=None, state="merged", cache_bytes=3 * STORAGE.GIB, caches=("target",), head=None):
        root = self.base / name
        for cache in caches:
            (root / cache).mkdir(parents=True)
            self.caches[root / cache] = cache_bytes
        entry = record(root, branch, head or (("b" * 40) if branch else ("c" * 40)))
        self.records.append(entry)
        self.states[root] = state
        return root

    def plan(self, leases=frozenset(), processes=(), current=None):
        current = current or self.base / "elsewhere"
        return STORAGE.plan_prune(
            self.records, self.caches, current_root=current, current_target=current / "target",
            leases=None if leases is None else set(leases), processes=None if processes is None else list(processes),
            branch_state=lambda entry: self.states[entry["path"].resolve()],
            lock_held=lambda cache: cache in self.locked)

    def roots(self, entries):
        return [root for root, _, _ in entries]

    def test_a_merged_or_absent_branch_nobody_holds_or_builds_is_prunable_with_its_sizes(self):
        merged = self.worktree("merged", "feature/done", "merged", cache_bytes=5 * STORAGE.GIB, caches=("target", "fuzz/target"))
        absent = self.worktree("absent", "feature/gone", "absent")
        detached = self.worktree("detached", None, "merged")
        prunable, kept = self.plan()
        self.assertEqual(self.roots(prunable), [merged, absent, detached])
        root, owned, reason = prunable[0]
        self.assertEqual([(cache.relative_to(root).as_posix(), size) for cache, size in owned],
                         [("target", 5 * STORAGE.GIB), ("fuzz/target", 5 * STORAGE.GIB)])
        self.assertIn("feature/done is merged into origin/main", reason)
        self.assertIn("no longer exists", prunable[1][2])
        # The main checkout has no cache here, so nothing is kept either.
        self.assertEqual(kept, [])

    def test_the_main_checkout_and_the_current_worktree_are_never_candidates(self):
        (self.base / "main/target").mkdir(parents=True)
        self.caches[self.base / "main/target"] = STORAGE.GIB
        current = self.worktree("current", "feature/mine", "merged")
        prunable, kept = self.plan(current=current)
        self.assertEqual(prunable, [])
        self.assertEqual({root: reason for root, _, reason in kept},
                         {self.base / "main": "the project's main checkout", current: "the current worktree"})

    def test_a_cache_that_is_the_current_target_directory_is_kept_wherever_it_lives(self):
        shared = self.worktree("shared", "feature/old", "merged")
        prunable, kept = STORAGE.plan_prune(
            self.records, self.caches, current_root=self.base / "elsewhere", current_target=shared / "target",
            leases=set(), processes=[], branch_state=lambda entry: "merged", lock_held=lambda cache: False)
        self.assertEqual(prunable, [])
        self.assertEqual(kept[0][2], "the current worktree")

    def test_a_live_lease_on_the_path_a_file_in_it_a_parent_or_the_branch_keeps_it(self):
        whole = self.worktree("whole", "feature/a")
        inside = self.worktree("inside", "feature/b")
        parent = self.worktree("parent", "feature/c")
        branch = self.worktree("branch", "feature/d")
        free = self.worktree("free", "feature/e")
        leases = {f"path:{whole}", f"path:{inside}/src/lib.rs", f"path:{self.base}", "branch:feature/d",
                  "task:local-cargo-campaign"}
        prunable, kept = self.plan(leases=leases)
        # The lease on the parent folder covers every worktree under it.
        self.assertEqual(prunable, [])
        reasons = {root: reason for root, _, reason in kept}
        for root in (whole, inside, parent, free):
            self.assertRegex(reasons[root], r"^lease path:.* is live$")
        self.assertEqual(reasons[branch], "lease branch:feature/d is live")
        prunable, kept = self.plan(leases={f"path:{whole}", f"path:{inside}/src/lib.rs"})
        self.assertEqual(self.roots(prunable), [parent, branch, free])
        reasons = {root: reason for root, _, reason in kept}
        self.assertEqual(reasons[whole], f"lease path:{whole} is live")
        self.assertEqual(reasons[inside], f"lease path:{inside}/src/lib.rs is live")
        prunable, kept = self.plan(leases={"branch:feature/d", f"path:{whole}"})
        self.assertEqual(self.roots(prunable), [inside, parent, free])
        self.assertEqual({root: reason for root, _, reason in kept}[branch], "lease branch:feature/d is live")

    def test_no_daemon_or_no_process_table_means_nothing_is_prunable(self):
        self.worktree("merged", "feature/done")
        prunable, kept = self.plan(leases=None)
        self.assertEqual(prunable, [])
        self.assertIn("no daemon answers", kept[0][2])
        prunable, kept = self.plan(processes=None)
        self.assertEqual(prunable, [])
        self.assertIn("processes cannot be listed", kept[0][2])

    def test_a_process_naming_the_worktree_or_working_in_it_keeps_it_but_not_a_longer_sibling(self):
        short = self.worktree("ad-9", "feature/short")
        longer = self.worktree("ad-93", "feature/long")
        by_cwd = self.worktree("cwd", "feature/cwd")
        idle = self.worktree("idle", "feature/idle")
        processes = [
            (10, f"/usr/bin/rustc --crate-name x --out-dir {longer}/target/debug/deps", None),
            (11, "cargo test -p agentdocker", by_cwd / "crates/cli"),
            (12, f"zsh -c cd {idle}-other && make", None),
        ]
        prunable, kept = self.plan(processes=processes)
        self.assertEqual(self.roots(prunable), [short, idle])
        reasons = {root: reason for root, _, reason in kept}
        self.assertEqual(reasons[longer], "process 10 (rustc) runs in it")
        self.assertEqual(reasons[by_cwd], "process 11 (cargo) runs in it")

    def test_an_unmerged_current_or_unknown_checkout_keeps_its_cache(self):
        unmerged = self.worktree("unmerged", "feature/wip", "unmerged")
        current = self.worktree("current", "feature/new", "current")
        detached = self.worktree("detached", None, "current")
        unknown = self.worktree("unknown", "feature/x", "unknown")
        prunable, kept = self.plan()
        self.assertEqual(prunable, [])
        reasons = {root: reason for root, _, reason in kept}
        self.assertIn("not merged into origin/main", reasons[unmerged])
        self.assertEqual(reasons[current], "feature/new is at origin/main's tip: current, not a leftover")
        self.assertEqual(reasons[detached], "cccccccccccc is at origin/main's tip: current, not a leftover")
        self.assertIn("origin/main is unknown", reasons[unknown])

    def test_git_says_merged_current_unmerged_absent_or_unknown(self):
        def git(*args, cwd):
            return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            origin, clone = base / "origin", base / "clone"
            origin.mkdir()
            git("init", "-q", "-b", "main", cwd=origin)
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "one", cwd=origin)
            old = git("rev-parse", "HEAD", cwd=origin)
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "two", cwd=origin)
            git("clone", "-q", str(origin), str(clone), cwd=base)
            git("branch", "merged", old, cwd=clone)
            git("branch", "fresh", "origin/main", cwd=clone)
            git("checkout", "-q", "-b", "wip", cwd=clone)
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "three", cwd=clone)
            branches = STORAGE.Branches(clone)
            self.assertEqual(branches.state(record(clone, "merged")), "merged")
            self.assertEqual(branches.state(record(clone, "fresh")), "current")
            self.assertEqual(branches.state(record(clone, "wip")), "unmerged")
            self.assertEqual(branches.state(record(clone, "deleted")), "absent")
            self.assertEqual(branches.state(record(clone, None, head=old)), "merged")
            self.assertEqual(branches.state(record(clone, None, head=git("rev-parse", "origin/main", cwd=clone))), "current")
            self.assertEqual(STORAGE.Branches(origin).state(record(origin, "main")), "unknown")

    def test_a_held_cargo_lock_or_a_symlinked_cache_keeps_it(self):
        locked = self.worktree("locked", "feature/building")
        self.locked.add(locked / "target")
        linked = self.base / "linked"
        linked.mkdir()
        os.symlink(self.base / "shared-target", linked / "target")
        self.records.append(record(linked, "feature/linked"))
        self.states[linked] = "merged"
        prunable, kept = self.plan()
        self.assertEqual(prunable, [])
        reasons = {root: reason for root, _, reason in kept}
        self.assertEqual(reasons[locked], "a Cargo build holds its lock")
        self.assertIn("symbolic link", reasons[linked])

    def test_a_worktree_without_a_cache_is_not_mentioned(self):
        (self.base / "bare").mkdir()
        self.records.append(record(self.base / "bare", "feature/bare"))
        self.states[self.base / "bare"] = "merged"
        self.assertEqual(self.plan(), ([], []))


class PruneParsing(unittest.TestCase):
    def test_worktree_porcelain_yields_paths_heads_and_branches(self):
        porcelain = (b"worktree /repo\0HEAD " + b"1" * 40 + b"\0branch refs/heads/main\0\0"
                     b"worktree /wt/a b\0HEAD " + b"2" * 40 + b"\0branch refs/heads/feature/x\0\0"
                     b"worktree /wt/c\0HEAD " + b"3" * 40 + b"\0detached\0\0")
        records = STORAGE.parse_worktrees(porcelain)
        self.assertEqual([(str(r["path"]), r["head"][0], r["branch"]) for r in records],
                         [("/repo", "1", "main"), ("/wt/a b", "2", "feature/x"), ("/wt/c", "3", None)])
        self.assertEqual(STORAGE.parse_worktrees(b""), [])

    def test_lease_table_yields_resources_spaces_included(self):
        table = ("LEASE ID       RESOURCE                         HOLDER         MODE        EXPIRES   NOTE\n"
                 "9f4afa751c2a   task:local-cargo-campaign        b3a71baca609   exclusive   in 1h     verify\n"
                 "a66d59c85242   path:/Users/me/My Work/wt        b3a71baca609   exclusive   in 1h     AD-1\n"
                 "a66d59c85243   branch:feature/x                 b3a71baca609   exclusive   in 1h     \n")
        self.assertEqual(STORAGE.parse_lease_resources(table),
                         {"task:local-cargo-campaign", "path:/Users/me/My Work/wt", "branch:feature/x"})
        self.assertEqual(STORAGE.parse_lease_resources("LEASE ID  RESOURCE  HOLDER\n"), set())
        self.assertEqual(STORAGE.parse_lease_resources(""), set())

    def test_an_unreachable_daemon_is_none_and_never_started(self):
        def failing(command, **kwargs):
            self.assertEqual(kwargs["env"]["AGENTDOCKER_NO_AUTOSTART"], "1")
            self.assertNotIn("AGENTDOCKER_HOME", kwargs["env"])
            raise subprocess.CalledProcessError(1, command)
        with patch.dict(os.environ, {"AGENTDOCKER_HOME": "/tmp/private"}), \
                patch.object(STORAGE.subprocess, "check_output", side_effect=failing):
            self.assertIsNone(STORAGE.lease_resources())
        with patch.object(STORAGE.subprocess, "check_output", side_effect=FileNotFoundError("agentdocker")):
            self.assertIsNone(STORAGE.lease_resources())

    def test_command_lines_mention_a_path_only_at_a_boundary(self):
        self.assertTrue(STORAGE.mentions("cargo build --manifest-path /wt/ad-9/Cargo.toml", Path("/wt/ad-9")))
        self.assertTrue(STORAGE.mentions("bash with-cargo.sh /wt/ad-9", Path("/wt/ad-9")))
        self.assertTrue(STORAGE.mentions("rustc --out-dir=/wt/ad-9/target/debug", Path("/wt/ad-9")))
        self.assertFalse(STORAGE.mentions("bash with-cargo.sh /wt/ad-93 cargo test", Path("/wt/ad-9")))
        self.assertFalse(STORAGE.mentions("bash with-cargo.sh /wt/ad-9x", Path("/wt/ad-9")))
        self.assertTrue(STORAGE.is_build_tool("/Users/me/.rustup/toolchains/stable/bin/cargo test"))
        self.assertTrue(STORAGE.is_build_tool("/opt/homebrew/bin/cargo-nextest nextest run"))
        self.assertFalse(STORAGE.is_build_tool("python3 scripts/build_storage.py"))

    def test_a_cargo_lock_somebody_holds_is_seen(self):
        fcntl = __import__("fcntl")
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary) / "target"
            (cache / "debug").mkdir(parents=True)
            lock = cache / "debug/.cargo-lock"
            lock.write_bytes(b"")
            self.assertFalse(STORAGE.build_lock_held(cache))
            # A second descriptor holds the lock, as a running cargo would; one
            # process's flock is not exclusive against itself, so hold it from
            # a child.
            holder = subprocess.Popen([
                "python3", "-c",
                "import fcntl, sys, time; f = open(sys.argv[1], 'rb'); fcntl.flock(f, fcntl.LOCK_EX); "
                "print('held', flush=True); time.sleep(30)", str(lock)], stdout=subprocess.PIPE, text=True)
            try:
                self.assertEqual(holder.stdout.readline().strip(), "held")
                self.assertTrue(STORAGE.build_lock_held(cache))
            finally:
                holder.kill()
                holder.wait()
            del fcntl


class PruneRun(unittest.TestCase):
    """main() with --prune: a dry run removes nothing and names sizes; a real
    run removes exactly the prunable caches."""

    def run_main(self, argv, records, caches, leases, processes, state):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(STORAGE.sys, "argv", ["build_storage.py", *argv]), \
                patch.object(STORAGE, "measure", return_value=(Path("/nowhere/target"), records, caches)), \
                patch.object(STORAGE, "lease_resources", return_value=leases), \
                patch.object(STORAGE, "process_table", return_value=processes), \
                patch.object(STORAGE, "Branches", lambda: SimpleNamespace(state=lambda entry: state)), \
                patch.object(STORAGE, "build_lock_held", return_value=False), \
                redirect_stdout(out), redirect_stderr(err):
            code = STORAGE.main()
        return code, out.getvalue(), err.getvalue()

    def test_dry_run_lists_sizes_and_removes_nothing_and_a_real_run_removes_only_the_candidates(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            gone, busy = base / "gone", base / "busy"
            for root in (gone, busy):
                (root / "target/debug").mkdir(parents=True)
                (root / "target/debug/out").write_text("x")
            records = [record(base / "main", "main"), record(gone, "feature/done"), record(busy, "feature/busy")]
            caches = {gone / "target": 2 * STORAGE.GIB, busy / "target": STORAGE.GIB}
            leases = {f"path:{busy}"}
            code, out, err = self.run_main(["--prune", "--dry-run"], records, caches, leases, [], "merged")
            self.assertEqual(code, 0, err)
            self.assertIn("registered worktree caches use 3.0 GiB", out)
            self.assertIn(f"would remove   2.00 GiB  {gone}  (target)", out)
            self.assertIn(f"keeping        1.00 GiB  {busy}  (target): lease path:{busy} is live", out)
            self.assertTrue((gone / "target/debug/out").exists(), "a dry run removed something")
            code, out, err = self.run_main(["--prune"], records, caches, leases, [], "merged")
            self.assertEqual(code, 0, err)
            self.assertIn(f"removed   2.00 GiB  {gone / 'target'}", out)
            self.assertIn("freed 2.0 GiB", out)
            self.assertFalse((gone / "target").exists())
            self.assertTrue((busy / "target/debug/out").exists(), "a leased cache was removed")
            self.assertTrue(gone.exists(), "only the cache goes, never the worktree")

    def test_no_daemon_prunes_nothing_and_says_so(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            (base / "gone/target").mkdir(parents=True)
            records = [record(base / "main", "main"), record(base / "gone", "feature/done")]
            code, out, _ = self.run_main(["--prune"], records, {base / "gone/target": STORAGE.GIB}, None, [], "merged")
            self.assertEqual(code, 0)
            self.assertIn("no daemon answers", out)
            self.assertIn("pruning: nothing", out)
            self.assertTrue((base / "gone/target").exists())

    def test_dry_run_needs_prune(self):
        with patch.object(STORAGE.sys, "argv", ["build_storage.py", "--dry-run"]), redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                STORAGE.main()


class VerifyReport(unittest.TestCase):
    def test_verify_reports_prunable_caches_after_the_run_without_pruning_or_failing_it(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/verify.sh", root / "scripts/verify.sh")
            log = root / "storage.log"
            # The stand-in preflight records how it was called; its report
            # run fails, which must not fail the verification.
            (root / "scripts/build_storage.py").write_text(
                "import sys, os\n"
                "open(os.environ['STORAGE_LOG'], 'a').write(' '.join(sys.argv[1:]) + '\\n')\n"
                "sys.exit(1 if '--prune' in sys.argv else 0)\n")
            binary = root / "bin"; binary.mkdir()
            cargo = binary / "cargo"
            cargo.write_text("#!/bin/sh\necho cargo >> \"$STORAGE_LOG\"\n")
            cargo.chmod(0o700)
            env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_")}
            env.update(PATH=str(binary) + os.pathsep + os.environ["PATH"], STORAGE_LOG=str(log), CI="1")
            result = subprocess.run(["bash", "scripts/verify.sh", "test"], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(log.read_text().splitlines(), ["", "cargo", "cargo", "--prune --dry-run"])
            self.assertIn("the cache report is unavailable", result.stderr)
