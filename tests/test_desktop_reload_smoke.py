"""The desktop reload smoke's pin trial makes each installation generation once."""
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
DESKTOP_SPEC = importlib.util.spec_from_file_location("desktop_reload_smoke", ROOT / "scripts/desktop_reload_smoke.py")
DESKTOP = importlib.util.module_from_spec(DESKTOP_SPEC)
DESKTOP_SPEC.loader.exec_module(DESKTOP)


class DesktopPinGenerations(unittest.TestCase):
    def test_linux_pin_trial_creates_each_generation_once(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            (source / "build.json").write_text(json.dumps({"version": "fixture"}))
            made = []

            def pin_trial(args, trial_root, prefix, payload, controller, environment, cli, result):
                for generation in (2, 3):
                    copied = DESKTOP.second_generation(payload, trial_root, generation)
                    made.append(json.loads((copied / "build.json").read_text())["installation_acceptance_generation"])

            args = SimpleNamespace(source=source, output=root / "output", pin_trial=True)
            with patch.multiple(DESKTOP, MAC=False, PAYLOAD="agentdocker-desktop", BIN=Path("bin"), META=Path("build.json")), patch.object(DESKTOP, "pin_trial", side_effect=pin_trial):
                DESKTOP.trial(args)
            self.assertEqual(made, [2, 3])
            self.assertTrue(json.loads((args.output / "result.json").read_text())["passed"])
