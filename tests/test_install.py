"""Exercise install.sh with local archive fixtures; no network or user install."""
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zlib

ROOT = Path(__file__).resolve().parents[1]

# Stands in for the download: serves the checksum file and the archive
# from whatever the environment points at.
CURL_STUB = """#!/bin/sh
for arg in "$@"; do
  case "$arg" in https://*) url="$arg";; esac
  if [ "${previous:-}" = "-o" ]; then output="$arg"; fi
  previous="$arg"
done
case "$url" in
  *.sha256) [ "$TEST_MODE" != missing ] || exit 22; cp "$TEST_CHECKSUM" "$output" ;;
  *) cp "$TEST_ARCHIVE" "$output" ;;
esac
"""


def _read_png(path):
    """Rows of RGBA bytes from one of our own PNGs.

    Only handles what `scripts/icon.py` writes — 8-bit RGBA, one IDAT,
    no row filtering — which is the point: a decoder that accepts more
    would also accept a file the icon script never produces.
    """
    raw = path.read_bytes()
    pos, size, idat = 8, 0, b""
    while pos < len(raw):
        length = struct.unpack(">I", raw[pos : pos + 4])[0]
        kind = raw[pos + 4 : pos + 8]
        body = raw[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            size = struct.unpack(">I", body[:4])[0]
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    data = zlib.decompress(idat)
    stride = size * 4
    rows = []
    for y in range(size):
        start = y * (stride + 1)
        assert data[start] == 0, "the icon script writes unfiltered rows"
        rows.append(data[start + 1 : start + 1 + stride])
    return rows, size


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
                curl.write_text(CURL_STUB)
                curl.chmod(0o755)
                install = root / "installed"
                install.mkdir()
                (install / "agentd").write_text("existing")
                env = dict(os.environ, PATH=str(mock) + ":" + os.environ["PATH"], TEST_MODE=mode,
                           TEST_CHECKSUM=str(checksums), TEST_ARCHIVE=str(archive), AGENTDOCKER_INSTALL_DIR=str(install))
                result = subprocess.run(["sh", str(ROOT / "install.sh")], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, mode == "valid", result.stderr)
                self.assertEqual((install / "agentd").read_text(), "#!/bin/sh\nexit 0\n" if mode == "valid" else "existing")

    def test_macos_archive_installs_the_app_bundle(self):
        """The bundle is what macOS reads the name and the icon from, so an
        archive that carries one must end up with it in Applications — and
        an archive that does not must still install cleanly."""
        for carries_bundle in [True, False]:
            with self.subTest(bundle=carries_bundle), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                archive = root / "fixture.tar.gz"
                with tarfile.open(archive, "w:gz") as tar:
                    for name in ["agentd", "agentdocker", "agentdocker-ui"]:
                        data = b"#!/bin/sh\nexit 0\n"
                        info = tarfile.TarInfo(name)
                        info.size = len(data)
                        tar.addfile(info, io.BytesIO(data))
                    if carries_bundle:
                        plist = b"<plist></plist>\n"
                        for name, data in [
                            ("AgentDocker.app/Contents/Info.plist", plist),
                            ("AgentDocker.app/Contents/MacOS/AgentDocker", b"#!/bin/sh\nexit 0\n"),
                        ]:
                            info = tarfile.TarInfo(name)
                            info.size = len(data)
                            tar.addfile(info, io.BytesIO(data))
                checksums = root / "checksum"
                checksums.write_text(
                    hashlib.sha256(archive.read_bytes()).hexdigest() + "  fixture.tar.gz\n"
                )
                mock = root / "mock"
                mock.mkdir()
                curl = mock / "curl"
                curl.write_text(CURL_STUB)
                curl.chmod(0o755)
                install = root / "installed"
                install.mkdir()
                # A home of its own: the installer writes into
                # $HOME/Applications and a test must not touch the real one.
                home = root / "home"
                home.mkdir()
                env = dict(
                    os.environ,
                    PATH=str(mock) + ":" + os.environ["PATH"],
                    TEST_MODE="valid",
                    TEST_CHECKSUM=str(checksums),
                    TEST_ARCHIVE=str(archive),
                    AGENTDOCKER_INSTALL_DIR=str(install),
                    HOME=str(home),
                )
                result = subprocess.run(
                    ["sh", str(ROOT / "install.sh")], env=env, capture_output=True, text=True
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                bundled = home / "Applications/AgentDocker.app/Contents/MacOS/AgentDocker"
                self.assertEqual(bundled.exists(), carries_bundle, result.stdout)
                self.assertTrue((install / "agentdocker-ui").exists(), result.stdout)

    def test_the_mark_is_present_and_square(self):
        """The icon is composed around artwork now, not drawn, so the
        artwork has to be there and has to be the shape the pipeline
        assumes: square, so centring it needs no judgement."""
        mark = ROOT / "docs/images/agentdocker-mark.png"
        head = mark.read_bytes()[:24]
        self.assertEqual(head[:8], b"\x89PNG\r\n\x1a\n")
        width, height = struct.unpack(">II", head[16:24])
        self.assertEqual(width, height, "the mark is square")
        self.assertGreaterEqual(width, 512, "large enough for a 1024 icon")

    def test_icon_renders_every_size_the_iconset_needs(self):
        """The icon has a source rather than a stored file, so the source
        has to actually produce one. A PNG here is checked by its header
        and its declared dimensions, which is what `iconutil` reads."""
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(
                ["python3", str(ROOT / "scripts/icon.py"), tmp],
                check=True,
                capture_output=True,
            )
            iconset = Path(tmp) / "AgentDocker.iconset"
            names = sorted(p.name for p in iconset.iterdir())
            self.assertEqual(len(names), 10, names)
            for png in list(iconset.iterdir()) + [Path(tmp) / "icon-1024.png"]:
                head = png.read_bytes()[:24]
                self.assertEqual(head[:8], b"\x89PNG\r\n\x1a\n", png.name)
                width, height = struct.unpack(">II", head[16:24])
                self.assertEqual(width, height, png.name)
                self.assertIn(width, {16, 32, 64, 128, 256, 512, 1024}, png.name)

    def test_icon_sits_on_apples_grid(self):
        """macOS sizes every Dock icon against a 1024pt canvas whose body
        is 824pt square and centred. An icon drawn edge to edge is not
        rejected by anything — it just renders about a quarter larger
        than everything beside it, which is why this is asserted rather
        than eyeballed."""
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(
                ["python3", str(ROOT / "scripts/icon.py"), tmp],
                check=True,
                capture_output=True,
            )
            pixels, size = _read_png(Path(tmp) / "icon-1024.png")
            self.assertEqual(size, 1024)

            def alpha(x, y):
                return pixels[y][x * 4 + 3]

            middle = size // 2
            # 100pt of margin: the corner and the outer band are empty.
            for x, y in [(0, 0), (size - 1, 0), (0, size - 1), (40, middle), (middle, 40)]:
                self.assertEqual(alpha(x, y), 0, f"margin at {x},{y} is not clear")
            # And the body fills what is left, edge and centre alike.
            for x, y in [(middle, middle), (120, middle), (size - 120, middle)]:
                self.assertEqual(alpha(x, y), 255, f"body at {x},{y} is not solid")

    def test_the_smallest_icon_is_given_more_of_the_tile(self):
        """At 16pt the body is thirteen pixels across. Every pixel spent
        on breathing room is one not spent on the shape, and what
        survives of a detailed mark at that size is a smudge either way —
        so the small sizes fill the tile and the large ones keep their
        margin. The check is that they really are laid out differently
        rather than being one drawing resampled."""
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(
                ["python3", str(ROOT / "scripts/icon.py"), tmp],
                check=True,
                capture_output=True,
            )
            iconset = Path(tmp) / "AgentDocker.iconset"

            def mark_share(name, size):
                """How much of the canvas the mark itself covers.

                The mark is the blue; the tile behind it is nearly black.
                Anything with a blue channel well above the ground is the
                artwork rather than the tile it sits on.
                """
                rows, _ = _read_png(iconset / name)
                blue = sum(
                    1
                    for y in range(size)
                    for x in range(size)
                    if rows[y][x * 4 + 3] > 200 and rows[y][x * 4 + 2] > 90
                )
                return blue / (size * size)

            small = mark_share("icon_16x16.png", 16)
            large = mark_share("icon_512x512.png", 512)
            self.assertGreater(large, 0.05, "the mark is on the large tile at all")
            self.assertGreater(
                small,
                large * 1.15,
                f"16pt gives the mark more room: {small:.3f} against {large:.3f}",
            )

    @unittest.skipUnless(sys.platform == "darwin", "iconutil is macOS-only")
    def test_bundle_names_the_app_for_macos(self):
        """A bare executable is named after its file in the Dock and the
        app switcher. The bundle is the only thing that changes that, so
        what it says about itself is worth asserting."""
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "agentdocker-ui"
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            out = Path(tmp) / "dist"
            out.mkdir()
            subprocess.run(
                ["sh", str(ROOT / "scripts/bundle-macos.sh"), str(binary), str(out), "9.9.9"],
                check=True,
                capture_output=True,
            )
            app = out / "AgentDocker.app"
            plist = (app / "Contents/Info.plist").read_text()
            for key in ["<string>AgentDocker</string>", "<string>9.9.9</string>"]:
                self.assertIn(key, plist)
            self.assertTrue((app / "Contents/MacOS/AgentDocker").exists())
            self.assertEqual((app / "Contents/PkgInfo").read_text(), "APPL????")
            self.assertTrue((app / "Contents/Resources/AgentDocker.icns").exists())

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
