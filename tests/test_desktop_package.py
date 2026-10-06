"""Artifact integrity/architecture failures must not replace a prior installable build."""
import importlib.util
import contextlib
import hashlib
import io
import json
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("desktop_package", ROOT / "packaging/desktop/package.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class DesktopPackaging(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        root = Path(self.scratch.name)
        self.binaries, self.output = root / "bin", root / "out"
        self.binaries.mkdir()
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", header, 18, 62)
        for name in PACKAGE.BINARIES:
            path = self.binaries / name
            path.write_bytes(header)
            path.chmod(0o755)
        self.manifest = {"format": 1, "version": "0.1.0", "source_commit": "a" * 40,
                         "source_tree": "b" * 40, "source_input_sha256": "c" * 64,
                         "source_dirty": False, "state_schema": 8, "installation_lock": 1, "launcher_redirect": 1, "target": "x86_64-unknown-linux-gnu",
                         "binary_sha256": {name: PACKAGE.sha256(self.binaries / name) for name in PACKAGE.BINARIES}}
        self.save_manifest()
        self.args = PACKAGE.parser().parse_args(["--binary-dir", str(self.binaries), "--output", str(self.output),
                        "--source", "a" * 40, "--version", "0.1.0", "--target", "x86_64-unknown-linux-gnu"])

    def save_manifest(self):
        (self.binaries / "native-build.json").write_text(json.dumps(self.manifest))

    def test_linux_artifact_contains_launchers_and_exact_binaries(self):
        result = PACKAGE.package(self.args)
        self.assertEqual(result["source_commit"], "a" * 40)
        self.assertEqual(result["binary_sha256"], self.manifest["binary_sha256"])
        archive = self.output / "agentdocker-desktop-x86_64-unknown-linux-gnu.tar.gz"
        with tarfile.open(archive) as bundle:
            names = bundle.getnames()
            self.assertIn("agentdocker-desktop/share/applications/agentdocker.desktop", names)
            self.assertIn("agentdocker-desktop/share/metainfo/dev.agentdocker.desktop.metainfo.xml", names)
            self.assertEqual(
                bundle.extractfile("agentdocker-desktop/share/licenses/agentdocker/LICENSE-Inter.txt").read(),
                (ROOT / "crates/ui/src/fonts/LICENSE-Inter.txt").read_bytes())
            for name in PACKAGE.BINARIES:
                self.assertEqual(bundle.extractfile("agentdocker-desktop/bin/" + name).read(), (self.binaries / name).read_bytes())
            metadata = json.load(bundle.extractfile("agentdocker-desktop/build.json"))
            self.assertEqual(metadata["binary_sha256"], self.manifest["binary_sha256"])
            self.assertEqual(metadata["state_schema"], self.manifest["state_schema"])
            self.assertEqual(metadata["installation_lock"], 1)
            self.assertEqual(metadata["launcher_redirect"], 1)
        self.assertEqual(result["artifacts"][archive.name], PACKAGE.sha256(archive))

    def test_invalid_lifetime_pin_contract_never_publishes(self):
        for pin in [True, "1", -1, 2]:
            self.manifest["installation_lock"] = pin
            self.save_manifest()
            with self.assertRaisesRegex(ValueError, "lifetime pin contract"):
                PACKAGE.package(self.args)
            self.assertFalse(self.output.exists())

    def test_invalid_launcher_redirect_contract_never_publishes(self):
        for redirect in [True, "1", -1, 2]:
            self.manifest["launcher_redirect"] = redirect
            self.save_manifest()
            with self.assertRaisesRegex(ValueError, "launcher redirect contract"):
                PACKAGE.package(self.args)
            self.assertFalse(self.output.exists())

    def test_changed_binary_never_publishes(self):
        (self.binaries / "agentd").write_bytes(b"substituted")
        with self.assertRaisesRegex(ValueError, "changed after"):
            PACKAGE.package(self.args)
        self.assertFalse(self.output.exists())

    def test_oversized_payload_never_publishes(self):
        executable = self.binaries / "agentd"
        with executable.open("ab") as file:
            file.truncate(101 * 1024 ** 2)
        self.manifest["binary_sha256"]["agentd"] = PACKAGE.sha256(executable)
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "payload exceeds"):
            PACKAGE.package(self.args)
        self.assertFalse(self.output.exists())

    def test_oversized_download_is_rejected_even_with_a_small_payload(self):
        archive = self.binaries / "fixture.zip"
        with archive.open("wb") as file:
            file.truncate(41 * 1024 ** 2)
        payload = self.binaries / "app"
        payload.mkdir()
        with self.assertRaisesRegex(ValueError, "download exceeds"):
            PACKAGE.measure_sizes(payload, [archive])

    def test_schema_is_taken_from_the_binary_build_and_required(self):
        self.manifest["state_schema"] = 9
        self.save_manifest()
        self.assertEqual(PACKAGE.package(self.args)["state_schema"], 9)
        del self.manifest["state_schema"]
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "state schema"):
            PACKAGE.validate_inputs(self.args)

    def test_wrong_architecture_never_publishes_even_with_matching_checksum(self):
        self.manifest["target"] = self.args.target = "aarch64-unknown-linux-gnu"
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "architecture"):
            PACKAGE.package(self.args)
        self.assertFalse(self.output.exists())

    def test_previous_output_is_preserved(self):
        self.output.mkdir()
        (self.output / "previous").write_text("preserve me")
        with self.assertRaises(FileExistsError):
            PACKAGE.package(self.args)
        self.assertEqual((self.output / "previous").read_text(), "preserve me")

    def test_schema_boundary_matches_installer(self):
        self.manifest["state_schema"] = 0xFFFF_FFFF
        self.save_manifest()
        self.assertEqual(PACKAGE.package(self.args)["state_schema"], 0xFFFF_FFFF)
        for value in [0x1_0000_0000, 0, -1, True, "8"]:
            with self.subTest(value=value):
                self.manifest["state_schema"] = value
                self.save_manifest()
                with self.assertRaisesRegex(ValueError, "state schema"):
                    PACKAGE.validate_inputs(self.args)

    def test_wrong_source_and_version_are_refused(self):
        for field, value in [("source", "d" * 40), ("version", "0.2.0")]:
            with self.subTest(field=field):
                old = getattr(self.args, field)
                setattr(self.args, field, value)
                with self.assertRaisesRegex(ValueError, "provenance"):
                    PACKAGE.package(self.args)
                setattr(self.args, field, old)

    def test_notarization_cannot_claim_an_ad_hoc_build(self):
        self.args.notary_profile = "local-profile"
        with self.assertRaisesRegex(ValueError, "Developer ID"):
            PACKAGE.validate_inputs(self.args)

    def test_universal_inputs_must_match(self):
        import shutil
        second = self.binaries.parent / "second"
        shutil.copytree(self.binaries, second)
        other = {**self.manifest, "target": "x86_64-apple-darwin", "source_input_sha256": "d" * 64}
        (second / "native-build.json").write_text(json.dumps(other))
        self.args.second_binary_dir = second
        self.args.target = "universal-apple-darwin"
        self.manifest["target"] = "aarch64-apple-darwin"
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "different source"):
            PACKAGE.validate_inputs(self.args)

    def test_distribution_signing_requires_a_clean_source_build(self):
        self.args.target = self.manifest["target"] = "aarch64-apple-darwin"
        self.args.identity = "Developer ID Application: local identity"
        self.manifest["source_dirty"] = True
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, "clean source"):
            PACKAGE.validate_inputs(self.args)


class DiskImageCreation(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        root = Path(self.scratch.name)
        self.image, self.diagnostics = root / 'owned.dmg', root / 'diagnostics.json'
        self.commands = []

    def simulate(self, outcomes, *, verification=((0, ''),), during_delay=None):
        outcomes = iter(outcomes)
        verification = iter(verification)

        def command(args, **kwargs):
            self.commands.append(args)
            if args[1] == 'verify':
                self.assertEqual(self.image.read_bytes(), b'complete')
                outcome = next(verification)
                if isinstance(outcome, Exception):
                    kwargs['stderr'].write(b'verification did not finish\n')
                    raise outcome
                code, error = outcome
                kwargs['stderr'].write(error.encode())
                return subprocess.CompletedProcess(args, code)
            self.assertEqual(kwargs['timeout'], 180)
            self.image.write_bytes(b'partial')
            outcome = next(outcomes)
            if isinstance(outcome, Exception):
                kwargs['stderr'].write(b'creation did not finish\n')
                raise outcome
            code, error = outcome
            kwargs['stderr'].write(error.encode())
            if code == 0:
                self.image.write_bytes(b'complete')
            return subprocess.CompletedProcess(args, code)

        with mock.patch.object(PACKAGE.subprocess, 'run', side_effect=command), \
                mock.patch.object(PACKAGE.time, 'sleep', side_effect=during_delay) as sleep, \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            try:
                return PACKAGE.create_dmg(self.image.parent, self.image, self.diagnostics)
            finally:
                self.delays = [call.args[0] for call in sleep.call_args_list]

    def test_busy_recovery_keeps_failure_and_requires_a_verified_image(self):
        result = self.simulate([(1, 'hdiutil: create failed - Resource busy\n'), (0, '')])
        self.assertEqual(result, {'attempts': 2, 'verification_attempts': 1, 'recovered': True})
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'passed')
        self.assertTrue(report['verified'])
        self.assertEqual([v['exit_code'] for v in report['attempts']], [1, 0])
        self.assertIn('Resource busy', report['attempts'][0]['stderr_tail'])
        self.assertEqual(self.delays, [2])
        self.assertEqual([args[1] for args in self.commands], ['create', 'create', 'verify'])

    def test_other_errors_stop_even_after_a_prior_busy_attempt(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.simulate([(1, 'hdiutil: create failed - Resource busy\n'),
                           (1, 'hdiutil: create failed - Permission denied\n')])
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'failed')
        self.assertEqual(len(report['attempts']), 2)
        self.assertEqual(self.image.read_bytes(), b'partial')
        self.assertEqual(self.delays, [2])

    def test_persistent_busy_failure_is_bounded_and_partial_image_is_not_success(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.simulate([(1, 'hdiutil: create failed - Resource busy\n')] * 3)
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'failed')
        self.assertEqual(len(report['attempts']), 3)
        self.assertEqual(self.delays, [2, 4])
        self.assertEqual(self.image.read_bytes(), b'partial')

    def test_timeout_is_retained_and_never_retried(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            self.simulate([subprocess.TimeoutExpired(['hdiutil'], 180)])
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'failed')
        self.assertEqual(len(report['attempts']), 1)
        self.assertTrue(report['attempts'][0]['timed_out'])
        self.assertEqual(self.delays, [])

    def test_failed_integrity_verification_keeps_the_failure(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.simulate([(0, '')], verification=[(1, 'checksum mismatch\n')])
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'failed')
        self.assertIn('CalledProcessError', report['verification'])
        self.assertEqual(report['verification_attempts'][0]['stderr_tail'], 'checksum mismatch\n')
        self.assertEqual(self.delays, [])

    def test_unavailable_verification_retries_same_image_and_preserves_diagnostics(self):
        error = 'hdiutil: verify failed - Resource temporarily unavailable\n'
        result = self.simulate([(0, '')], verification=[(1, error), (0, '')])
        self.assertEqual(result, {'attempts': 1, 'verification_attempts': 2, 'recovered': True})
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['image_sha256'], hashlib.sha256(b'complete').hexdigest())
        self.assertEqual([v['exit_code'] for v in report['verification_attempts']], [1, 0])
        self.assertEqual(report['verification_attempts'][0]['stderr_tail'], error)
        self.assertEqual([args[1] for args in self.commands], ['create', 'verify', 'verify'])
        self.assertEqual(self.delays, [2])
        self.assertTrue(report['verified'])

    def test_persistent_unavailable_verification_stops_after_three_attempts(self):
        error = 'hdiutil: verify failed - Resource temporarily unavailable\n'
        with self.assertRaises(subprocess.CalledProcessError):
            self.simulate([(0, '')], verification=[(1, error)] * 3)
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'failed')
        self.assertNotIn('verified', report)
        self.assertEqual(len(report['verification_attempts']), 3)
        self.assertEqual(self.delays, [2, 4])

    def test_verification_recovery_never_retries_a_later_checksum_error(self):
        error = 'hdiutil: verify failed - Resource temporarily unavailable\n'
        with self.assertRaises(subprocess.CalledProcessError):
            self.simulate([(0, '')], verification=[(1, error), (1, 'checksum mismatch\n')])
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'failed')
        self.assertEqual(len(report['verification_attempts']), 2)
        self.assertEqual(self.delays, [2])

    def test_verification_timeout_is_retained_without_retry(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            self.simulate([(0, '')], verification=[subprocess.TimeoutExpired(['hdiutil'], 180)])
        report = json.loads(self.diagnostics.read_text())
        self.assertTrue(report['verification_attempts'][0]['timed_out'])
        self.assertEqual(report['result'], 'failed')
        self.assertEqual(self.delays, [])

    def test_verification_launch_failure_is_retained_without_retry(self):
        with self.assertRaises(FileNotFoundError):
            self.simulate([(0, '')], verification=[FileNotFoundError('verify unavailable')])
        report = json.loads(self.diagnostics.read_text())
        self.assertIn('verify unavailable', report['verification_attempts'][0]['launch_error'])
        self.assertEqual(report['result'], 'failed')
        self.assertEqual(self.delays, [])

    def test_image_change_during_backoff_prevents_another_verification(self):
        error = 'hdiutil: verify failed - Resource temporarily unavailable\n'
        with self.assertRaisesRegex(RuntimeError, 'image changed'):
            self.simulate([(0, '')], verification=[(1, error)],
                          during_delay=lambda _: self.image.write_bytes(b'changed'))
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'failed')
        self.assertEqual(len(report['verification_attempts']), 1)
        self.assertEqual([args[1] for args in self.commands], ['create', 'verify'])

    def test_verification_retry_requires_the_exact_error_and_exit_status(self):
        for code, error in [(2, 'hdiutil: verify failed - Resource temporarily unavailable\n'),
                            (1, 'some other resource is temporarily unavailable\n')]:
            with self.subTest(code=code, error=error):
                self.diagnostics.unlink(missing_ok=True)
                with self.assertRaises(subprocess.CalledProcessError):
                    self.simulate([(0, '')], verification=[(code, error)])
                self.assertEqual(self.delays, [])

    def test_existing_diagnostics_are_never_replaced(self):
        self.diagnostics.write_text('prior failure evidence')
        with self.assertRaises(FileExistsError):
            self.simulate([])
        self.assertEqual(self.diagnostics.read_text(), 'prior failure evidence')
        self.assertEqual(self.commands, [])

    def test_launch_failure_is_retained_without_retry(self):
        with self.assertRaises(FileNotFoundError):
            self.simulate([FileNotFoundError('hdiutil unavailable')])
        report = json.loads(self.diagnostics.read_text())
        self.assertEqual(report['result'], 'failed')
        self.assertEqual(len(report['attempts']), 1)
        self.assertIn('hdiutil unavailable', report['attempts'][0]['launch_error'])
        self.assertEqual(self.delays, [])


if __name__ == "__main__":
    unittest.main()
