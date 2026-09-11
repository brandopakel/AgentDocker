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
    report = {"passed": False, "source": str(source), "target": target, "steps": []}
    with tempfile.TemporaryDirectory(prefix="ad-update-", dir="/tmp") as scratch:
        root = Path(scratch)
        prefix = root / "prefix"
        prefix.mkdir(mode=0o700)
        env = {**os.environ, "AGENTDOCKER_NO_AUTOSTART": "1", "AGENTDOCKER_SOCKET": str(root / "no.sock")}
        # A preview feed for one made-up newer version pointing at the local archive.
        version = manifest["version"]
        major, minor, patch = version.split("+")[0].split("-")[0].split(".")
        newer = f"{major}.{minor}.{int(patch) + 1}"
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        feed = {
            "format": 1, "product": "agentdocker", "channel": "preview",
            "policy": {"check_interval_hours": 24, "download": "manual", "activation": "explicit"},
            "releases": [{"target": target, "version": version, "source_commit": manifest["source_commit"],
                          "state_schema": manifest["state_schema"], "signing": manifest.get("signing", "ad-hoc"),
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

        # 5. An honest newer release: the same payload relabelled to the newer version (its build.json
        #    edited and re-signed ad hoc) so the feed, the archive and the payload agree.
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
    (args.output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
