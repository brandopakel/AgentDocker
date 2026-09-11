#!/usr/bin/env python3
"""Exercise `agentdocker desktop update` offline against a locally packaged
release: a file:// feed built from the package manifest, a disposable prefix,
and the packaged CLI itself. Checks that a check downloads nothing, that a
tampered checksum is refused before anything is installed, that download,
verify, extract, preview and apply produce a managed installation of the
advertised version, and that a second check then finds nothing newer.

No real launcher, daemon or provider configuration is touched.
"""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

MAC = sys.platform == "darwin"
PAYLOAD = "AgentDocker.app" if MAC else "agentdocker-desktop"
BIN = Path("Contents/MacOS") if MAC else Path("bin")


@contextmanager
def record(report, output):
    try:
        yield
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (output / "result.json").write_text(json.dumps(report, indent=2) + "\n")


def cli(binary, prefix, *arguments, env, success=True):
    command = [str(binary), "desktop", "--prefix", str(prefix), *arguments]
    result = subprocess.run(command, capture_output=True, text=True, timeout=600, env=env)
    if success:
        assert result.returncode == 0, f"{command} failed: {result.stderr}"
        return json.loads(result.stdout) if result.stdout.strip() else {}
    assert result.returncode != 0, f"{command} unexpectedly succeeded: {result.stdout}"
    return result.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="package.py output directory (manifest.json, archive, payload)")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source = args.source.resolve(strict=True)
    args.output.mkdir(mode=0o700)
    manifest = json.loads((source / "manifest.json").read_text())
    target = manifest["target"]
    archive_name = "agentdocker-desktop-" + target + (".zip" if MAC else ".tar.gz")
    archive = source / archive_name
    assert archive.is_file(), f"missing {archive}"
    binary = source / PAYLOAD / BIN / "agentdocker"
    report = {"passed": False, "source": {key: manifest[key] for key in
              ("source_commit", "source_tree", "source_input_sha256", "source_dirty")},
              "target": target, "steps": [], "scope": "Owned offline installation flow with synthetic version relabelling; not a distinct-source upgrade or hosted-download test.",
              "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "binary_sha256": {name: hashlib.sha256((source / PAYLOAD / BIN / name).read_bytes()).hexdigest()
                                for name in ("agentdocker", "agentd", "agentdocker-ui")}}
    with record(report, args.output), tempfile.TemporaryDirectory(prefix="ad-update-", dir="/tmp") as scratch:
        root = Path(scratch)
        prefix = root / "prefix"
        prefix.mkdir(mode=0o700)
        env = {key: value for key, value in os.environ.items() if not key.startswith("AGENTDOCKER_")}
        env.update(AGENTDOCKER_NO_AUTOSTART="1", AGENTDOCKER_SOCKET=str(root / "no.sock"),
                   AGENTDOCKER_HOME=str(root / "state"), AGENTDOCKER_NO_NOTIFICATIONS="1")
        # A preview feed for one made-up newer version pointing at the local archive.
        version = manifest["version"]
        major, minor, patch = version.split("+")[0].split("-")[0].split(".")
        newer = f"{major}.{minor}.{int(patch) + 1}"
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        feed = {
            "format": 1, "product": "agentdocker", "channel": "preview",
            "policy": {"check_interval_hours": 24, "download": "manual", "activation": "explicit"},
            "releases": [{"target": target, "version": version, "source_commit": manifest["source_commit"],
                          "state_schema": manifest["state_schema"], "signing": manifest["signing"],
                          "notarized": False,
                          "archive": {"name": archive_name, "sha256": digest, "bytes": archive.stat().st_size,
                                      "url": "file://" + str(archive)}}],
        }
        feed_path = root / "updates.json"
        feed_path.write_text(json.dumps(feed))
        feed_url = "file://" + str(feed_path)

        # 1. A stable feed is refused when it points at file:// without --local-preview.
        cli(binary, prefix, "update", "--feed", feed_url, "--check", env=env, success=False)
        report["steps"].append("file feed refused without local preview")

        # 2. Same version installed nowhere yet: the running CLI is the baseline, so nothing is newer.
        same = cli(binary, prefix, "update", "--feed", feed_url, "--check", "--local-preview", env=env)
        assert same["update"]["update_available"] is False, same
        assert not (prefix / ".local/share/agentdocker/desktop/downloads" / version).exists(), "check downloaded something"
        report["steps"].append("check reports no update for the running version and downloads nothing")

        for invalid in (".", "..", "1.2.3-01"):
            feed["releases"][0]["version"] = invalid
            feed_path.write_text(json.dumps(feed))
            rejected = cli(binary, prefix, "update", "--feed", feed_url, "--local-preview", env=env, success=False)
            assert "semantic version" in rejected, rejected
        assert not (prefix / ".local/share/agentdocker/desktop/downloads").exists()
        report["steps"].append("malformed version paths refused before staging")
        feed["releases"][0]["version"] = version
        feed["releases"].append(dict(feed["releases"][0]))
        feed_path.write_text(json.dumps(feed))
        ambiguous = cli(binary, prefix, "update", "--feed", feed_url, "--check", "--local-preview", env=env, success=False)
        assert "duplicate feed target" in ambiguous, ambiguous
        feed["releases"].pop()
        report["steps"].append("ambiguous duplicate target refused during check")

        # 3. Advertise a newer version but keep the real archive: the check believes the feed, the
        #    download then finds a payload whose own metadata disagrees, and nothing is installed.
        feed["releases"][0]["version"] = newer
        feed_path.write_text(json.dumps(feed))
        check = cli(binary, prefix, "update", "--feed", feed_url, "--check", "--local-preview", env=env)
        assert check["update"]["update_available"] is True and check["update"]["available"]["version"] == newer, check
        report["steps"].append("check sees the advertised newer version")
        mismatch = cli(binary, prefix, "update", "--feed", feed_url, "--local-preview", env=env, success=False)
        assert "does not match the feed entry" in mismatch, mismatch
        assert cli(binary, prefix, "status", env=env)["installation"] is None, "mismatch installed something"
        report["steps"].append("payload whose metadata disagrees with the feed is refused before install")

        # 4. A tampered checksum is refused after download, before extraction.
        feed["releases"][0]["archive"]["sha256"] = "0" * 64
        feed_path.write_text(json.dumps(feed))
        tampered = cli(binary, prefix, "update", "--feed", feed_url, "--local-preview", env=env, success=False)
        assert "does not match the feed" in tampered, tampered
        downloads = prefix / ".local/share/agentdocker/desktop/downloads" / newer
        assert not any(downloads.glob("*.part")), "partial download left behind"
        assert not (downloads / "payload").exists(), "tampered archive was extracted"
        report["steps"].append("tampered checksum refused; no partial file, no extraction")

        feed["releases"][0]["archive"]["url"] = "file://" + str(root / "missing-archive")
        feed_path.write_text(json.dumps(feed))
        failed = cli(binary, prefix, "update", "--feed", feed_url, "--local-preview", env=env, success=False)
        assert "cannot download the update archive" in failed, failed
        assert not any(downloads.glob("*.part")), "failed fetch retained a partial file"
        report["steps"].append("failed archive fetch leaves no partial download")

        # 5. A synthetic newer version: relabel this payload and sign ad hoc so the
        #    installation flow can run. This does not prove a distinct-source upgrade.
        relabel_dir = root / "relabelled"
        relabel_dir.mkdir()
        payload_copy = relabel_dir / PAYLOAD
        subprocess.run(["/usr/bin/ditto", str(source / PAYLOAD), str(payload_copy)] if MAC else ["cp", "-a", str(source / PAYLOAD), str(payload_copy)], check=True)
        meta_path = payload_copy / ("Contents/Resources/build.json" if MAC else "build.json")
        meta = json.loads(meta_path.read_text())
        meta["version"] = newer
        meta_path.write_text(json.dumps(meta, indent=2) + "\n")
        if MAC:
            subprocess.run(["/usr/bin/codesign", "--force", "--deep", "--sign", "-", str(payload_copy)], check=True, capture_output=True)
        relabelled_archive = relabel_dir / archive_name
        if MAC:
            subprocess.run(["/usr/bin/ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(payload_copy), str(relabelled_archive)], check=True)
        else:
            subprocess.run(["tar", "-czf", str(relabelled_archive), "-C", str(relabel_dir), PAYLOAD], check=True)
        feed["releases"][0]["archive"] = {"name": archive_name, "sha256": hashlib.sha256(relabelled_archive.read_bytes()).hexdigest(),
                                          "bytes": relabelled_archive.stat().st_size, "url": "file://" + str(relabelled_archive)}
        feed_path.write_text(json.dumps(feed))
        preview = cli(binary, prefix, "update", "--feed", feed_url, "--local-preview", env=env)
        assert preview["preview"] is True and preview["candidate"]["version"] == newer, preview
        assert preview["update"]["downloaded"].endswith(archive_name), preview["update"]
        assert cli(binary, prefix, "status", env=env)["installation"] is None, "preview installed something"
        report["steps"].append("download, verify, extract and preview of the newer release")
        applied = cli(binary, prefix, "update", "--feed", feed_url, "--local-preview", "--apply", env=env)
        assert applied["preview"] is False, applied
        status = cli(binary, prefix, "status", env=env)
        assert status["installation"]["current"]["version"] == newer, status
        for name in ("agentdocker", "agentd", "agentdocker-ui"):
            link = prefix / ".local/bin" / name
            assert link.is_symlink() and link.resolve().is_file(), f"{name} launcher missing"
        report["steps"].append("apply produced a managed installation of the advertised version")
        again = cli(binary, prefix, "update", "--feed", feed_url, "--check", "--local-preview", env=env)
        assert again["update"]["update_available"] is False and again["update"]["installed_version"] == newer, again
        report["steps"].append("second check finds nothing newer than the installed version")
        report["installed"] = status["installation"]["current"]
        report["passed"] = True
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
