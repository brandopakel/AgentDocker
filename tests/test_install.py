"""Exercise install.sh with local archive fixtures; no network or user install."""
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class InstallerTests(unittest.TestCase):
    def test_checksum_controls_installation(self):
        for mode in ["valid", "mismatch", "missing", "malformed"]:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                archive = root / "fixture.tar.gz"
                with tarfile.open(archive, "w:gz") as tar:
                    for name in ["agentd", "agentdocker"]:
                        data = b"#!/bin/sh\nexit 0\n"
                        info = tarfile.TarInfo(name)
                        info.size = len(data)
                        tar.addfile(info, io.BytesIO(data))
                checksums = root / "checksum"
                digest = hashlib.sha256(archive.read_bytes()).hexdigest()
                checksums.write_text((digest if mode == "valid" else "0" * 64 if mode == "mismatch" else "broken") + "  fixture.tar.gz\n")
                mock = root / "mock"
                mock.mkdir()
                curl = mock / "curl"
                curl.write_text('''#!/bin/sh
for arg in "$@"; do
  case "$arg" in https://*) url="$arg";; esac
  if [ "${previous:-}" = "-o" ]; then output="$arg"; fi
  previous="$arg"
done
case "$url" in
  *.sha256) [ "$TEST_MODE" != missing ] || exit 22; cp "$TEST_CHECKSUM" "$output" ;;
  *) cp "$TEST_ARCHIVE" "$output" ;;
esac
''')
                curl.chmod(0o755)
                install = root / "installed"
                install.mkdir()
                (install / "agentd").write_text("existing")
                env = dict(os.environ, PATH=str(mock) + ":" + os.environ["PATH"], TEST_MODE=mode,
                           TEST_CHECKSUM=str(checksums), TEST_ARCHIVE=str(archive), AGENTDOCKER_INSTALL_DIR=str(install))
                result = subprocess.run(["sh", str(ROOT / "install.sh")], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, mode == "valid", result.stderr)
                self.assertEqual((install / "agentd").read_text(), "#!/bin/sh\nexit 0\n" if mode == "valid" else "existing")

    def test_formula_requires_real_hashes_for_every_target(self):
        spec = importlib.util.spec_from_file_location("formula", ROOT / "packaging/homebrew/generate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(FileNotFoundError):
                module.generate("v0.1.0", root)
            for target in ["aarch64-apple-darwin", "x86_64-apple-darwin", "aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"]:
                (root / f"agentdocker-{target}.tar.gz.sha256").write_text("a" * 64)
            text = module.generate("v0.1.0", root)
            self.assertNotIn("@SHA_", text)
            self.assertIn('version "0.1.0"', text)
            with self.assertRaises(ValueError):
                module.generate('v0.1.0"; bad', root)
