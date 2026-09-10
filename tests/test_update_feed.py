import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("update_feed", ROOT / "packaging/desktop/feed.py")
FEED = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FEED)


class UpdateFeed(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)

    def package(self, target="x86_64-unknown-linux-gnu", **changes):
        root = self.root / target
        root.mkdir()
        archive = root / ("agentdocker-desktop-" + target + (".zip" if target.endswith("darwin") else ".tar.gz"))
        archive.write_bytes(b"verified fixture archive")
        value = {"format": 1, "product": "agentdocker", "version": "0.2.0", "target": target,
                 "source_commit": "a"*40, "source_tree": "b"*40, "source_input_sha256": "c"*64,
                 "source_dirty": False, "state_schema": 8, "signing": "checksum", "notarized": False,
                 "artifacts": {archive.name: FEED.checksum(archive)},
                 "size": {"archive_bytes": {archive.name: archive.stat().st_size}}}
        value.update(changes)
        manifest = root / "manifest.json"
        manifest.write_text(json.dumps(value))
        return manifest, archive

    def test_only_exact_archive_bytes_are_advertised(self):
        manifest, archive = self.package()
        feed = FEED.generate([manifest])
        self.assertEqual(feed["channel"], "stable")
        item = feed["releases"][0]["archive"]
        self.assertEqual(item["sha256"], FEED.checksum(archive))
        self.assertEqual(item["bytes"], archive.stat().st_size)
        archive.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "checksum"):
            FEED.generate([manifest])

    def test_preview_mac_cannot_enter_public_feed(self):
        manifest, _ = self.package("aarch64-apple-darwin", signing="local-preview", source_dirty=True)
        with self.assertRaises(ValueError):
            FEED.generate([manifest])
        self.assertEqual(FEED.generate([manifest], preview=True)["channel"], "preview")
        value = json.loads(manifest.read_text())
        value.update(source_dirty=False, signing="developer-id", notarized="true")
        manifest.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "notarized"):
            FEED.generate([manifest])
        value["notarized"] = True
        manifest.write_text(json.dumps(value))
        self.assertEqual(FEED.generate([manifest])["channel"], "stable")

    def test_mixed_source_schema_and_duplicate_targets_are_refused(self):
        first, _ = self.package()
        second, _ = self.package("aarch64-unknown-linux-gnu", source_commit="d"*40)
        with self.assertRaisesRegex(ValueError, "one source"):
            FEED.generate([first, second])
        value = json.loads(second.read_text())
        value.update(source_commit="a"*40, state_schema=9)
        second.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "one source"):
            FEED.generate([first, second])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            FEED.generate([first, first])
        with self.assertRaises(ValueError):
            FEED.generate([])

    def test_symlink_archive_and_unverified_size_are_refused(self):
        manifest, archive = self.package()
        original = self.root / "original"
        archive.rename(original)
        archive.symlink_to(original)
        with self.assertRaisesRegex(ValueError, "regular desktop"):
            FEED.generate([manifest])
        archive.unlink()
        original.rename(archive)
        value = json.loads(manifest.read_text())
        value["size"]["archive_bytes"][archive.name] += 1
        manifest.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "size"):
            FEED.generate([manifest])
