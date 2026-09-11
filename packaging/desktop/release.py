#!/usr/bin/env python3
"""Prepare installable desktop release assets; never publish or install them."""
import argparse
import base64
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
TARGETS = {"aarch64-apple-darwin", "x86_64-apple-darwin",
           "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"}
SIGNING_ENV = ("MACOS_CERTIFICATE_BASE64", "MACOS_CERTIFICATE_PASSWORD",
               "MACOS_SIGNING_IDENTITY", "MACOS_NOTARY_KEY", "MACOS_NOTARY_KEY_ID",
               "MACOS_NOTARY_ISSUER")


def module(name):
    spec = importlib.util.spec_from_file_location(name, HERE / (name + ".py"))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def version(tag):
    if not re.fullmatch(r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[A-Za-z0-9.-]+)?", tag):
        raise ValueError("release tag must be v followed by a semantic version without build metadata")
    return tag[1:]


def preview(tag):
    return "-" in version(tag).split("+", 1)[0]


def publication(tag, latest):
    """Only a newer stable version may move GitHub's latest download endpoint."""
    requested = version(tag)
    if preview(tag):
        return False
    if str(latest.get("status")) == "404":
        return True
    previous = version(latest.get("tag_name", ""))
    if latest.get("draft") or latest.get("prerelease") or "-" in previous:
        raise ValueError("latest-release response must describe a published stable release")
    return tuple(map(int, requested.split("."))) > tuple(map(int, previous.split(".")))


def signing_config(environment, required):
    values = {key: environment.get(key, "") for key in SIGNING_ENV}
    # An empty export password is valid, but the variable must be supplied.
    present = [bool(values[key]) if key != "MACOS_CERTIFICATE_PASSWORD"
               else key in environment for key in SIGNING_ENV]
    if not any(values.values()) and not required:
        return None
    if not all(present):
        raise ValueError("Mac releases need all signing/notary variables; see docs/RELEASE-AUTOMATION.md")
    if not values["MACOS_SIGNING_IDENTITY"].startswith("Developer ID Application:"):
        raise ValueError("Mac releases require a Developer ID Application identity")
    try:
        certificate = base64.b64decode(values["MACOS_CERTIFICATE_BASE64"], validate=True)
    except ValueError:
        raise ValueError("invalid signing certificate encoding") from None
    if not certificate or len(certificate) > 1024 * 1024:
        raise ValueError("signing certificate must be between 1 byte and 1 MiB")
    return values, certificate


def credential_command(*arguments):
    # Never include command arguments or captured credential-tool output in errors.
    try:
        result = subprocess.run(arguments, capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.TimeoutExpired):
        raise RuntimeError(f"{Path(arguments[0]).name} could not finish signing setup") from None
    if result.returncode:
        raise RuntimeError(f"{Path(arguments[0]).name} failed during signing setup (exit {result.returncode})")
    return result.stdout.strip()


@contextmanager
def signing(environment, required):
    configuration = signing_config(environment, required)
    if configuration is None:
        yield []
        return
    values, certificate = configuration
    original_default = shlex.split(credential_command("security", "default-keychain", "-d", "user"))
    original_search = shlex.split(credential_command("security", "list-keychains", "-d", "user"))
    with tempfile.TemporaryDirectory(prefix="agentdocker-release-signing-") as directory:
        root = Path(directory)
        cert = root / "certificate.p12"
        key = root / "notary.p8"
        keychain = str(root / "signing.keychain-db")
        cert.write_bytes(certificate)
        key.write_text(values["MACOS_NOTARY_KEY"])
        cert.chmod(0o600)
        key.chmod(0o600)
        password = secrets.token_hex(32)
        created = False
        try:
            credential_command("security", "create-keychain", "-p", password, keychain)
            created = True
            credential_command("security", "set-keychain-settings", "-lut", "21600", keychain)
            credential_command("security", "unlock-keychain", "-p", password, keychain)
            credential_command("security", "import", str(cert), "-P", values["MACOS_CERTIFICATE_PASSWORD"],
                               "-T", "/usr/bin/codesign", "-t", "cert", "-f", "pkcs12", "-k", keychain)
            credential_command("security", "set-key-partition-list", "-S", "apple-tool:,apple:",
                               "-s", "-k", password, keychain)
            credential_command("security", "list-keychains", "-d", "user", "-s", keychain, *original_search)
            credential_command("security", "default-keychain", "-d", "user", "-s", keychain)
            credential_command("xcrun", "notarytool", "store-credentials", "agentdocker-release",
                               "--key", str(key), "--key-id", values["MACOS_NOTARY_KEY_ID"],
                               "--issuer", values["MACOS_NOTARY_ISSUER"], "--keychain", keychain)
            yield ["--identity", values["MACOS_SIGNING_IDENTITY"], "--notary-profile", "agentdocker-release"]
        finally:
            # Restore both settings even when packaging/notarization fails.
            try:
                if original_default:
                    credential_command("security", "default-keychain", "-d", "user", "-s", *original_default)
            finally:
                try:
                    credential_command("security", "list-keychains", "-d", "user", "-s", *original_search)
                finally:
                    if created:
                        credential_command("security", "delete-keychain", keychain)


def prepare(native_manifest, output, tag, environment=None):
    environment = os.environ if environment is None else environment
    requested = version(tag)
    build = json.loads(native_manifest.read_text())
    if build.get("version") != requested or build.get("source_dirty") is not False:
        raise ValueError("release tag requires a matching clean source build")
    target = build.get("target")
    if target not in TARGETS:
        raise ValueError("unsupported desktop release target")
    if output.exists():
        raise FileExistsError("release output already exists")
    output.parent.mkdir(parents=True, exist_ok=True)
    packager = module("package")
    with tempfile.TemporaryDirectory(prefix=".agentdocker-release-", dir=output.parent) as temporary:
        root = Path(temporary)
        payload = root / "package"
        arguments = ["--binary-dir", build["binary_directory"], "--output", str(payload),
                     "--version", requested, "--source", build["source_commit"], "--target", target]
        if target.endswith("apple-darwin"):
            with signing(environment, required=not preview(tag)) as flags:
                info = packager.package(packager.parser().parse_args([*arguments, *flags]))
        else:
            info = packager.package(packager.parser().parse_args(arguments))
        # Run feed policy before exposing any completed release assets.
        module("feed").generate([payload / "manifest.json"], preview=preview(tag))
        assets = root / "assets"
        assets.mkdir()
        for name in info["artifacts"]:
            shutil.copyfile(payload / name, assets / name)
            shutil.copyfile(payload / (name + ".sha256"), assets / (name + ".sha256"))
        shutil.copyfile(payload / "manifest.json", assets / ("manifest-" + target + ".json"))
        assets.rename(output)
    return info


def collect(directory, output, tag):
    requested = version(tag)
    manifests = sorted(directory.glob("manifest-*.json"))
    values = [json.loads(path.read_text()) for path in manifests]
    if len(values) != len(TARGETS) or {value.get("target") for value in values} != TARGETS:
        raise ValueError("release feed requires all four native desktop targets")
    if any(value.get("version") != requested for value in values):
        raise ValueError("release feed version does not match tag")
    feed = module("feed")
    value = feed.generate(manifests, preview=preview(tag))
    expected_name = "updates-preview.json" if preview(tag) else "updates.json"
    if output.name != expected_name:
        raise ValueError("feed filename must match the stable/preview channel")
    feed.write(output, value)
    return value


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    package_parser = commands.add_parser("package")
    package_parser.add_argument("--native-manifest", type=Path, required=True)
    feed_parser = commands.add_parser("feed")
    feed_parser.add_argument("--directory", type=Path, required=True)
    validate_parser = commands.add_parser("validate")
    validate_parser.add_argument("--tag", required=True)
    publication_parser = commands.add_parser("publication")
    publication_parser.add_argument("--tag", required=True)
    publication_parser.add_argument("--latest", type=Path, required=True)
    for command in (package_parser, feed_parser):
        command.add_argument("--tag", required=True)
        command.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "package":
        prepare(args.native_manifest, args.output, args.tag)
    elif args.command == "feed":
        collect(args.directory, args.output, args.tag)
    elif args.command == "publication":
        print("true" if publication(args.tag, json.loads(args.latest.read_text())) else "false")
    else:
        import tomllib
        declared = tomllib.loads((HERE.parents[1] / "Cargo.toml").read_text())["workspace"]["package"]["version"]
        if version(args.tag) != declared:
            raise ValueError("release tag does not match Cargo.toml")
