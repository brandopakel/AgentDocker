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


def validate_native_report(native, info, scenario):
    """A passing process must still identify the exact extracted payload."""
    automatic = scenario == "automatic"
    hashes = native.get("receiver_binary_sha256" if automatic else "binary_sha256", {})
    if native.get("result") != "passed" or any(
        hashes.get(name) != info["binary_sha256"][name]
        for name in ("agentdocker.exe", "agentd.exe")
    ):
        raise ValueError("native Codex acceptance did not pass on the exact archive binaries")
    if automatic and (
        native.get("source_commit") != info["source_commit"]
        or native.get("scratch_removed") is not True
        or any(native.get(key) != [] for key in ("cleanup_errors", "reader_errors", "forced_processes"))
        or len(native.get("steps", [])) != 13
        or not all(step.get("passed") is True for step in native["steps"])
    ):
        raise ValueError("automatic native Codex acceptance has incomplete source, lifecycle or cleanup evidence")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native-manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--codex", type=Path,
                        help="actual native Codex executable for private ConPTY/loopback receiver acceptance")
    parser.add_argument("--codex-scenario", choices=("startup", "established", "automatic"), default="startup",
                        help="automatic tests the product-owned launcher; startup retains the strict direct-Codex diagnostic")
    parser.add_argument("--startup-samples", type=int, default=0,
                        help="additional fresh-home samples per Windows ancestry type (0..20)")
    parser.add_argument("--service", action="store_true",
                        help="also test the private home's owned Task Scheduler lifecycle")
    args = parser.parse_args()
    if not 0 <= args.startup_samples <= 20:
        parser.error("--startup-samples must be between 0 and 20")
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
                            "--binary-dir", str(app), "--output", str(output / "smoke"), "--desktop",
                            "--startup-samples", str(args.startup_samples)],
                           cwd=scratch, check=True)
            observed = json.loads((output / "smoke/windows-daemon-smoke.json").read_text(encoding="utf-8"))
            if observed.get("result") != "passed" or observed.get("binary_sha256") != info["binary_sha256"]:
                raise ValueError("native smoke did not pass on the exact archive binaries")
            if args.service:
                subprocess.run([sys.executable, str(ROOT / "scripts/windows_service_smoke.py"),
                                "--binary-dir", str(app), "--output", str(output / "service")],
                               cwd=scratch, check=True, timeout=600)
                service = json.loads((output / "service/result.json").read_text(encoding="utf-8"))
                if service.get("result") != "passed" or any(
                    service.get("binary_sha256", {}).get(name) != info["binary_sha256"][name]
                    for name in ("agentdocker.exe", "agentd.exe")
                ):
                    raise ValueError("service lifecycle did not pass on the exact archive binaries")
                report["service"] = {"result": "passed", "steps": len(service["steps"])}
                subprocess.run([sys.executable, str(ROOT / "scripts/windows_connector_service_smoke.py"),
                                "--binary-dir", str(app), "--output", str(output / "connector-service")],
                               cwd=scratch, check=True, timeout=600)
                connector = json.loads((output / "connector-service/result.json").read_text(encoding="utf-8"))
                if connector.get("result") != "passed" or connector.get("cleanup_errors") or any(
                    connector.get("binary_sha256", {}).get(name) != info["binary_sha256"][name]
                    for name in ("agentdocker.exe", "agentd.exe")
                ):
                    raise ValueError("connector service did not pass on the exact archive binaries")
                report["connector_service"] = {"result": "passed", "steps": len(connector["steps"])}
            subprocess.run([sys.executable, str(ROOT / "scripts/windows_install_smoke.py"),
                            "--binary-dir", str(app), "--output", str(output / "installation"),
                            *(["--service"] if args.service else [])],
                           cwd=scratch, check=True, timeout=900)
            installed = json.loads((output / "installation/result.json").read_text(encoding="utf-8"))
            if installed.get("result") != "passed" or installed.get("binary_sha256") != info["binary_sha256"]:
                raise ValueError("installation trial did not pass on the exact archive binaries")
            report["installation"] = {"result": "passed", "steps": len(installed["steps"])}
            report.update(result="passed", steps=len(observed["steps"]), desktop=observed.get("desktop"))
            if args.codex:
                report["result"] = "failed"
                import psutil
                automatic = args.codex_scenario == "automatic"
                driver = "windows_native_launcher_smoke.py" if automatic else "windows_native_codex_smoke.py"
                trial = subprocess.Popen([sys.executable, str(ROOT / "scripts" / driver),
                                          "--binary-dir", str(app), "--codex", str(args.codex.resolve(strict=True)),
                                          *([] if automatic else ["--scenario", args.codex_scenario]),
                                          "--output", str(output / "native-codex")], cwd=scratch)
                owner = psutil.Process(trial.pid)
                try:
                    if trial.wait(timeout=300) != 0:
                        raise ValueError("native Codex acceptance failed; see its retained report")
                except subprocess.TimeoutExpired:
                    children = owner.children(recursive=True)
                    for process in reversed(children):
                        try:
                            process.kill()
                        except psutil.NoSuchProcess:
                            pass
                    owner.kill()
                    _, alive = psutil.wait_procs([owner, *children], timeout=5)
                    report["native_codex_timeout_survivors"] = [p.pid for p in alive]
                    trial.wait(timeout=5)
                    raise
                native = json.loads((output / "native-codex/result.json").read_text(encoding="utf-8"))
                validate_native_report(native, info, args.codex_scenario)
                report.update(result="passed", native_codex=native, native_codex_scenario=args.codex_scenario)
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (output / "package-acceptance.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
