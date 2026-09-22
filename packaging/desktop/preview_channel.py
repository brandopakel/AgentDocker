#!/usr/bin/env python3
"""Promote the feed of an already published, verified preview release.

Run through the preview-channel workflow: its shared release-publication lock
serializes promotions and repairs. Versioned release assets are only read.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

CHANNEL = "channel-preview"
FEED = "updates-preview.json"
MARKER = "<!-- agentdocker-preview-channel-v1 -->"
LIMIT = 128 * 1024
TARGETS = {"aarch64-apple-darwin", "x86_64-apple-darwin",
           "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"}


def version_key(tag):
    """SemVer precedence for strictly valid prerelease tags, without build metadata."""
    if not isinstance(tag, str) or len(tag) > 65:
        raise ValueError("preview tag exceeds its limit")
    match = re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)", tag)
    if not match:
        raise ValueError("preview channel requires a semantic prerelease tag")
    pre = []
    for part in match[4].split("."):
        if part.isdigit():
            if len(part) > 1 and part.startswith("0"):
                raise ValueError("numeric prerelease identifiers cannot have leading zeroes")
            pre.append((0, int(part)))
        else:
            pre.append((1, part))
    return (*map(int, match.group(1, 2, 3)), tuple(pre))


def digest(data):
    return hashlib.sha256(data).hexdigest()


def hex_value(value, width):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{" + str(width) + "}", value)


class GitHub:
    def __init__(self, repo):
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
            raise ValueError("invalid repository")
        self.repo = repo

    def command(self, *args):
        result = subprocess.run(["gh", *args], capture_output=True, timeout=120)
        if result.returncode:
            # Do not echo command environments, credentials or arbitrary server bodies.
            raise RuntimeError(f"GitHub command failed (exit {result.returncode})")
        return result.stdout

    def release(self, tag):
        result = subprocess.run(
            ["gh", "api", "--include", f"repos/{self.repo}/releases/tags/{tag}"],
            capture_output=True, timeout=120)
        headers, separator, body = result.stdout.replace(b"\r\n", b"\n").partition(b"\n\n")
        status = re.match(rb"HTTP/[0-9.]+ ([0-9]{3})", headers)
        if not separator or not status:
            raise RuntimeError("GitHub release lookup returned no HTTP status")
        if status[1] == b"404":
            return None
        if result.returncode or status[1] != b"200":
            raise RuntimeError(f"GitHub release lookup failed (HTTP {status[1].decode()})")
        return json.loads(body)

    def source(self, tag):
        ref = json.loads(self.command("api", f"repos/{self.repo}/git/ref/tags/{tag}"))["object"]
        for _ in range(5):
            if not re.fullmatch(r"[0-9a-f]{40}", ref.get("sha", "")):
                raise ValueError("release tag has no valid source identity")
            if ref.get("type") == "commit":
                return ref["sha"]
            if ref.get("type") != "tag":
                break
            ref = json.loads(self.command("api", f"repos/{self.repo}/git/tags/{ref['sha']}"))["object"]
        raise ValueError("release tag does not resolve to a commit")

    def download(self, asset):
        if (type(asset.get("id")) is not int or asset["id"] <= 0
                or type(asset.get("size")) is not int or not 0 < asset["size"] <= LIMIT):
            raise ValueError("feed asset exceeds its limit or has no asset id")
        data = self.command("api", f"repos/{self.repo}/releases/assets/{asset['id']}",
                            "-H", "Accept: application/octet-stream")
        if len(data) != asset["size"] or len(data) > LIMIT:
            raise ValueError("downloaded feed size differs from its published asset")
        if asset.get("digest") not in (None, "sha256:" + digest(data)):
            raise ValueError("downloaded feed digest differs from its published asset")
        return data

    def create(self, notes, source):
        self.command("release", "create", CHANNEL, "--repo", self.repo, "--target", source,
                     "--title", "Preview update channel", "--notes-file", str(notes),
                     "--draft", "--prerelease", "--latest=false")

    def record(self, notes):
        self.command("release", "edit", CHANNEL, "--repo", self.repo,
                     "--notes-file", str(notes), "--prerelease", "--latest=false")

    def upload(self, feed):
        self.command("release", "upload", CHANNEL, str(feed), "--repo", self.repo, "--clobber")

    def publish(self):
        self.command("release", "edit", CHANNEL, "--repo", self.repo,
                     "--draft=false", "--prerelease", "--latest=false")


def assets(release):
    if not isinstance(release, dict):
        raise ValueError("release has an invalid asset inventory")
    entries = release.get("assets")
    if not isinstance(entries, list) or len(entries) > 100:
        raise ValueError("release has an invalid asset inventory")
    result = {}
    for asset in entries:
        if not isinstance(asset, dict):
            raise ValueError("release has an invalid asset entry")
        name = asset.get("name")
        if not isinstance(name, str) or name in result or asset.get("state") != "uploaded":
            raise ValueError("release contains ambiguous or unfinished assets")
        result[name] = asset
    return result


def feed_tag(data):
    if len(data) > LIMIT:
        raise ValueError("preview feed exceeds its limit")
    value = json.loads(data)
    if (not isinstance(value, dict) or type(value.get("format")) is not int
            or value["format"] != 1 or value.get("product") != "agentdocker"
            or value.get("channel") != "preview"):
        raise ValueError("not an AgentDocker preview feed")
    policy = value.get("policy")
    if (not isinstance(policy, dict) or policy.get("download") != "manual"
            or policy.get("activation") != "explicit"
            or policy.get("daemon_replacement") != "deferred_until_sessions_finish"):
        raise ValueError("preview feed must preserve explicit update consent")
    releases = value.get("releases")
    if not isinstance(releases, list) or len(releases) != len(TARGETS):
        raise ValueError("preview feed must contain all four desktop targets")
    if any(not isinstance(entry, dict) or not isinstance(entry.get("target"), str)
           or not isinstance(entry.get("version"), str) for entry in releases):
        raise ValueError("preview feed has an invalid release entry")
    if {entry.get("target") for entry in releases} != TARGETS:
        raise ValueError("preview feed has missing or duplicate targets")
    versions = {entry.get("version") for entry in releases}
    if len(versions) != 1 or not isinstance(next(iter(versions)), str):
        raise ValueError("preview feed mixes versions")
    tag = "v" + next(iter(versions))
    version_key(tag)
    return tag, value


def verify_feed(data, tag, release, repo, source):
    actual, value = feed_tag(data)
    if actual != tag:
        raise ValueError("preview feed version does not match its published tag")
    inventory = assets(release)
    schemas = set()
    for entry in value["releases"]:
        if entry.get("source_commit") != source:
            raise ValueError("preview feed source differs from its immutable release tag")
        schema = entry.get("state_schema")
        if type(schema) is not int or not 0 < schema <= 0xFFFFFFFF:
            raise ValueError("invalid preview state schema")
        schemas.add(schema)
        target = entry["target"]
        name = "agentdocker-desktop-" + target + (".zip" if target.endswith("darwin") else ".tar.gz")
        archive = entry.get("archive", {})
        if not isinstance(archive, dict):
            raise ValueError("preview feed has an invalid archive entry")
        size = archive.get("bytes")
        expected = inventory.get(name, {})
        sha = archive.get("sha256", "")
        if (archive.get("name") != name or type(size) is not int or not 0 < size <= 40 * 1024 ** 2
                or expected.get("size") != size or not hex_value(sha, 64)
                or archive.get("url") != f"https://github.com/{repo}/releases/download/{tag}/{name}"
                or expected.get("digest") not in (None, "sha256:" + sha)):
            raise ValueError("preview archive does not match its published versioned asset")
    if len(schemas) != 1:
        raise ValueError("preview feed mixes state schemas")


def promotion(github, tag):
    requested = version_key(tag)
    release = github.release(tag)
    if (not isinstance(release, dict) or release.get("tag_name") != tag or release.get("draft") is not False
            or release.get("prerelease") is not True or not release.get("published_at")):
        raise ValueError("preview channel requires an already published prerelease")
    asset = assets(release).get(FEED)
    if asset is None:
        raise ValueError("published preview has no update feed")
    data = github.download(asset)
    source = github.source(tag)
    verify_feed(data, tag, release, github.repo, source)
    channel = github.release(CHANNEL)
    if channel is not None:
        if (not isinstance(channel, dict) or channel.get("tag_name") != CHANNEL
                or channel.get("prerelease") is not True or type(channel.get("draft")) is not bool
                or not isinstance(channel.get("body"), str)
                or not channel["body"].startswith(MARKER + "\n")):
            raise ValueError("refusing to replace an unrecognized channel release")
        record = json.loads(channel["body"][len(MARKER) + 1:])
        if (not isinstance(record, dict) or type(record.get("format")) is not int or record["format"] != 1
                or not hex_value(record.get("feed_sha256"), 64)
                or not hex_value(record.get("source_commit"), 40)):
            raise ValueError("invalid preview channel promotion record")
        previous = version_key(record.get("tag"))
        if requested < previous:
            return {"action": "preserved_newer", "tag": record["tag"]}
        if requested == previous and record["feed_sha256"] != digest(data):
            raise ValueError("same-version preview feed bytes changed")
        if requested == previous and record["source_commit"] != source:
            raise ValueError("same-version preview source changed")
        inventory = assets(channel)
        if set(inventory) - {FEED}:
            raise ValueError("channel contains unrelated assets; preserved")
        if FEED in inventory:
            existing = github.download(inventory[FEED])
            existing_tag, _ = feed_tag(existing)
            if version_key(existing_tag) > previous:
                raise ValueError("channel feed is newer than its promotion record; preserved")
            if existing == data and channel.get("draft") is False:
                return {"action": "unchanged", "tag": tag}
    record = {"format": 1, "tag": tag, "source_commit": source, "feed_sha256": digest(data)}
    with tempfile.TemporaryDirectory(prefix="agentdocker-preview-channel-") as directory:
        root = Path(directory)
        feed = root / FEED
        feed.write_bytes(data)
        notes = root / "notes.md"
        notes.write_text(MARKER + "\n" + json.dumps(record, indent=2) + "\n")
        # Record the new high-water version BEFORE clobber can remove the old
        # asset. A failed upload cannot permit an older release to take over.
        if channel:
            github.record(notes)
        else:
            github.create(notes, source)
        github.upload(feed)
        uploaded = github.release(CHANNEL)
        published_asset = assets(uploaded).get(FEED)
        if published_asset is None or github.download(published_asset) != data:
            raise ValueError("uploaded preview channel feed did not verify")
        github.publish()
        published = github.release(CHANNEL)
        if (not isinstance(published, dict) or published.get("draft") is not False
                or published.get("prerelease") is not True or not published.get("published_at")):
            raise ValueError("preview channel publication did not verify")
    return {"action": "promoted", "tag": tag, "feed_sha256": digest(data)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", "brandopakel/AgentDocker"))
    args = parser.parse_args()
    print(json.dumps(promotion(GitHub(args.repo), args.tag), indent=2))
