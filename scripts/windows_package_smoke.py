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
        or len(native.get("steps", [])) != 22
        or not all(step.get("passed") is True for step in native["steps"])
    ):
        raise ValueError("automatic native Codex acceptance has incomplete source, lifecycle or cleanup evidence")
    for probe in ("front_end_exit", "owner_exit") if automatic else ():
        exited = native.get(probe, {})
        if (not isinstance(exited, dict) or len(exited.get("watched", [])) < 3
                or exited.get("remaining") != []
                or exited.get("capability_revoked") is not True
                or exited.get("receipts_preserved") is not True
                or exited.get("additional_model_requests") != 0):
            raise ValueError(f"automatic native Codex acceptance lacks {probe} evidence")
        target = exited.get("target")
        watched = exited.get("watched", [])
        if (not isinstance(target, dict) or not isinstance(target.get("pid"), int)
                or target["pid"] <= 0 or not isinstance(target.get("birth"), (int, float))
                or not isinstance(exited.get("frontend_exit_code"), int)
                or exited["frontend_exit_code"] == 0
                or (probe == "front_end_exit" and
                    (target["pid"] != exited.get("pid") or target["birth"] != exited.get("birth")))
                or (probe == "owner_exit" and
                    (target["pid"] == exited.get("pid") or target not in watched
                     or {"pid": exited.get("pid"), "birth": exited.get("birth")} not in watched))):
            raise ValueError("automatic native Codex acceptance lacks distinct pinned exit targets")
        host = exited.get("console_host")
        if (not isinstance(host, dict) or not isinstance(host.get("pid"), int)
                or host["pid"] <= 0 or host["pid"] in (exited.get("pid"), target["pid"])
                or host.get("alive_after_cleanup") is not True
                or host.get("resize_after_cleanup") is not True):
            raise ValueError("automatic native Codex exit acceptance did not keep its console open")
    if automatic:
        closed = native.get("console_close", {})
        host = closed.get("console_host", {}) if isinstance(closed, dict) else {}
        watched = closed.get("watched") if isinstance(closed, dict) else None
        if (not isinstance(closed, dict)
                or closed.get("trigger") != "drop_final_conpty_owner"
                or closed.get("pty_references_before_release") != 2
                or closed.get("participants_alive_before_release") is not True
                or closed.get("conpty_owner_released") is not True
                or closed.get("remaining") != []
                or closed.get("capability_revoked") is not True
                or closed.get("receipts_preserved") is not True
                or closed.get("additional_model_requests") != 0
                or not isinstance(host, dict)
                or not isinstance(watched, list) or len(watched) < 5
                or host.get("alive_after_cleanup") is not False
                or not isinstance(closed.get("pid"), int)
                or not isinstance(host.get("pid"), int)
                or closed["pid"] <= 0 or host["pid"] <= 0 or closed["pid"] == host["pid"]
                or {"pid": closed["pid"], "birth": closed.get("birth")} not in watched
                or {"pid": host["pid"], "birth": host.get("birth")} not in watched):
            raise ValueError("automatic native Codex acceptance lacks whole-console closure evidence")


def run_native_trial(command, cwd, report, label):
    """Bound one owned fixture and retain timeout cleanup as a failure."""
    import psutil
    trial = subprocess.Popen(command, cwd=cwd)
    owner = psutil.Process(trial.pid)
    try:
        if trial.wait(timeout=300) != 0:
            raise ValueError(f"{label} acceptance failed; see its retained report")
    except subprocess.TimeoutExpired:
        children = owner.children(recursive=True)
        for process in reversed(children):
            try:
                process.kill()
            except psutil.NoSuchProcess:
                pass
        owner.kill()
        _, alive = psutil.wait_procs([owner, *children], timeout=5)
        report[label + "_timeout_survivors"] = [p.pid for p in alive]
        trial.wait(timeout=5)
        raise


def validate_queue_recovery_report(native, info, mode):
    def require(ok):
        if not ok:
            raise ValueError("queued recovery acceptance lacks exact source, receipts, refusals or cleanup")

    require(mode in ("normal", "holds") and native.get("result") == "passed"
            and native.get("source_commit") == info["source_commit"]
            and native.get("scratch_removed") is True
            and all(native.get(key) == [] for key in ("cleanup_errors", "reader_errors", "forced_processes"))
            and not native.get("fixture_errors")
            and len(native.get("recovery_processes", [])) >= 4
            and native.get("recovery_remaining") == []
            and len(native.get("steps", [])) == 9
            and all(s.get("passed") is True for s in native["steps"])
            and all(native.get("receiver_binary_sha256", {}).get(n) == info["binary_sha256"][n]
                    for n in ("agentdocker.exe", "agentd.exe")))
    proof = native.get("queued_recovery", {})
    require(proof.get("mode") == mode and proof.get("hold_seconds", 0) >= 30)
    expected = (["provider_rate_hold", "project_pause"] if mode == "holds" else
                ["wrong_digest", "wrong_message", "manual_read_is_not_start", "foreign_head"])
    require([r.get("case") for r in proof.get("refusals", [])] == expected)
    before = proof.get("ledger_before", {})
    require(len(before.get("completed", [])) == 2 and isinstance(before.get("attempt"), dict)
            and before["attempt"].get("message") == proof.get("message")
            and bool(before["attempt"].get("queued"))
            and before["attempt"].get("start") is None and before["attempt"].get("receipt") is None)
    for refusal in proof["refusals"]:
        require(refusal.get("reply", {}).get("exit_code") not in (None, 0)
                and refusal.get("ledger_after", {}).get("attempt") == before["attempt"]
                and refusal.get("ledger_after", {}).get("completed") == before["completed"])
    rows = proof.get("completed_after", [])
    require(len(rows) == (2 if mode == "holds" else 3) and rows[:2] == before["completed"])
    if mode == "normal":
        require(proof.get("response", {}).get("exit_code") == 0 and proof.get("repeat", {}).get("exit_code") == 0)
        first, repeat = (json.loads(proof[k]["stdout"]) for k in ("response", "repeat"))
        require(first.get("already_attempted") is False and repeat.get("already_attempted") is True
                and first.get("message") == proof.get("message") and bool(first.get("queued_start"))
                and bool(first.get("turn")) and all(first.get(k) == repeat.get(k) for k in ("message", "queued_start", "turn"))
                and rows[-1]["message"] == first["message"] and rows[-1]["receipt"]["turn"] == first["turn"]
                and sum(bool(r.get("recovery")) and not r.get("title") for r in native.get("requests", [])) == 1)
    else:
        require("daemon is holding this input" in proof["refusals"][0]["reply"].get("stderr", "")
                and "project is paused" in proof["refusals"][1]["reply"].get("stderr", "")
                and proof.get("paused_preview", {}).get("exit_code") == 0
                and not any(r.get("recovery") for r in native.get("requests", [])))
        require(any(p.get("project") == proof.get("pause", {}).get("pause", {}).get("project")
                    for p in proof.get("pauses_after", {}).get("pauses", [])))
        pending = json.loads(proof["paused_preview"]["stdout"])["pending"]
        original = json.loads(proof["preview"]["stdout"])["pending"]
        require(pending["message"] == proof["message"] and pending["start_intent"] is None
                and pending["start_confirmation"] == original["start_confirmation"])
    users = [v for v in proof.get("final_history", {}).get("data", []) if v.get("item", {}).get("type") == "userMessage"]
    require(len(users) == len(rows))
    for row in rows:
        matching = [v for v in users if v["item"].get("clientId") == row["message"]]
        require(len(matching) == 1)
        item = matching[0]
        require(row["receipt"] == {"thread": native["descriptor"]["provider"]["session"],
                                   "turn": item["turnId"], "item": item["item"]["id"]})


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
    parser.add_argument("--queue-recovery", action="store_true",
                        help="with automatic Codex acceptance, test explicit queued start and daemon holds")
    args = parser.parse_args()
    if not 0 <= args.startup_samples <= 20:
        parser.error("--startup-samples must be between 0 and 20")
    if args.queue_recovery and (not args.codex or args.codex_scenario != "automatic"):
        parser.error("--queue-recovery requires --codex with --codex-scenario automatic")
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
                automatic = args.codex_scenario == "automatic"
                driver = "windows_native_launcher_smoke.py" if automatic else "windows_native_codex_smoke.py"
                run_native_trial([sys.executable, str(ROOT / "scripts" / driver),
                                          "--binary-dir", str(app), "--codex", str(args.codex.resolve(strict=True)),
                                          *([] if automatic else ["--scenario", args.codex_scenario]),
                                          "--output", str(output / "native-codex")], scratch, report, "native_codex")
                native = json.loads((output / "native-codex/result.json").read_text(encoding="utf-8"))
                validate_native_report(native, info, args.codex_scenario)
                if args.queue_recovery:
                    report["queued_recovery"] = {}
                    for mode in ("normal", "holds"):
                        destination = output / ("queued-recovery-" + mode)
                        run_native_trial([sys.executable, str(ROOT / "scripts/windows_native_launcher_smoke.py"),
                                          "--binary-dir", str(app), "--codex", str(args.codex.resolve(strict=True)),
                                          "--queued-recovery", mode, "--output", str(destination)],
                                         scratch, report, "queued_recovery_" + mode)
                        observed_recovery = json.loads((destination / "result.json").read_text(encoding="utf-8"))
                        validate_queue_recovery_report(observed_recovery, info, mode)
                        report["queued_recovery"][mode] = observed_recovery
                report.update(result="passed", native_codex=native, native_codex_scenario=args.codex_scenario)
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (output / "package-acceptance.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
