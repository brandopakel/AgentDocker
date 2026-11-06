"""A targeted run tests the packages a change touches and the packages that
depend on them, and falls back to the whole workspace for a change every
package depends on."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("touched_crates", ROOT / "scripts/touched_crates.py")
TOUCHED = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOUCHED)

# A made-up workspace: `base` at the bottom, `middle` on it, `top` and `app`
# on `middle`, `tools` on nothing.
METADATA = {
    "workspace_root": "/work/example",
    "workspace_members": ["base 0.1.0", "middle 0.1.0", "top 0.1.0", "app 0.1.0", "tools 0.1.0", "other 1.0.0"],
    "packages": [
        {"id": "base 0.1.0", "name": "base", "manifest_path": "/work/example/crates/base/Cargo.toml", "dependencies": []},
        {"id": "middle 0.1.0", "name": "middle", "manifest_path": "/work/example/crates/middle/Cargo.toml",
         "dependencies": [{"name": "base"}, {"name": "serde"}]},
        {"id": "top 0.1.0", "name": "top", "manifest_path": "/work/example/crates/top/Cargo.toml",
         "dependencies": [{"name": "middle"}]},
        {"id": "app 0.1.0", "name": "app", "manifest_path": "/work/example/crates/app/Cargo.toml",
         "dependencies": [{"name": "middle"}, {"name": "base"}]},
        {"id": "tools 0.1.0", "name": "tools", "manifest_path": "/work/example/tools/Cargo.toml", "dependencies": []},
    ],
}


class TouchedCrates(unittest.TestCase):
    def setUp(self):
        self.packages = TOUCHED.workspace_packages(METADATA)

    def test_packages_come_with_their_directories_and_workspace_dependencies(self):
        self.assertEqual(self.packages["middle"], ("crates/middle/", {"base"}))
        self.assertEqual(self.packages["tools"], ("tools/", set()))
        self.assertNotIn("other", self.packages)

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
        for rel in ("crates/base/src/lib.rs", "crates/top/src/lib.rs", "docs/note.md"):
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


if __name__ == "__main__":
    unittest.main()
