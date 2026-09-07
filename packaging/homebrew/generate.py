#!/usr/bin/env python3
"""Generate the installable formula, and the cask beside it, from the
release archives' checksums.

Two files because Homebrew has two kinds of thing: a *formula* installs
commands, a *cask* installs an application. `agentdocker` and `agentd`
are commands; `AgentDocker.app` is an application. Trying to put the app
in the formula would work badly and be removed badly.
"""
import argparse
from pathlib import Path
import re


def _checksum(path: Path) -> str:
    checksum = path.read_text().split()[0]
    if not re.fullmatch(r"[0-9a-fA-F]{64}", checksum) or checksum == "0" * 64:
        raise ValueError(f"invalid checksum in {path.name}")
    return checksum.lower()


def _version(version: str) -> str:
    if not re.fullmatch(r"v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("expected a release version such as v0.1.0")
    return version.removeprefix("v")


def generate(version: str, checksums: Path) -> str:
    """Reject incomplete release inputs instead of shipping placeholder hashes."""
    text = Path(__file__).with_name("agentdocker.rb.in").read_text()
    text = text.replace("@VERSION@", _version(version))
    for target in re.findall(r"@SHA_([^@]+)@", text):
        text = text.replace(
            f"@SHA_{target}@",
            _checksum(checksums / f"agentdocker-{target}.tar.gz.sha256"),
        )
    if re.search(r"@(VERSION|SHA_)", text):
        raise ValueError("unresolved formula placeholder")
    return text


def generate_cask(version: str, checksums: Path) -> str:
    """The application cask. Its archives come from the desktop workflow
    rather than the release build, so this is generated only when they
    are present — a release without a packaged app has no cask, which is
    better than one that points at a download that is not there."""
    text = Path(__file__).with_name("agentdocker-app.rb.in").read_text()
    text = text.replace("@VERSION@", _version(version))
    for target in re.findall(r"@SHA_APP_([^@]+)@", text):
        source = checksums / f"agentdocker-desktop-{target}.zip.sha256"
        if not source.exists():
            raise FileNotFoundError(source)
        text = text.replace(f"@SHA_APP_{target}@", _checksum(source))
    if re.search(r"@(VERSION|SHA_)", text):
        raise ValueError("unresolved cask placeholder")
    return text


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("checksums", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument(
        "--cask",
        type=Path,
        help="also write the application cask here, when the desktop "
        "archives are among the checksums",
    )
    args = parser.parse_args()
    args.output.write_text(generate(args.version, args.checksums))
    if args.cask:
        try:
            args.cask.write_text(generate_cask(args.version, args.checksums))
        except FileNotFoundError as missing:
            # Said out loud and not fatal: the desktop archives are built
            # by a different workflow and a release may legitimately not
            # have them yet.
            print(f"no cask: {missing.filename} is not among the checksums")
