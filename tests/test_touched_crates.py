"""A targeted run tests the packages a change touches and the packages that
depend on them, and falls back to the whole workspace for a change every
package depends on."""
import importlib.util
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
