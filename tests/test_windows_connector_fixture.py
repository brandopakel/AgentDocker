"""Service fixture records must not become consumable before their ACL is ready."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "windows_connector_fixture", ROOT / "scripts/windows_connector_service_smoke.py")
FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURE)


class ConnectorFixturePublication(unittest.TestCase):
    def test_stop_consumer_cannot_remove_acl_source_before_copy(self):
        for existing in (False, True):
            with self.subTest(existing=existing), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                target, source = root / "stop.json", root / "running.json"
                source.write_text("protected ACL source", encoding="utf-8")
                if existing:
                    target.write_text('{"stale":true}', encoding="utf-8")
                initial = target.read_bytes() if existing else None
                generation = {"pid": 123, "birth": 456}
                copied = []

                def protect(staged):
                    # Model an immediate service stop when the matching record
                    # first becomes visible. The old write-then-ACL order loses
                    # its ACL source here, without relying on scheduler timing.
                    if target.exists() and json.loads(target.read_bytes()) == generation:
                        source.unlink()
                    copied.append(source.read_bytes())
                    self.assertNotEqual(staged, target)
                    self.assertEqual(staged.parent, target.parent)
                    self.assertEqual(json.loads(staged.read_bytes()), generation)
                    self.assertEqual(target.read_bytes() if target.exists() else None, initial)

                FIXTURE.publish_fixture_json(target, generation, protect)
                self.assertEqual(copied, [b"protected ACL source"])
                self.assertEqual(json.loads(target.read_bytes()), generation)
                self.assertEqual(set(root.iterdir()), {target, source})

    def test_failed_protection_or_replace_preserves_target_and_removes_staging(self):
        for failure in ("protection", "replace"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                target = root / "stop.json"
                target.write_bytes(b"original protected record")

                def protect(staged):
                    if failure == "protection":
                        raise PermissionError("ACL copy refused")

                with patch.object(FIXTURE.os, "replace", side_effect=PermissionError("replace refused")):
                    with self.assertRaises(PermissionError):
                        FIXTURE.publish_fixture_json(target, {"pid": 123}, protect)
                self.assertEqual(target.read_bytes(), b"original protected record")
                self.assertEqual(list(root.iterdir()), [target])


if __name__ == "__main__":
    unittest.main()
