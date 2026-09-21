"""Windows archives bind PE architecture and extracted bytes to their provenance."""
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("windows_package_smoke", ROOT / "scripts/windows_package_smoke.py")
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)
PACKAGE = SMOKE.PACKAGE


class WindowsDesktopPackaging(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.binaries = self.root / "compiled"
        self.binaries.mkdir()
        self.target = "x86_64-pc-windows-msvc"
        self.names = PACKAGE.binary_names(self.target)
        # Only PE header validation is exercised by this fixture. Executability
        # is established separately on the native runner's real compiled files.
        header = bytearray(256)
        header[:2] = b"MZ"
        struct.pack_into("<I", header, 60, 64)
        header[64:68] = b"PE\0\0"
        struct.pack_into("<H", header, 68, 0x8664)
        struct.pack_into("<HHH", header, 84, 112, 0x0002, 0x20B)
        for name in self.names:
            (self.binaries / name).write_bytes(header)
        self.manifest = {"format": 1, "source_commit": "a" * 40, "source_tree": "b" * 40,
                         "source_input_sha256": "c" * 64, "source_dirty": False,
                         "version": "0.1.0", "target": self.target, "state_schema": 23,
                         "installation_lock": 1, "launcher_redirect": 1}
        self.save_manifest()
        self.output = self.root / "package"
        self.args = PACKAGE.parser().parse_args([
            "--binary-dir", str(self.binaries), "--output", str(self.output),
            "--source", self.manifest["source_commit"], "--version", "0.1.0", "--target", self.target])

    def save_manifest(self):
        self.manifest["binary_sha256"] = {name: PACKAGE.sha256(self.binaries / name) for name in self.names}
        (self.binaries / "native-build.json").write_text(json.dumps(self.manifest), encoding="utf-8")

    def build(self):
        info = PACKAGE.package(self.args)
        return info, self.output / next(iter(info["artifacts"]))

    def test_portable_archive_contains_only_exact_executables_metadata_and_licenses(self):
        info, archive = self.build()
        self.assertEqual(info["signing"], "unsigned")
        self.assertEqual(info["distribution"], "portable-preview")
        self.assertEqual(info["binary_sha256"], self.manifest["binary_sha256"])
        app = SMOKE.extract_checked(archive, self.root / "unpacked ü folder", info)
        for name in self.names:
            self.assertEqual((app / name).read_bytes(), (self.binaries / name).read_bytes())
        self.assertEqual((app / "licenses/LICENSE-AgentDocker.txt").read_bytes(), (ROOT / "LICENSE").read_bytes())
        self.assertEqual((app / "licenses/LICENSE-Inter.txt").read_bytes(), (ROOT / "crates/ui/src/fonts/LICENSE-Inter.txt").read_bytes())
        self.assertIn("no installer", (app / "README.txt").read_text(encoding="utf-8"))
        self.assertEqual((self.output / (archive.name + ".sha256")).read_text().strip(), info["artifacts"][archive.name] + "  " + archive.name)

    def test_wrong_machine_dll_or_truncated_pe_is_refused_before_publication(self):
        binary = self.binaries / "agentd.exe"
        original = binary.read_bytes()
        for offset, value, fmt in [(68, 0xAA64, "<H"), (86, 0x2002, "<H"),
                                   (86, 0, "<H"), (60, 0xFFFF_FFFF, "<I"),
                                   (84, 0xFFFF, "<H"), (88, 0x10B, "<H"), (64, 0, "<I")]:
            with self.subTest(offset=offset, value=value):
                header = bytearray(original)
                struct.pack_into(fmt, header, offset, value)
                binary.write_bytes(header)
                self.save_manifest()  # Matching hashes must not bypass the PE check.
                with self.assertRaisesRegex(ValueError, "Windows"):
                    PACKAGE.package(self.args)
                self.assertFalse(self.output.exists())
        binary.write_bytes(b"MZ")
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "Windows"):
            PACKAGE.package(self.args)

    def test_substituted_executable_and_wrong_manifest_are_refused(self):
        (self.binaries / "agentdocker.exe").write_bytes(b"replacement")
        with self.assertRaisesRegex(ValueError, "changed after"):
            PACKAGE.package(self.args)
        self.assertFalse(self.output.exists())
        self.manifest["target"] = "x86_64-unknown-linux-gnu"
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "target"):
            PACKAGE.package(self.args)

    def test_replacement_during_copy_cannot_acquire_the_verified_source_provenance(self):
        copy = PACKAGE.shutil.copyfile

        def replace(source, destination, **kwargs):
            result = copy(source, destination, **kwargs)
            if source.name == "agentd.exe":
                # Still a valid x64 executable header, but no longer the build's bytes.
                with destination.open("ab") as stream:
                    stream.write(b"different build")
            return result

        with patch.object(PACKAGE.shutil, "copyfile", side_effect=replace):
            with self.assertRaisesRegex(ValueError, "between build verification and packaging"):
                PACKAGE.package(self.args)
        self.assertFalse(self.output.exists())

    def test_supplied_build_evidence_cannot_be_replaced_by_another_directory_manifest(self):
        for key, value in [("source_input_sha256", "d" * 64),
                           ("binary_sha256", {name: "e" * 64 for name in self.names})]:
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "supplied build evidence"):
                PACKAGE.package(self.args, expected_build={**self.manifest, key: value})
            self.assertFalse(self.output.exists())

    def test_changed_archive_is_refused_before_extraction(self):
        info, archive = self.build()
        with archive.open("ab") as stream:
            stream.write(b"changed")
        destination = self.root / "extract"
        with self.assertRaisesRegex(ValueError, "archive checksum"):
            SMOKE.extract_checked(archive, destination, info)
        self.assertFalse(destination.exists())

    def test_unexpected_archive_member_is_refused_even_with_matching_hash(self):
        info, archive = self.build()
        with zipfile.ZipFile(archive, "a") as bundle:
            bundle.writestr("../outside.exe", "must not be extracted")
        info["artifacts"][archive.name] = PACKAGE.sha256(archive)
        with self.assertRaisesRegex(ValueError, "exact portable layout"):
            SMOKE.extract_checked(archive, self.root / "extract", info)
        self.assertFalse((self.root / "outside.exe").exists())

    def test_archive_metadata_and_executable_hashes_must_match(self):
        info, archive = self.build()
        for key, value in [("state_schema", 99), ("binary_sha256", {name: "d" * 64 for name in self.names})]:
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "metadata differs"):
                SMOKE.extract_checked(archive, self.root / key, {**info, key: value})

    def test_substituted_zip_executable_is_refused_even_with_matching_archive_hash(self):
        info, archive = self.build()
        with zipfile.ZipFile(archive) as bundle:
            members = {name: bundle.read(name) for name in bundle.namelist()}
        members["AgentDocker/agentd.exe"] = b"substituted payload"
        with zipfile.ZipFile(archive, "w") as bundle:
            for name, content in members.items():
                bundle.writestr(name, content)
        info["artifacts"][archive.name] = PACKAGE.sha256(archive)
        with self.assertRaisesRegex(ValueError, "binary checksum differs"):
            SMOKE.extract_checked(archive, self.root / "extract", info)

    def test_mac_only_options_and_existing_outputs_are_refused(self):
        self.args.dmg = True
        with self.assertRaisesRegex(ValueError, "only for macOS"):
            PACKAGE.package(self.args)
        self.args.dmg = False
        self.output.mkdir()
        (self.output / "keep.txt").write_text("previous")
        with self.assertRaises(FileExistsError):
            PACKAGE.package(self.args)
        self.assertEqual((self.output / "keep.txt").read_text(), "previous")


if __name__ == "__main__":
    unittest.main()
