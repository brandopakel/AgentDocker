#!/usr/bin/env python3
"""Packaged hook input bounds; no provider, credentials or running daemon."""
import argparse
import hashlib
import json
import os
import signal
import subprocess
import tempfile
import time
from pathlib import Path


def run(binary, manifest, output):
    os.umask(0o077)
    output.mkdir(mode=0o700)
    metadata = json.loads(manifest.read_text())
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    assert digest == metadata["binary_sha256"]["agentdocker"]
    result = {"source_commit": metadata["source_commit"],
              "source_tree": metadata["source_tree"], "binary_sha256": digest,
              "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "scope": "Owned hook input fixtures, no provider or production daemon",
              "checks": [], "result": "failed"}
    children = []
    scratch = None
    try:
        with tempfile.TemporaryDirectory(prefix="ad-hook-input-", dir="/tmp") as directory:
            scratch = Path(directory).resolve()
            env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTDOCKER_")}
            env.update(AGENTDOCKER_HOME=str(scratch / "state"),
                       AGENTDOCKER_SOCKET=str(scratch / "absent.sock"),
                       AGENTDOCKER_NO_AUTOSTART="1")
            for provider in ["claude-code", "codex"]:
                for scenario in ["held-open", "oversized"]:
                    with tempfile.TemporaryFile() as source, tempfile.TemporaryFile() as errors:
                        source.write(b" " * (1024 * 1024 + 1))
                        source.seek(0)
                        child = subprocess.Popen(
                            [str(binary), "hook", provider], env=env, cwd=scratch,
                            stdin=subprocess.PIPE if scenario == "held-open" else source,
                            stdout=subprocess.DEVNULL, stderr=errors, start_new_session=True)
                        children.append(child)
                        started = time.monotonic()
                        try:
                            # The writer stays open until after the hook exits.
                            code = child.wait(timeout=3)
                        finally:
                            if child.returncode is None:
                                # The unreaped direct child reserves this PGID.
                                try:
                                    os.killpg(child.pid, signal.SIGKILL)
                                except ProcessLookupError:
                                    pass
                                child.wait(timeout=5)
                            if child.stdin is not None:
                                child.stdin.close()
                        errors.seek(0)
                        diagnostic = errors.read(4097)
                        expected = b"timed out" if scenario == "held-open" else b"exceeds 1 MiB"
                        assert code == 0 and expected in diagnostic and len(diagnostic) <= 4096
                        result["checks"].append({"provider": provider, "scenario": scenario,
                                                 "seconds": time.monotonic() - started,
                                                 "exit": code, "bounded_diagnostic": True})
            assert not (scratch / "state").exists(), "hook unexpectedly started a daemon"
            result["result"] = "passed"
    except Exception as error:
        result["error"] = str(error)
    finally:
        result["cleanup"] = {"owned_children_remaining": sum(p.returncode is None for p in children),
                             "scratch_removed": scratch is None or not scratch.exists()}
        (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 0 if result["result"] == "passed" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(run(args.binary.resolve(strict=True), args.manifest.resolve(strict=True), args.output))
