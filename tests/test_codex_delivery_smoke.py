"""A provider acceptance fixture must not trust unrelated user hooks/plugins."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("codex_delivery_smoke", ROOT / "scripts/codex_delivery_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


@unittest.skipUnless(os.name == "posix", "Unix provider fixture")
class IsolatedProfile(unittest.TestCase):
    def test_auth_and_cached_plugin_sources_are_left_untouched(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            auth = root / "auth.json"
            auth.write_text("private fixture, not a real credential")
            (root / "plugins/cache").mkdir(parents=True)
            (root / "plugins/.remote-plugin-install-staging").mkdir()
            cached = root / "plugins/cache/hooks.json"
            cached.write_text("cached plugin remains disabled by invocation flags")
            self.assertEqual(SMOKE.checked_provider_home(root), root.resolve())
            self.assertEqual(auth.read_text(), "private fixture, not a real credential")
            self.assertEqual(cached.read_text(), "cached plugin remains disabled by invocation flags")

    def test_existing_configuration_and_hook_sources_are_refused_not_removed(self):
        for entry in ["config.toml", "hooks.json", "AGENTS.md", "AGENTS.override.md"]:
            with self.subTest(entry=entry), tempfile.TemporaryDirectory() as name:
                root = Path(name)
                target = root / entry
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("leave this source untouched")
                with self.assertRaises(ValueError):
                    SMOKE.checked_provider_home(root)
                self.assertEqual(target.read_text(), "leave this source untouched")

    def test_refused_profile_records_failure_before_any_child_launch(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            (root / "hooks.json").write_text("unreviewed hook")
            args = SimpleNamespace(output=root / "result", binary_dir=root / "bin",
                                   provider_home=root)
            original_umask = os.umask(0o077)
            try:
                with patch.object(SMOKE.subprocess, "Popen", side_effect=AssertionError("child launched")):
                    with patch("builtins.print"):
                        self.assertEqual(SMOKE.trial(args), 1)
            finally:
                os.umask(original_umask)
            report = json.loads((args.output / "result.json").read_text())
            self.assertFalse(report["profile_preflight_passed"])
            self.assertIsNone(report["provider_configuration_unchanged"])
            self.assertIn("hooks.json", report["error"])
            self.assertEqual(report["owned_children_remaining"], 0)
            self.assertEqual((root / "hooks.json").read_text(), "unreviewed hook")

    def test_symlink_sources_and_shared_profile_are_refused(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            link = root / "profile-link"
            link.symlink_to(root)
            with self.assertRaises(ValueError):
                SMOKE.checked_provider_home(link)
            link.unlink()
            (root / "hooks.json").symlink_to(root / "missing-hooks")
            with self.assertRaises(ValueError):
                SMOKE.checked_provider_home(root)
            (root / "hooks.json").unlink()
            root.chmod(0o750)
            try:
                with self.assertRaises(ValueError):
                    SMOKE.checked_provider_home(root)
            finally:
                root.chmod(0o700)


if __name__ == "__main__":
    unittest.main()
