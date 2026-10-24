"""A package built in the checkout must never answer for the installed app."""
from pathlib import Path
import shutil
import subprocess
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

    def test_cleaning_forgets_and_removes_the_old_location_too(self):
        commands = recipe("clean-artifacts").splitlines()
        self.assertIn("artifacts/local/desktop-*/", commands[0])
        self.assertIn(f'"{LSREGISTER}" -u', commands[0])
        self.assertEqual(commands[1], 'rm -rf "artifacts/local.noindex" artifacts/local')


if __name__ == "__main__":
    unittest.main()
