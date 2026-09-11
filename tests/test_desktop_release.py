"""Release assets must be installable, complete, and verified before publication."""
import base64
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("desktop_release", ROOT / "packaging/desktop/release.py")
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)


class DesktopRelease(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)

    def test_release_archive_contains_installer_metadata_and_exact_binaries(self):
        binaries = self.root / "bin"
        binaries.mkdir()
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", header, 18, 62)
        package = RELEASE.module("package")
        for name in package.BINARIES:
            (binaries / name).write_bytes(header)
            (binaries / name).chmod(0o755)
        manifest = {"format": 1, "version": "0.2.0", "source_commit": "a" * 40,
                    "source_tree": "b" * 40, "source_input_sha256": "c" * 64,
                    "source_dirty": False, "state_schema": 10, "installation_lock": 1,
                    "target": "x86_64-unknown-linux-gnu", "binary_directory": str(binaries),
                    "binary_sha256": {name: package.sha256(binaries / name) for name in package.BINARIES}}
        native = binaries / "native-build.json"
        native.write_text(json.dumps(manifest))
        output = self.root / "assets"
        RELEASE.prepare(native, output, "v0.2.0", {})
        archive = output / "agentdocker-desktop-x86_64-unknown-linux-gnu.tar.gz"
        with tarfile.open(archive) as bundle:
            metadata = json.load(bundle.extractfile("agentdocker-desktop/build.json"))
            self.assertEqual(metadata["source_commit"], manifest["source_commit"])
            self.assertEqual(metadata["state_schema"], 10)
            for name in package.BINARIES:
                self.assertEqual(bundle.extractfile("agentdocker-desktop/bin/" + name).read(), header)
        self.assertTrue((output / (archive.name + ".sha256")).is_file())
        self.assertTrue((output / "manifest-x86_64-unknown-linux-gnu.json").is_file())
        self.assertEqual(len(list(output.iterdir())), 3)
        with self.assertRaisesRegex(ValueError, "matching clean"):
            RELEASE.prepare(native, self.root / "wrong-version", "v0.3.0", {})
        manifest["source_dirty"] = True
        native.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "matching clean"):
            RELEASE.prepare(native, self.root / "dirty", "v0.2.0", {})

    def release_assets(self, version="0.2.0", signed=True):
        feed = RELEASE.module("feed")
        for target in RELEASE.TARGETS:
            mac = target.endswith("darwin")
            archive = self.root / ("agentdocker-desktop-" + target + (".zip" if mac else ".tar.gz"))
            archive.write_bytes(b"test archive")
            value = {"format": 1, "product": "agentdocker", "version": version, "target": target,
                     "source_commit": "a" * 40, "source_tree": "b" * 40, "source_input_sha256": "c" * 64,
                     "source_dirty": False, "state_schema": 10,
                     "signing": "developer-id" if mac and signed else "local-preview" if mac else "checksum",
                     "notarized": mac and signed, "artifacts": {archive.name: feed.checksum(archive)},
                     "size": {"archive_bytes": {archive.name: archive.stat().st_size}}}
            (self.root / ("manifest-" + target + ".json")).write_text(json.dumps(value))

    def test_complete_feed_checks_bytes_and_preserves_prior_feed_on_failure(self):
        self.release_assets()
        output = self.root / "updates.json"
        value = RELEASE.collect(self.root, output, "v0.2.0")
        self.assertEqual(len(value["releases"]), 4)
        self.assertEqual(value["channel"], "stable")
        before = output.read_bytes()
        archive = next(self.root.glob("*.zip"))
        archive.write_bytes(b"corrupted archive")
        with self.assertRaisesRegex(ValueError, "checksum"):
            RELEASE.collect(self.root, output, "v0.2.0")
        self.assertEqual(output.read_bytes(), before)
        next(self.root.glob("manifest-*.json")).unlink()
        with self.assertRaisesRegex(ValueError, "all four"):
            RELEASE.collect(self.root, output, "v0.2.0")
        self.assertEqual(output.read_bytes(), before)

    def test_unsigned_preview_cannot_replace_stable_feed(self):
        self.release_assets("0.2.0-rc.1", signed=False)
        output = self.root / "updates.json"
        with self.assertRaisesRegex(ValueError, "filename"):
            RELEASE.collect(self.root, output, "v0.2.0-rc.1")
        self.assertFalse(output.exists())
        value = RELEASE.collect(self.root, self.root / "updates-preview.json", "v0.2.0-rc.1")
        self.assertEqual(value["channel"], "preview")
        with self.assertRaises(ValueError):
            RELEASE.preview("v0.2.0+build-1")
        with self.assertRaises(ValueError):
            RELEASE.version("0.2.0")

    def test_maintenance_and_preview_releases_do_not_move_latest(self):
        latest = {"tag_name": "v0.2.0", "draft": False, "prerelease": False}
        self.assertFalse(RELEASE.publication("v0.1.1", latest))
        self.assertFalse(RELEASE.publication("v0.2.0", latest))
        self.assertFalse(RELEASE.publication("v0.3.0-rc.1", latest))
        self.assertTrue(RELEASE.publication("v0.2.1", latest))
        self.assertTrue(RELEASE.publication("v0.1.0", {"status": "404"}))
        with self.assertRaises(ValueError):
            RELEASE.publication("v0.2.1", {"status": "403"})

    def test_signing_requires_complete_credentials_and_cleanup_on_package_failure(self):
        self.assertIsNone(RELEASE.signing_config({}, False))
        with self.assertRaises(ValueError):
            RELEASE.signing_config({}, True)
        with self.assertRaises(ValueError):
            RELEASE.signing_config({"MACOS_NOTARY_KEY": "partial"}, False)
        environment = dict.fromkeys(RELEASE.SIGNING_ENV, "test")
        environment.update(MACOS_CERTIFICATE_BASE64=base64.b64encode(b"fixture cert").decode(),
                           MACOS_SIGNING_IDENTITY="Developer ID Application: Fixture",
                           MACOS_CERTIFICATE_PASSWORD="")
        calls = []
        def command(*arguments):
            calls.append(arguments)
            if arguments == ("security", "default-keychain", "-d", "user"):
                return '"/fixture/login.keychain-db"'
            if arguments == ("security", "list-keychains", "-d", "user"):
                return '"/fixture/login.keychain-db" "/fixture/other.keychain-db"'
            return ""
        with patch.object(RELEASE, "credential_command", side_effect=command):
            with self.assertRaisesRegex(RuntimeError, "package failed"):
                with RELEASE.signing(environment, True) as flags:
                    self.assertIn("--notary-profile", flags)
                    raise RuntimeError("package failed")
        self.assertEqual(calls[-3], ("security", "default-keychain", "-d", "user", "-s", "/fixture/login.keychain-db"))
        self.assertEqual(calls[-2], ("security", "list-keychains", "-d", "user", "-s", "/fixture/login.keychain-db", "/fixture/other.keychain-db"))
        self.assertEqual(calls[-1][:2], ("security", "delete-keychain"))
        self.assertFalse(Path(calls[-1][-1]).parent.exists())

    def test_credential_failure_does_not_expose_command_arguments(self):
        arguments = ("security", "import", "private", "-P", "do-not-log")
        with patch.object(RELEASE.subprocess, "run", side_effect=subprocess.TimeoutExpired(arguments, 120)):
            with self.assertRaises(RuntimeError) as raised:
                RELEASE.credential_command(*arguments)
        self.assertNotIn("do-not-log", str(raised.exception))
