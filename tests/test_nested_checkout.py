"""A checkout nested in another checkout of this repository builds alone.

A project's worktrees live inside it (`.agentdocker/worktrees/`), so every
checkout but the main one has another checkout's files above it. Cargo
looks upward for the workspace a package belongs to: a package the
workspace leaves out, with no workspace of its own, is looked for past the
nested root and found by the outer checkout's workspace, which does not
know it, and nothing that names its manifest runs there.
"""

import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def manifests():
    """Every Cargo manifest git tracks, with what it says."""
    import subprocess
    listed = subprocess.check_output(["git", "ls-files", "-z", "--", "Cargo.toml", "*/Cargo.toml"], cwd=ROOT)
    for name in filter(None, listed.decode().split("\0")):
        yield Path(name), tomllib.loads((ROOT / name).read_text())


class NestedCheckout(unittest.TestCase):
    def test_a_package_outside_the_workspace_is_a_workspace_of_its_own(self):
        found = dict(manifests())
        root = found[Path("Cargo.toml")]["workspace"]
        members = {Path(member) / "Cargo.toml" for member in root["members"]}
        self.assertTrue(members <= set(found), sorted(map(str, members - set(found))))
        outside = sorted(set(found) - members - {Path("Cargo.toml")})
        # The fuzz crate is the one there is; the rule is for any other.
        self.assertIn(Path("fuzz/Cargo.toml"), outside)
        for name in outside:
            self.assertIn(
                "workspace", found[name],
                f"{name} is not a member of the workspace and has no [workspace] table: "
                "from a worktree inside the project Cargo would find the main checkout's workspace for it")
            # Left out of the root workspace by name too, so the root never claims it.
            self.assertIn(name.parent.as_posix(), root.get("exclude", []), name)


if __name__ == "__main__":
    unittest.main()
