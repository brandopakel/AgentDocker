#!/usr/bin/env python3
"""Package Windows release binaries, then exercise only the extracted archive."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("desktop_package", ROOT / "packaging/desktop/package.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


def extract_checked(archive, destination, manifest):
    """Check the exact portable layout and all executable bytes before running."""
    names = PACKAGE.binary_names("x86_64-pc-windows-msvc")
    expected = {"AgentDocker/" + name for name in names}
    expected.update({"AgentDocker/build.json", "AgentDocker/README.txt",
                     "AgentDocker/licenses/LICENSE-Inter.txt",
                     "AgentDocker/licenses/LICENSE-AgentDocker.txt"})
    if PACKAGE.sha256(archive) != manifest["artifacts"][archive.name]:
        raise ValueError("Windows archive checksum differs from its manifest")
    with zipfile.ZipFile(archive) as bundle:
        if len(bundle.infolist()) != len(expected) or set(bundle.namelist()) != expected:
            raise ValueError("Windows archive does not have the exact portable layout")
        if sum(item.file_size for item in bundle.infolist()) > 100 * 1024 ** 2:
            raise ValueError("Windows extracted payload exceeds the size budget")
        # Copy regular file bytes explicitly; never honor symlinks or traversal.
        for name in sorted(expected):
            path = destination / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with bundle.open(name) as source, path.open("xb") as target:
                shutil.copyfileobj(source, target)
    app = destination / "AgentDocker"
    inside = json.loads((app / "build.json").read_text(encoding="utf-8"))
    for key in ("source_commit", "source_tree", "source_input_sha256", "source_dirty", "target",
                "version", "state_schema", "installation_lock", "launcher_redirect", "signing",
                "distribution", "binary_sha256"):
        if inside.get(key) != manifest.get(key):
            raise ValueError(f"Windows archive build metadata differs: {key}")
    for name in names:
        if PACKAGE.sha256(app / name) != manifest["binary_sha256"][name]:
            raise ValueError(f"Windows archived binary checksum differs: {name}")
    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native-manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("the archive acceptance trial requires native Windows")
    build = json.loads(args.native_manifest.read_text(encoding="utf-8"))
    if build.get("target") != "x86_64-pc-windows-msvc":
        parser.error("the Windows preview currently targets x64 MSVC")
    output = args.output.resolve()
    info = PACKAGE.package(PACKAGE.parser().parse_args([
        "--binary-dir", build["binary_directory"], "--output", str(output),
        "--source", build["source_commit"], "--version", build["version"], "--target", build["target"]]),
        expected_build=build)
    report = {"result": "failed", "source_commit": info["source_commit"],
              "source_input_sha256": info["source_input_sha256"],
              "artifacts": info["artifacts"], "binary_sha256": info["binary_sha256"],
              "scope": "Extracted unsigned x64 portable archive; native private-home daemon/CLI/terminal/desktop smoke, not a clean-machine or actual-provider trial"}
    try:
        archive = output / next(iter(info["artifacts"]))
        # Both a space and Unicode in a location outside the source/build tree.
        with tempfile.TemporaryDirectory(prefix="AgentDocker portable ü ") as scratch:
            app = extract_checked(archive, Path(scratch), info)
            # The original staging payload cannot accidentally satisfy sibling lookup.
            shutil.rmtree(output / "AgentDocker")
            subprocess.run([sys.executable, str(ROOT / "scripts/windows_daemon_smoke.py"),
                            "--binary-dir", str(app), "--output", str(output / "smoke"), "--desktop"],
                           cwd=scratch, check=True)
            observed = json.loads((output / "smoke/windows-daemon-smoke.json").read_text(encoding="utf-8"))
            if observed.get("result") != "passed" or observed.get("binary_sha256") != info["binary_sha256"]:
                raise ValueError("native smoke did not pass on the exact archive binaries")
            report.update(result="passed", steps=len(observed["steps"]), desktop=observed.get("desktop"))
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (output / "package-acceptance.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
