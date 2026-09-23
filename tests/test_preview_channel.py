"""Publication failures must never move preview or stable users backwards."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("preview_channel", ROOT / "packaging/desktop/preview_channel.py")
CHANNEL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHANNEL)


class FakeGitHub:
    repo = "brandopakel/AgentDocker"

    def __init__(self):
        self.releases = {"v0.1.0": {"tag_name": "v0.1.0", "draft": False, "prerelease": False}}
        self.sources = {}
        self.blobs = {}
        self.writes = []
        self.fail_upload = False
        self.leave_starter = False
        self.fail_publish = False
        self.corrupt_upload = False

    def asset(self, name, data):
        asset_id = len(self.blobs) + 1
        self.blobs[asset_id] = data
        return {"id": asset_id, "name": name, "size": len(data), "state": "uploaded",
                "digest": "sha256:" + CHANNEL.digest(data)}

    def candidate(self, tag):
        source = CHANNEL.digest(tag.encode())[:40]
        self.sources[tag] = source
        value = {"format": 1, "product": "agentdocker", "channel": "preview",
                 "policy": {"download": "manual", "activation": "explicit",
                            "daemon_replacement": "deferred_until_sessions_finish"}, "releases": []}
        release = {"tag_name": tag, "draft": False, "prerelease": True,
                   "published_at": "2026-09-22T00:00:00Z", "assets": []}
        for target in sorted(CHANNEL.TARGETS):
            name = "agentdocker-desktop-" + target + (".zip" if target.endswith("darwin") else ".tar.gz")
            archive = self.asset(name, (tag + target).encode())
            release["assets"].append(archive)
            value["releases"].append({"target": target, "version": tag[1:], "source_commit": source,
                                      "state_schema": 8, "signing": "local-preview", "notarized": False,
                                      "archive": {"name": name, "bytes": archive["size"],
                                                  "sha256": archive["digest"][7:],
                                                  "url": f"https://github.com/{self.repo}/releases/download/{tag}/{name}"}})
        release["assets"].append(self.asset(CHANNEL.FEED, json.dumps(value).encode()))
        self.releases[tag] = release
        return value

    def set_feed(self, tag, value):
        release = self.releases[tag]
        release["assets"] = [a for a in release["assets"] if a["name"] != CHANNEL.FEED]
        release["assets"].append(self.asset(CHANNEL.FEED, json.dumps(value).encode()))

    def release(self, tag):
        return copy.deepcopy(self.releases.get(tag))

    def source(self, tag):
        return self.sources[tag]

    def download(self, asset):
        return self.blobs[asset["id"]]

    def create(self, notes, source):
        self.writes.append("create")
        self.releases[CHANNEL.CHANNEL] = {"tag_name": CHANNEL.CHANNEL, "draft": True,
                                         "prerelease": True, "body": notes.read_text(), "assets": []}

    def record(self, notes):
        self.writes.append("record")
        self.releases[CHANNEL.CHANNEL]["body"] = notes.read_text()

    def upload(self, feed):
        self.writes.append("upload")
        channel = self.releases[CHANNEL.CHANNEL]
        channel["assets"] = []  # gh --clobber may remove the previous asset first.
        if self.fail_upload:
            if self.leave_starter:
                starter = self.asset(CHANNEL.FEED, b"")
                starter["state"] = "starter"
                channel["assets"] = [starter]
            raise RuntimeError("upload failed after delete")
        data = feed.read_bytes() + (b" " if self.corrupt_upload else b"")
        channel["assets"] = [self.asset(CHANNEL.FEED, data)]

    def publish(self):
        self.writes.append("publish")
        if self.fail_publish:
            raise RuntimeError("publish failed")
        self.releases[CHANNEL.CHANNEL].update(draft=False, published_at="2026-09-22T01:00:00Z")


class PreviewChannel(unittest.TestCase):
    def setUp(self):
        self.github = FakeGitHub()
        self.old = "v0.2.0-beta.2"
        self.new = "v0.2.0-beta.10"
        self.github.candidate(self.old)
        self.github.candidate(self.new)

    def promote(self, tag=None):
        return CHANNEL.promotion(self.github, tag or self.old)

    def test_exact_feed_promoted_and_versioned_releases_unchanged(self):
        original = copy.deepcopy(self.github.releases)
        self.assertEqual(self.promote()["action"], "promoted")
        published = self.github.releases[CHANNEL.CHANNEL]
        self.assertIs(published["draft"], False)
        self.assertIs(published["prerelease"], True)
        expected = CHANNEL.assets(original[self.old])[CHANNEL.FEED]
        actual = CHANNEL.assets(published)[CHANNEL.FEED]
        self.assertEqual(self.github.download(expected), self.github.download(actual))
        self.assertEqual({k: v for k, v in self.github.releases.items() if k != CHANNEL.CHANNEL}, original)
        self.assertEqual(self.github.writes, ["create", "upload", "publish"])

    def test_retry_is_idempotent_and_older_promotion_has_no_writes(self):
        self.promote(self.new)
        self.github.writes.clear()
        self.assertEqual(self.promote(self.new)["action"], "unchanged")
        self.assertEqual(self.promote(self.old), {"action": "preserved_newer", "tag": self.new})
        self.assertEqual(self.github.writes, [])

    def test_failed_clobber_records_high_water_and_same_version_repairs(self):
        self.promote(self.old)
        self.github.fail_upload = True
        with self.assertRaisesRegex(RuntimeError, "upload failed"):
            self.promote(self.new)
        self.github.writes.clear()
        self.assertEqual(self.promote(self.old)["action"], "preserved_newer")
        self.assertEqual(self.github.writes, [])
        self.github.fail_upload = False
        self.assertEqual(self.promote(self.new)["action"], "promoted")

    def test_failed_initial_upload_or_publication_can_be_repaired(self):
        for failure in ["fail_upload", "fail_publish"]:
            with self.subTest(failure=failure):
                self.setUp()
                setattr(self.github, failure, True)
                with self.assertRaises(RuntimeError):
                    self.promote()
                self.assertIs(self.github.releases[CHANNEL.CHANNEL]["draft"], True)
                setattr(self.github, failure, False)
                self.assertEqual(self.promote()["action"], "promoted")

    def test_empty_starter_feed_is_repaired_only_at_the_recorded_version(self):
        self.github.fail_upload = self.github.leave_starter = True
        with self.assertRaises(RuntimeError):
            self.promote()
        self.github.fail_upload = False
        self.github.writes.clear()
        with self.assertRaisesRegex(ValueError, "unfinished assets"):
            self.promote(self.new)
        self.assertEqual(self.github.writes, [])
        self.assertEqual(self.promote()["action"], "promoted")

    def test_starter_repair_preserves_feed_identity_and_unrelated_assets(self):
        for change in ["canonical_bytes", "unrelated", "nonempty", "versioned"]:
            with self.subTest(change=change):
                self.setUp()
                self.github.fail_upload = self.github.leave_starter = True
                with self.assertRaises(RuntimeError):
                    self.promote()
                self.github.fail_upload = False
                channel = self.github.releases[CHANNEL.CHANNEL]
                if change == "canonical_bytes":
                    value = json.loads(self.github.download(CHANNEL.assets(self.github.releases[self.old])[CHANNEL.FEED]))
                    value["changed"] = True
                    self.github.set_feed(self.old, value)
                elif change == "unrelated":
                    channel["assets"][0]["name"] = "unrelated.json"
                elif change == "nonempty":
                    channel["assets"][0]["size"] = 1
                else:
                    self.github.releases[self.old]["assets"][-1]["state"] = "starter"
                original = copy.deepcopy(channel)
                self.github.writes.clear()
                with self.assertRaises(ValueError):
                    self.promote()
                self.assertEqual(channel, original)
                self.assertEqual(self.github.writes, [])

    def test_readback_failure_does_not_publish_new_channel(self):
        self.github.corrupt_upload = True
        with self.assertRaisesRegex(ValueError, "did not verify"):
            self.promote()
        self.assertNotIn("publish", self.github.writes)
        self.github.corrupt_upload = False
        self.assertEqual(self.promote()["action"], "promoted")

    def test_same_version_changed_feed_is_refused(self):
        self.promote()
        value = json.loads(self.github.download(CHANNEL.assets(self.github.releases[self.old])[CHANNEL.FEED]))
        value["extra"] = "changed after publication"
        self.github.set_feed(self.old, value)
        self.github.writes.clear()
        with self.assertRaisesRegex(ValueError, "same-version"):
            self.promote()
        self.assertEqual(self.github.writes, [])

    def test_invalid_source_release_is_never_promoted(self):
        for changes in [{"draft": True}, {"prerelease": False}, {"published_at": None}, {"tag_name": self.new}]:
            with self.subTest(changes=changes):
                self.setUp()
                self.github.releases[self.old].update(changes)
                with self.assertRaisesRegex(ValueError, "published prerelease"):
                    self.promote()
                self.assertEqual(self.github.writes, [])

    def test_malformed_or_unverified_feed_cannot_mutate_channel(self):
        cases = [
            lambda v: v.update(channel="stable"),
            lambda v: v["policy"].update(activation="automatic"),
            lambda v: v["releases"].pop(),
            lambda v: v["releases"].__setitem__(0, None),
            lambda v: v["releases"][0].update(target=[]),
            lambda v: v["releases"][0].update(version="0.2.0-beta.3"),
            lambda v: v["releases"][0].update(source_commit="f" * 40),
            lambda v: v["releases"][0].update(state_schema=True),
            lambda v: v["releases"][0].update(state_schema=9),
            lambda v: v["releases"][0].update(archive=None),
            lambda v: v["releases"][0]["archive"].update(bytes=999),
            lambda v: v["releases"][0]["archive"].update(sha256="0" * 64),
            lambda v: v["releases"][0]["archive"].update(url="https://example.com/other.tar.gz"),
        ]
        for index, mutate in enumerate(cases):
            with self.subTest(case=index):
                self.setUp()
                value = self.github.candidate(self.old)
                mutate(value)
                self.github.set_feed(self.old, value)
                with self.assertRaises(ValueError):
                    self.promote()
                self.assertEqual(self.github.writes, [])

    def test_unrecognized_channel_or_unrelated_assets_preserved(self):
        for changes in [{"body": None}, {"body": "some other release"}, {"prerelease": False},
                        {"body": CHANNEL.MARKER + "\n[]"}, {"assets": [{"name": "other", "state": "uploaded"}]}]:
            with self.subTest(changes=changes):
                self.setUp()
                self.promote()
                self.github.releases[CHANNEL.CHANNEL].update(changes)
                original = copy.deepcopy(self.github.releases[CHANNEL.CHANNEL])
                self.github.writes.clear()
                with self.assertRaises(ValueError):
                    self.promote(self.new)
                self.assertEqual(self.github.releases[CHANNEL.CHANNEL], original)
                self.assertEqual(self.github.writes, [])

    def test_semver_order_and_stable_rejection(self):
        ordered = ["v0.2.0-alpha", "v0.2.0-alpha.1", "v0.2.0-alpha.beta", "v0.2.0-beta.2",
                   "v0.2.0-beta.10", "v0.2.0-rc.1", "v0.3.0-alpha"]
        self.assertEqual(sorted(reversed(ordered), key=CHANNEL.version_key), ordered)
        for tag in ["v0.2.0", "v0.2.0-beta.01", "v01.2.0-beta", "v0.2.0-beta+build", "../bad", None]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                CHANNEL.version_key(tag)


class GitHubTransport(unittest.TestCase):
    def setUp(self):
        self.github = CHANNEL.GitHub(FakeGitHub.repo)

    def test_channel_404_finds_the_unpublished_draft_and_verifies_its_bytes(self):
        fake = FakeGitHub()
        tag = "v0.2.0-beta.2"
        fake.candidate(tag)
        fake.fail_publish = True
        with self.assertRaises(RuntimeError):
            CHANNEL.promotion(fake, tag)
        draft = copy.deepcopy(fake.releases[CHANNEL.CHANNEL])
        missing = subprocess.CompletedProcess([], 1, b"HTTP/2.0 404 Not Found\r\n\r\n{}")
        with patch.object(CHANNEL.subprocess, "run", return_value=missing), \
                patch.object(self.github, "command", return_value=json.dumps([draft]).encode()) as command:
            self.assertEqual(self.github.release(CHANNEL.CHANNEL), draft)
            self.assertIn("per_page=100&page=1", command.call_args.args[1])
        fake.fail_publish = False
        self.assertEqual(CHANNEL.promotion(fake, tag)["action"], "promoted")

    def test_draft_lookup_is_bounded_and_preserves_ambiguous_releases(self):
        draft = {"tag_name": CHANNEL.CHANNEL, "draft": True}
        full_page = [{"tag_name": f"v0.1.{i}"} for i in range(100)]
        with patch.object(self.github, "command", side_effect=[json.dumps(full_page).encode(), json.dumps([draft]).encode()]):
            self.assertEqual(self.github.draft_channel(), draft)
        for pages in [[draft, draft], {}, [None], [{"draft": True}],
                      [{"tag_name": None}], [{"tag_name": 7}]]:
            with self.subTest(entries=pages), patch.object(self.github, "command", return_value=json.dumps(pages).encode()):
                with self.assertRaises(ValueError):
                    self.github.draft_channel()
        with patch.object(self.github, "command", return_value=json.dumps(full_page).encode()) as command:
            with self.assertRaisesRegex(ValueError, "lookup limit"):
                self.github.draft_channel()
            self.assertEqual(command.call_count, 10)
        with patch.object(self.github, "command", side_effect=RuntimeError("permission denied")):
            with self.assertRaises(RuntimeError):
                self.github.draft_channel()
        with patch.object(self.github, "command", return_value=b"[]"):
            self.assertIsNone(self.github.draft_channel())

    def test_malformed_inventory_never_reaches_channel_creation(self):
        fake = FakeGitHub()
        tag = "v0.2.0-beta.2"
        fake.candidate(tag)
        missing = subprocess.CompletedProcess([], 1, b"HTTP/2.0 404 Not Found\r\n\r\n{}")
        candidate = fake.release(tag)
        with patch.object(CHANNEL.subprocess, "run", return_value=missing), \
                patch.object(self.github, "command", return_value=b'[{"draft":true}]'), \
                patch.object(fake, "release", side_effect=lambda value: candidate if value == tag else self.github.release(value)):
            with self.assertRaisesRegex(ValueError, "invalid release inventory"):
                CHANNEL.promotion(fake, tag)
        self.assertEqual(fake.writes, [])

    def test_only_404_means_missing_release(self):
        for status, code in [(200, 0), (404, 1), (403, 1), (500, 1)]:
            result = subprocess.CompletedProcess([], code, f'HTTP/2.0 {status} Test\r\nX: y\r\n\r\n{{"tag_name":"v0.1.0"}}'.encode())
            with self.subTest(status=status), patch.object(CHANNEL.subprocess, "run", return_value=result):
                if status == 200:
                    self.assertEqual(self.github.release("v0.1.0")["tag_name"], "v0.1.0")
                elif status == 404:
                    self.assertIsNone(self.github.release("v0.1.0"))
                else:
                    with self.assertRaises(RuntimeError):
                        self.github.release("v0.1.0")

    def test_asset_size_and_digest_checked(self):
        data = b"test"
        asset = {"id": 1, "size": len(data), "digest": "sha256:" + CHANNEL.digest(data)}
        with patch.object(self.github, "command", return_value=data) as command:
            self.assertEqual(self.github.download(asset), data)
            for changes in [{"size": None}, {"size": True}, {"size": CHANNEL.LIMIT + 1},
                            {"id": -1}, {"size": 5}, {"digest": "sha256:" + "0" * 64}]:
                with self.subTest(changes=changes), self.assertRaises(ValueError):
                    self.github.download({**asset, **changes})
            self.assertEqual(command.call_count, 3)

    def test_lookup_uses_final_http_header_block(self):
        for status, code in [(200, 0), (404, 1), (403, 1)]:
            response = (f'HTTP/1.1 100 Continue\r\n\r\nHTTP/2.0 {status} Test\r\n\r\n'
                        '{"tag_name":"v0.1.0"}').encode()
            result = subprocess.CompletedProcess([], code, response)
            with self.subTest(status=status), patch.object(CHANNEL.subprocess, "run", return_value=result):
                if status == 200:
                    self.assertEqual(self.github.release("v0.1.0")["tag_name"], "v0.1.0")
                elif status == 404:
                    self.assertIsNone(self.github.release("v0.1.0"))
                else:
                    with self.assertRaisesRegex(RuntimeError, "HTTP 403"):
                        self.github.release("v0.1.0")

    def test_mutating_commands_only_target_nonlatest_preview_channel(self):
        with patch.object(self.github, "command") as command:
            self.github.create(Path("notes.md"), "a" * 40)
            self.github.record(Path("notes.md"))
            self.github.upload(Path(CHANNEL.FEED))
            self.github.publish()
        for call in command.call_args_list:
            args = call.args
            self.assertEqual(args[2], CHANNEL.CHANNEL)
            if args[1] != "upload":
                self.assertIn("--prerelease", args)
                self.assertIn("--latest=false", args)


if __name__ == "__main__":
    unittest.main()
