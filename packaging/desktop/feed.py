#!/usr/bin/env python3
"""Generate a download feed from verified native desktop package artifacts.

Public feeds require clean source and notarized Developer ID Mac packages.
Preview feeds are explicitly labelled and never advertise automatic activation.
This generator does not publish files or change a running installation.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile

TARGETS = {"aarch64-apple-darwin", "x86_64-apple-darwin", "universal-apple-darwin",
           "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"}


def checksum(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def generate(manifests, preview=False):
    releases, targets, sources, versions = [], set(), set(), set()
    for path in manifests:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 128 * 1024:
            raise ValueError("package manifest must be a regular file below 128 KiB")
        value = json.loads(path.read_text())
        if value.get("format") != 1 or value.get("product") != "agentdocker":
            raise ValueError("unsupported package manifest")
        target, version = value.get("target"), value.get("version", "")
        if target not in TARGETS or target in targets:
            raise ValueError("unsupported or duplicate target")
        if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9._-]+)?", version):
            raise ValueError("invalid release version")
        if not preview and "-" in version.split("+", 1)[0]:
            raise ValueError("prerelease versions require a preview feed")
        for key, width in [("source_commit", 40), ("source_tree", 40), ("source_input_sha256", 64)]:
            if not re.fullmatch(r"[0-9a-f]{" + str(width) + "}", value.get(key, "")):
                raise ValueError("missing or invalid source identity")
        if type(value.get("source_dirty")) is not bool or (not preview and value["source_dirty"]):
            raise ValueError("public feed requires clean source")
        if type(value.get("state_schema")) is not int or not 1 <= value["state_schema"] <= 0xFFFFFFFF:
            raise ValueError("invalid state schema")
        mac = target.endswith("apple-darwin")
        if not preview and mac and (value.get("signing") != "developer-id" or value.get("notarized") is not True):
            raise ValueError("public Mac feed requires notarized Developer ID packages")
        name = "agentdocker-desktop-" + target + (".zip" if mac else ".tar.gz")
        archive = path.parent / name
        if archive.is_symlink() or not archive.is_file():
            raise ValueError("missing regular desktop archive")
        size = archive.stat().st_size
        if not 0 < size <= 40 * 1024**2 * (2 if target.startswith("universal-") else 1):
            raise ValueError("archive exceeds download budget")
        expected = value.get("artifacts", {}).get(name)
        if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected) or checksum(archive) != expected:
            raise ValueError("archive checksum does not match package manifest")
        if value.get("size", {}).get("archive_bytes", {}).get(name) != size:
            raise ValueError("archive size does not match package manifest")
        sources.add((value["source_commit"], value["source_tree"], value["source_input_sha256"], value["state_schema"]))
        versions.add(version)
        targets.add(target)
        releases.append({"target": target, "version": version, "source_commit": value["source_commit"],
                         "state_schema": value["state_schema"], "signing": value["signing"],
                         "notarized": value.get("notarized") is True,
                         "archive": {"name": name, "sha256": expected, "bytes": size,
                            "url": f"https://github.com/brandopakel/AgentDocker/releases/download/v{version}/{name}"}})
    if not releases or len(sources) != 1 or len(versions) != 1:
        raise ValueError("feed requires one source/version/schema across all targets")
    return {"format": 1, "product": "agentdocker", "channel": "preview" if preview else "stable",
            "policy": {"check_interval_hours": 24, "download": "manual", "activation": "explicit",
                       "daemon_replacement": "deferred_until_sessions_finish"},
            "releases": sorted(releases, key=lambda entry: entry["target"])}


def write(output, value):
    # A failed check never replaces the previous feed. Avoid partial JSON on interruption.
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", dir=output.parent, delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(value, stream, indent=2)
            stream.write("\n")
            stream.flush()
        temporary.replace(output)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifests", type=Path, nargs="+")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--preview", action="store_true")
    args = parser.parse_args()
    write(args.output, generate(args.manifests, args.preview))
