#!/usr/bin/env python3
"""Build native desktop artifacts from binaries produced by the same checkout.

Build tooling only: opening/installing the app does not require Python or Rust.
Credentials are referenced through local signing identities/keychain profiles.
"""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
BINARIES = ("agentdocker", "agentd", "agentdocker-ui")


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def validate_inputs(args):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9._-]+)?", args.version):
        raise ValueError("version must be a semantic version")
    if not re.fullmatch(r"[0-9a-f]{40}", args.source):
        raise ValueError("source must be a full commit SHA")
    supported = {
        "aarch64-apple-darwin", "x86_64-apple-darwin", "universal-apple-darwin",
        "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    }
    if args.target not in supported:
        raise ValueError("unsupported desktop target")
    if args.target.startswith("universal-") != bool(args.second_binary_dir):
        raise ValueError("universal Mac packaging requires exactly two binary directories")
    if args.notary_profile and (not args.identity or args.identity == "-"):
        raise ValueError("notarization requires a Developer ID Application identity")
    if args.identity and args.identity != "-" and "apple-darwin" not in args.target:
        raise ValueError("Developer ID signing is only for macOS")
    if args.build_number < 1:
        raise ValueError("build number must be positive")
    manifests = []
    for directory in filter(None, [args.binary_dir, args.second_binary_dir]):
        manifest = json.loads((directory / "native-build.json").read_text())
        if manifest.get("format") != 1 or manifest.get("source_commit") != args.source or manifest.get("version") != args.version:
            raise ValueError("build provenance does not match the requested source/version")
        if type(manifest.get("state_schema")) is not int or not 1 <= manifest["state_schema"] <= 0xFFFF_FFFF:
            raise ValueError("build provenance lacks the daemon state schema; rebuild the binaries")
        pin = manifest.get("installation_lock", 0)
        if type(pin) is not int or pin not in (0, 1):
            raise ValueError("invalid desktop lifetime pin contract")
        if args.identity and args.identity != "-" and manifest.get("source_dirty"):
            raise ValueError("distribution signing requires a clean source build")
        expected_target = args.target
        if args.second_binary_dir:
            expected_target = "aarch64-apple-darwin" if directory == args.binary_dir else "x86_64-apple-darwin"
        if manifest.get("target") != expected_target:
            raise ValueError("build provenance target does not match the package")
        manifests.append(manifest)
        for name in BINARIES:
            path = directory / name
            if not path.is_file() or path.is_symlink() or not path.stat().st_mode & 0o111:
                raise ValueError(f"missing executable regular binary: {path}")
            if sha256(path) != manifest.get("binary_sha256", {}).get(name):
                raise ValueError(f"binary changed after its verified build: {name}")
    if len({m.get("source_input_sha256") for m in manifests}) != 1:
        raise ValueError("universal binaries were built from different source inputs")
    if len({m["state_schema"] for m in manifests}) != 1:
        raise ValueError("universal binaries have different state schemas")
    if len({m.get("installation_lock", 0) for m in manifests}) != 1:
        raise ValueError("universal binaries have different lifetime pin contracts")
    return manifests[0]


def metadata(args):
    return {
        "format": 1, "product": "agentdocker", "version": args.version,
        "source_commit": args.source, "target": args.target,
        "display_name": "agentdocker",
        "signing": "developer-id" if args.identity and args.identity != "-" else "local-preview",
        "notarized": False,
    }


def copy_binaries(args, destination):
    destination.mkdir(parents=True)
    for name in BINARIES:
        source = args.binary_dir / name
        target = destination / name
        if args.second_binary_dir:
            run("/usr/bin/lipo", "-create", source, args.second_binary_dir / name, "-output", target)
        else:
            shutil.copyfile(source, target)
        target.chmod(0o755)
        if "linux" in args.target:
            with target.open("rb") as binary:
                header = binary.read(64)
            expected = 62 if args.target.startswith("x86_64-") else 183
            if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", header, 18)[0] != expected:
                raise ValueError(f"{name} does not match the declared Linux architecture")
        else:
            actual = set(subprocess.check_output(["/usr/bin/lipo", "-archs", str(target)], text=True).split())
            expected = {"arm64", "x86_64"} if args.second_binary_dir else {
                "arm64" if args.target.startswith("aarch64-") else "x86_64"}
            if actual != expected:
                raise ValueError(f"{name} has unexpected architectures: {actual}")


def zip_app(app, archive):
    run("/usr/bin/ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", app, archive)


def notarize(artifact, profile, report):
    reply = subprocess.check_output([
        "xcrun", "notarytool", "submit", str(artifact), "--keychain-profile", profile,
        "--wait", "--timeout", "30m", "--output-format", "json"], text=True)
    result = json.loads(reply)
    report.write_text(json.dumps(result, indent=2) + "\n")
    if result.get("status") != "Accepted":
        raise RuntimeError("notarization did not accept the artifact; it is not releasable")


def macos(args, stage, info):
    if sys.platform != "darwin":
        raise ValueError("macOS packaging requires a Mac build host")
    app = stage / "AgentDocker.app"
    contents = app / "Contents"
    copy_binaries(args, contents / "MacOS")
    resources = contents / "Resources"
    resources.mkdir()
    with tempfile.TemporaryDirectory(prefix="ad-icons-") as scratch:
        # One mark, from one place. This used to render its own icon in
        # Swift while `scripts/bundle-macos.sh` rendered a different one
        # in Python, so the app you installed from a release and the app
        # this packaged wore different faces.
        run(sys.executable, ROOT / "scripts/icon.py", scratch)
        run("/usr/bin/iconutil", "-c", "icns",
            Path(scratch) / "AgentDocker.iconset", "-o", resources / "AgentDocker.icns")
    version = re.split(r"[-+]", args.version)[0]
    plist = {
        "CFBundleIdentifier": "dev.agentdocker.desktop", "CFBundleName": "AgentDocker",
        "CFBundleDisplayName": "AgentDocker", "CFBundleExecutable": "agentdocker-ui",
        "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version,
        "CFBundleVersion": str(args.build_number), "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleIconFile": "AgentDocker.icns", "NSHighResolutionCapable": True,
        "CFBundleGetInfoString": f"AgentDocker {args.version} ({args.source[:12]})",
        "AgentDockerSourceCommit": args.source,
    }
    (contents / "Info.plist").write_bytes(plistlib.dumps(plist))
    (contents / "PkgInfo").write_text("APPL????")
    (resources / "build.json").write_text(json.dumps({key: value for key, value in info.items() if key != "notarized"}, indent=2) + "\n")
    identity = args.identity or "-"
    flags = ["--force", "--sign", identity]
    if identity != "-":
        flags += ["--options", "runtime", "--timestamp"]
    # Sign known nested code explicitly; --deep is reserved for verification.
    for name in ("agentdocker", "agentd"):
        run("/usr/bin/codesign", *flags, contents / "MacOS" / name)
    run("/usr/bin/codesign", *flags, app)
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", app)
    run("/usr/bin/swift", ROOT / "packaging/macos/display-name.swift", app)
    archive = stage / f"agentdocker-desktop-{args.target}.zip"
    zip_app(app, archive)
    if args.notary_profile:
        notarize(archive, args.notary_profile, stage / "notarization-app.json")
        run("xcrun", "stapler", "staple", app)
        run("xcrun", "stapler", "validate", app)
        run("/usr/sbin/spctl", "--assess", "--type", "execute", app)
        archive.unlink()
        zip_app(app, archive)
        info["notarized"] = True
    # Validate the final distributed bytes, after all bundle operations and
    # resource-preserving archive creation. This catches forbidden FinderInfo.
    with tempfile.TemporaryDirectory(prefix="ad-package-check-") as scratch:
        run("/usr/bin/ditto", "-x", "-k", archive, scratch)
        run("/usr/bin/codesign", "--verify", "--deep", "--strict", Path(scratch) / app.name)
    if args.dmg:
        with tempfile.TemporaryDirectory(prefix="ad-dmg-", dir=stage) as scratch:
            image_root = Path(scratch)
            run("/usr/bin/ditto", app, image_root / "AgentDocker.app")
            (image_root / "Applications").symlink_to("/Applications", target_is_directory=True)
            dmg = stage / f"agentdocker-desktop-{args.target}.dmg"
            run("/usr/bin/hdiutil", "create", "-volname", "agentdocker", "-srcfolder", image_root,
                "-format", "UDZO", "-ov", dmg)
            if identity != "-":
                run("/usr/bin/codesign", "--force", "--sign", identity, "--timestamp", dmg)
                run("/usr/bin/codesign", "--verify", "--strict", dmg)
            if args.notary_profile:
                notarize(dmg, args.notary_profile, stage / "notarization-dmg.json")
                run("xcrun", "stapler", "staple", dmg)
                run("xcrun", "stapler", "validate", dmg)
    return app, archive, contents / "MacOS"


def linux(args, stage, info):
    info["signing"] = "checksum"
    app = stage / "agentdocker-desktop"
    copy_binaries(args, app / "bin")
    share = app / "share"
    for directory in ["applications", "metainfo", "icons/hicolor/scalable/apps"]:
        (share / directory).mkdir(parents=True)
    shutil.copyfile(ROOT / "packaging/linux/agentdocker.desktop", share / "applications/agentdocker.desktop")
    shutil.copyfile(ROOT / "packaging/linux/dev.agentdocker.desktop.metainfo.xml", share / "metainfo/dev.agentdocker.desktop.metainfo.xml")
    shutil.copyfile(ROOT / "packaging/desktop/agentdocker.svg", share / "icons/hicolor/scalable/apps/agentdocker.svg")
    info["binary_sha256"] = {name: sha256(app / "bin" / name) for name in BINARIES}
    (app / "build.json").write_text(json.dumps(info, indent=2) + "\n")
    archive = stage / f"agentdocker-desktop-{args.target}.tar.gz"
    with tarfile.open(archive, "w:gz", format=tarfile.PAX_FORMAT) as target:
        target.add(app, arcname=app.name)
    return app, archive, app / "bin"


def package(args):
    provenance = validate_inputs(args)
    if args.output.exists():
        raise FileExistsError("output already exists; use a fresh directory to preserve prior artifacts")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # A failed signer/packager leaves no directory that looks like a finished release.
    with tempfile.TemporaryDirectory(prefix=".agentdocker-package-", dir=args.output.parent) as scratch:
        stage = Path(scratch)
        info = metadata(args)
        info.update({key: provenance[key] for key in ["source_tree", "source_input_sha256", "source_dirty", "state_schema"]})
        info["installation_lock"] = provenance.get("installation_lock", 0)
        build = macos if "apple-darwin" in args.target else linux
        app, archive, binaries = build(args, stage, info)
        info["binary_sha256"] = {name: sha256(binaries / name) for name in BINARIES}
        info["artifacts"] = {path.name: sha256(path) for path in stage.iterdir() if path.is_file() and path.suffix in {".zip", ".gz", ".dmg"}}
        for name, checksum in info["artifacts"].items():
            (stage / f"{name}.sha256").write_text(f"{checksum}  {name}\n")
        (stage / "manifest.json").write_text(json.dumps(info, indent=2) + "\n")
        # Atomic publication of the complete local artifact directory.
        stage.rename(args.output)
    return info


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--binary-dir", type=Path, required=True)
    result.add_argument("--second-binary-dir", type=Path)
    result.add_argument("--output", type=Path, required=True)
    result.add_argument("--version", required=True)
    result.add_argument("--source", required=True)
    result.add_argument("--target", required=True)
    result.add_argument("--build-number", type=int, default=1)
    result.add_argument("--identity", help="local Developer ID Application identity; default is ad-hoc preview")
    result.add_argument("--notary-profile", help="local notarytool keychain profile (no credentials on the command line)")
    result.add_argument("--dmg", action="store_true")
    return result


if __name__ == "__main__":
    print(json.dumps(package(parser().parse_args()), indent=2))
