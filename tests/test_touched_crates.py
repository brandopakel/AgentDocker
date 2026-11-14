"""A targeted run tests the packages a change touches and the packages that
depend on them, and falls back to the whole workspace for a change every
package depends on."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("touched_crates", ROOT / "scripts/touched_crates.py")
TOUCHED = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOUCHED)

# A made-up workspace: `base` at the bottom, `middle` on it, `top` and `app`
# on `middle`, `tools` on nothing. `app` is a binary with no library, as the
# CLI is; `tools` is a procedural macro, which counts as a library.
LIB = [{"kind": ["lib"], "name": "x"}]
METADATA = {
    "workspace_root": "/work/example",
    "workspace_members": ["base 0.1.0", "middle 0.1.0", "top 0.1.0", "app 0.1.0", "tools 0.1.0", "other 1.0.0"],
    "packages": [
        {"id": "base 0.1.0", "name": "base", "manifest_path": "/work/example/crates/base/Cargo.toml", "dependencies": [],
         "targets": LIB},
        {"id": "middle 0.1.0", "name": "middle", "manifest_path": "/work/example/crates/middle/Cargo.toml",
         "dependencies": [{"name": "base"}, {"name": "serde"}], "targets": LIB},
        {"id": "top 0.1.0", "name": "top", "manifest_path": "/work/example/crates/top/Cargo.toml",
         "dependencies": [{"name": "middle"}], "targets": LIB + [{"kind": ["test"], "name": "t"}]},
        {"id": "app 0.1.0", "name": "app", "manifest_path": "/work/example/crates/app/Cargo.toml",
         "dependencies": [{"name": "middle"}, {"name": "base"}],
         "targets": [{"kind": ["bin"], "name": "app"}, {"kind": ["test"], "name": "cli"}]},
        {"id": "tools 0.1.0", "name": "tools", "manifest_path": "/work/example/tools/Cargo.toml", "dependencies": [],
         "targets": [{"kind": ["proc-macro"], "name": "tools"}]},
    ],
}


class TouchedCrates(unittest.TestCase):
    def setUp(self):
        self.packages = TOUCHED.workspace_packages(METADATA)

    def test_packages_come_with_their_directories_and_workspace_dependencies(self):
        self.assertEqual(self.packages["middle"], ("crates/middle/", {"base"}))
        self.assertEqual(self.packages["tools"], ("tools/", set()))
        self.assertNotIn("other", self.packages)

    def test_windows_metadata_matches_the_paths_git_prints(self):
        metadata = json.loads(json.dumps(METADATA))
        metadata["workspace_root"] = "C:\\work\\example"
        for package in metadata["packages"]:
            package["manifest_path"] = package["manifest_path"].replace("/work/example", "C:\\work\\example").replace("/", "\\")
        packages = TOUCHED.workspace_packages(metadata)
        self.assertEqual(packages["middle"], ("crates/middle/", {"base"}))
        self.assertEqual(TOUCHED.touched(["crates/top/src/lib.rs"], packages), {"top"})

    def test_a_change_in_a_leaf_touches_only_that_package(self):
        self.assertEqual(TOUCHED.touched(["crates/top/src/lib.rs"], self.packages), {"top"})

    def test_a_change_in_a_dependency_touches_every_dependent(self):
        self.assertEqual(
            TOUCHED.touched(["crates/base/src/lib.rs"], self.packages),
            {"base", "middle", "top", "app"},
        )

    def test_a_change_outside_every_package_touches_none(self):
        self.assertEqual(TOUCHED.touched(["docs/GUIDE.md", "scripts/verify.sh", ""], self.packages), set())

    def test_a_workspace_wide_file_touches_everything(self):
        for path in ("Cargo.lock", "Cargo.toml", ".config/nextest.toml", ".cargo/config.toml"):
            with self.subTest(path=path):
                self.assertIsNone(TOUCHED.touched(["docs/GUIDE.md", path], self.packages))

    def test_a_package_manifest_is_that_package_not_the_workspace(self):
        self.assertEqual(TOUCHED.touched(["crates/tools/Cargo.toml"], self.packages), set())
        self.assertEqual(TOUCHED.touched(["tools/Cargo.toml"], self.packages), {"tools"})

    def test_selectors_name_each_package_or_the_workspace(self):
        self.assertEqual(TOUCHED.selectors({"top", "base"}), ["-p", "base", "-p", "top"])
        self.assertEqual(TOUCHED.selectors(None), ["--workspace"])
        self.assertEqual(TOUCHED.selectors(set()), [])

    def test_only_a_package_with_a_library_is_selected_for_doctests(self):
        libs = TOUCHED.libraries(METADATA)
        self.assertEqual(libs, {"base", "middle", "top", "tools"}, "a binary has none")
        self.assertEqual(TOUCHED.doc_selectors({"app"}, libs), [], "a binary alone has no doctests")
        self.assertEqual(TOUCHED.doc_selectors({"app", "top"}, libs), ["-p", "top"])
        self.assertEqual(TOUCHED.doc_selectors({"tools"}, libs), ["-p", "tools"])
        self.assertEqual(TOUCHED.doc_selectors(None, libs), ["--workspace"])
        self.assertEqual(TOUCHED.doc_selectors(set(), libs), [])
        bare = json.loads(json.dumps(METADATA))
        for package in bare["packages"]:
            del package["targets"]
        self.assertEqual(TOUCHED.libraries(bare), set(), "no targets listed, no library assumed")


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=True).stdout


class TouchedCratesInGit(unittest.TestCase):
    """What Git reports is read as exact paths, both sides of a move included."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "fixture@example.invalid")
        git(self.repo, "config", "user.name", "Fixture")
        for rel in ("crates/base/src/lib.rs", "crates/top/src/lib.rs", "crates/app/src/main.rs", "docs/note.md"):
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("// one\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "start")
        git(self.repo, "branch", "-q", "base")
        metadata = json.loads(json.dumps(METADATA))
        metadata["workspace_root"] = str(self.repo)
        for package in metadata["packages"]:
            package["manifest_path"] = package["manifest_path"].replace("/work/example", str(self.repo))
        self.metadata_path = self.repo / "metadata.json"
        self.metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
        self.packages = TOUCHED.workspace_packages(metadata)

    def tearDown(self):
        self.tmp.cleanup()

    def run_script(self, *args):
        return subprocess.run(
            [os.sys.executable, "-I", str(ROOT / "scripts/touched_crates.py"), "--base", "base",
             "--metadata", str(self.metadata_path), *args],
            cwd=self.repo, capture_output=True, text=True, check=False,
        )

    def test_a_non_ascii_file_name_is_read_as_the_path_it_is(self):
        (self.repo / "crates/top/src/caf\u00e9.rs").write_text("// two\n", encoding="utf-8")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "accent")
        self.assertEqual(TOUCHED.touched(TOUCHED.changed_paths("base", cwd=self.repo), self.packages), {"top"})

    def test_a_moved_file_touches_its_old_place_and_its_new_one(self):
        git(self.repo, "mv", "crates/base/src/lib.rs", "docs/lib.rs")
        git(self.repo, "commit", "-q", "-m", "move")
        self.assertEqual(
            TOUCHED.touched(TOUCHED.changed_paths("base", cwd=self.repo), self.packages),
            {"base", "middle", "top", "app"},
        )

    def test_an_untracked_file_counts_and_an_ignored_one_does_not(self):
        (self.repo / "crates/top/src/new.rs").write_text("// three\n", encoding="utf-8")
        (self.repo / ".gitignore").write_text("crates/base/target/\n", encoding="utf-8")
        (self.repo / "crates/base/target").mkdir()
        (self.repo / "crates/base/target/out.rs").write_text("// built\n", encoding="utf-8")
        paths = TOUCHED.changed_paths("base", cwd=self.repo)
        self.assertIn("crates/top/src/new.rs", paths)
        self.assertNotIn("crates/base/target/out.rs", paths)

    def test_the_script_writes_selectors_to_a_file_and_fails_loudly(self):
        (self.repo / "crates/top/src/lib.rs").write_text("// changed\n", encoding="utf-8")
        out = self.repo / "selection"
        result = self.run_script("--output", str(out))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(out.read_text(encoding="utf-8"), "-p\ntop\n")
        broken = subprocess.run(
            [os.sys.executable, "-I", str(ROOT / "scripts/touched_crates.py"), "--base", "base",
             "--metadata", str(self.repo / "missing.json")],
            cwd=self.repo, capture_output=True, text=True, check=False,
        )
        self.assertEqual(broken.returncode, 1)
        self.assertIn("could not name the changed packages", broken.stderr)
        self.assertEqual(broken.stdout, "")

    def test_the_doctest_selection_holds_only_touched_packages_with_a_library(self):
        (self.repo / "crates/app/src/main.rs").write_text("// changed\n", encoding="utf-8")
        out, doc = self.repo / "selection", self.repo / "doc-selection"
        result = self.run_script("--output", str(out), "--doc-output", str(doc))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(out.read_text(encoding="utf-8"), "-p\napp\n")
        self.assertEqual(doc.read_text(encoding="utf-8"), "", "the binary alone: no doctest step")
        (self.repo / "crates/top/src/lib.rs").write_text("// changed\n", encoding="utf-8")
        result = self.run_script("--output", str(out), "--doc-output", str(doc))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(out.read_text(encoding="utf-8"), "-p\napp\n-p\ntop\n")
        self.assertEqual(doc.read_text(encoding="utf-8"), "-p\ntop\n")
        (self.repo / "Cargo.lock").write_text("# changed\n", encoding="utf-8")
        result = self.run_script("--output", str(out), "--doc-output", str(doc))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(doc.read_text(encoding="utf-8"), "--workspace\n")



class QuickDoctestStep(unittest.TestCase):
    """`verify.sh quick` runs doctests on the selected packages that have a
    library, and skips the step, saying so, when none has: Cargo refuses
    `--doc` for a selection of binaries alone (a change to the CLI)."""

    def run_quick(self, changed):
        with tempfile.TemporaryDirectory() as folder, tempfile.TemporaryDirectory() as aside:
            root, aside = Path(folder).resolve(), Path(aside).resolve()
            (root / "scripts").mkdir()
            for script in ("verify.sh", "touched_crates.py"):
                shutil.copy2(ROOT / "scripts" / script, root / "scripts" / script)
            (root / "scripts/docs_check.py").write_text("")
            (root / "scripts/build_storage.py").write_text("print('{}')\n")
            (root / "tests").mkdir()
            # The gate ends with the Python suite, which must find a test.
            (root / "tests/test_fixture.py").write_text(
                "import unittest\nclass Fixture(unittest.TestCase):\n    def test_it(self):\n        pass\n")
            for rel in ("crates/base/src/lib.rs", "crates/top/src/lib.rs", "crates/app/src/main.rs"):
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text("// one\n", encoding="utf-8")
            git(root, "init", "-q", "-b", "main")
            git(root, "config", "user.email", "fixture@example.invalid")
            git(root, "config", "user.name", "Fixture")
            git(root, "add", "-A")
            git(root, "commit", "-q", "-m", "start")
            git(root, "update-ref", "refs/remotes/origin/main", "HEAD")
            for rel in changed:
                (root / rel).write_text("// changed\n", encoding="utf-8")
            metadata = json.loads(json.dumps(METADATA).replace("/work/example", str(root)))
            (aside / "metadata.json").write_text(json.dumps(metadata), encoding="utf-8")
            binary = aside / "bin"; binary.mkdir()
            calls = aside / "cargo-calls"
            # A stand-in cargo: answers `metadata` with the fixture, records
            # every call, and succeeds.
            cargo = binary / "cargo"
            cargo.write_text("#!/bin/sh\n"
                             f"echo \"$*\" >> '{calls}'\n"
                             f"[ \"$1\" = metadata ] && cat '{aside / 'metadata.json'}'\n"
                             "exit 0\n")
            cargo.chmod(0o700)
            nextest = binary / "cargo-nextest"; nextest.write_text("#!/bin/sh\n"); nextest.chmod(0o700)
            env = {"PATH": f"{binary}{os.pathsep}{os.environ['PATH']}", "HOME": str(root),
                   "AGENTDOCKER_CAMPAIGN_LEASE": "off"}
            result = subprocess.run(["bash", "scripts/verify.sh", "quick"], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=60)
            return result, calls.read_text().splitlines() if calls.exists() else []

    def test_a_change_to_a_binary_alone_runs_no_doctest_step_and_says_so(self):
        result, calls = self.run_quick(["crates/app/src/main.rs"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("clippy --locked -p app --all-targets -- -D warnings", calls)
        self.assertIn("nextest run --locked -p app --profile ci", calls)
        self.assertFalse([call for call in calls if call.startswith("test")], calls)
        self.assertIn("no selected package has a library target, so there are no doctests to run", result.stderr)

    def test_a_selection_with_a_library_doctests_only_the_library(self):
        result, calls = self.run_quick(["crates/app/src/main.rs", "crates/top/src/lib.rs"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("nextest run --locked -p app -p top --profile ci", calls)
        self.assertIn("test --locked -p top --doc", calls)
        self.assertNotIn("no doctests to run", result.stderr)


if __name__ == "__main__":
    unittest.main()
