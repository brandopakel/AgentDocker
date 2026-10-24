"""A package built in the checkout must never answer for the installed app."""
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
LSREGISTER = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"


def recipe(target, *variables):
    """The commands `make -n` would run, read without building anything."""
    return subprocess.run(
        ["make", "-n", "--no-print-directory", target, *variables],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout


@unittest.skipUnless(shutil.which("make"), "make is not installed")
class PackagedApp(unittest.TestCase):
    def test_packages_go_where_spotlight_does_not_index(self):
        self.assertIn('--output "artifacts/local.noindex/desktop-', recipe("app"))

    def test_packaging_and_installing_each_forget_the_package_afterwards(self):
        for target, after in [("app", "Packaged "), ("install", " install --from "), ("install-preview", "--preview")]:
            with self.subTest(target=target):
                commands = recipe(target, "ARTIFACTS=/tmp/out").splitlines()
                forgets = [i for i, line in enumerate(commands) if f'"{LSREGISTER}" -u' in line]
                self.assertTrue(forgets, commands)
                last = max(i for i, line in enumerate(commands) if after in line)
                self.assertGreater(forgets[-1], last, "the package is forgotten after it last ran")
                self.assertIn("/tmp/out/desktop-", commands[forgets[-1]])

    def test_packaging_first_moves_old_packages_out_of_spotlights_way(self):
        commands = recipe("app").splitlines()
        migrates = next(i for i, line in enumerate(commands) if '[ -d "artifacts/local" ]' in line)
        packages = next(i for i, line in enumerate(commands) if "packaging/desktop/package.py" in line)
        self.assertLess(migrates, packages, "old packages move before a new one is made")
        payload = "AgentDocker.app" if platform.system() == "Darwin" else "agentdocker-desktop"
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            calls = tmp / "calls"
            fake = tmp / "lsregister"
            fake.write_text(f'#!/bin/sh\necho "$@" >> "{calls}"\n')
            fake.chmod(0o755)
            checkout = tmp / "checkout"
            legacy = checkout / "artifacts/local"
            for stamp in ["desktop-1", "desktop-2"]:
                (legacy / stamp / payload / "Contents").mkdir(parents=True)
            (legacy / "smoke-1").mkdir()
            (legacy / "latest").symlink_to("desktop-2")
            # A name already in the new folder is left where it is.
            (checkout / "artifacts/local.noindex/desktop-2").mkdir(parents=True)

            def migrate():
                return subprocess.run(
                    ["make", "-f", str(ROOT / "Makefile"), "--no-print-directory",
                     "migrate-artifacts", f"LSREGISTER={fake}"],
                    cwd=checkout, check=True, capture_output=True, text=True,
                ).stdout

            self.assertIn("Left artifacts/local", migrate())
            self.assertEqual(
                sorted(calls.read_text().splitlines()),
                [f"-u artifacts/local/desktop-1/{payload}", f"-u artifacts/local/desktop-2/{payload}"],
            )
            moved = checkout / "artifacts/local.noindex"
            self.assertTrue((moved / "desktop-1" / payload).is_dir())
            self.assertTrue((moved / "smoke-1").is_dir())
            self.assertFalse((moved / "desktop-2" / payload).exists())
            self.assertEqual(sorted(os.listdir(legacy)), ["desktop-2"])

            # Once the old folder is empty it goes, naming nothing to Launch
            # Services, and a later run does nothing.
            shutil.rmtree(legacy / "desktop-2")
            calls.unlink()
            migrate()
            self.assertFalse(legacy.exists())
            migrate()
            self.assertFalse(calls.exists())

    def test_cleaning_forgets_and_removes_the_old_location_too(self):
        commands = recipe("clean-artifacts").splitlines()
        self.assertIn('"artifacts/local"/desktop-*/', commands[0])
        self.assertIn(f'"{LSREGISTER}" -u', commands[0])
        self.assertEqual(commands[1], 'rm -rf "artifacts/local.noindex" "artifacts/local"')


if __name__ == "__main__":
    unittest.main()
