"""Fault-test campaign orchestration without running a performance workload."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class BenchmarkCampaign(unittest.TestCase):
    def test_failed_workload_retains_identity_and_runs_each_remaining_scenario_once(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "scripts").mkdir()
            shutil.copy2(Path(__file__).parents[1] / "scripts/verify.sh", root / "scripts/verify.sh")
            # Storage admission has its own refusal regression. This fixture
            # tests outcome orchestration after a successful admission.
            (root / "scripts/build_storage.py").write_text("print('{}')\n")
            (root / "scripts/benchmark_manifest.py").write_text('print(\'{"source":"fixture"}\')\n')
            binary = root / "bin"
            binary.mkdir()
            cargo = binary / "cargo"
            cargo.write_text("#!/usr/bin/env python3\nimport json, os, pathlib, sys\n"
                "root = pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'release'\n"
                "for flag, name, path in [('--bin', 'agentd', root / 'agentd'), "
                "('--example', 'socket_load', root / 'examples/socket_load')]:\n"
                "    if flag in sys.argv:\n"
                "        print(json.dumps({'reason': 'compiler-artifact', 'target': {'name': name}, 'executable': str(path)}))\n")
            cargo.chmod(0o700)
            build = root / "custom build"
            target = build / "release/examples"
            target.mkdir(parents=True)
            (target.parent / "agentd").write_text("fixture daemon")
            workload = target / "socket_load"
            workload.write_text("#!/bin/sh\n[ \"$1\" = \"$CARGO_TARGET_DIR/release/agentd\" ] || exit 8\n"
                                "printf '%s %s\\n' \"$2\" \"$4\" >> calls\n"
                                "if [ \"$2 $4\" = '100 shared' ]; then echo 'fixture timeout' >&2; exit 7; fi\n"
                                "echo '{}'\n")
            workload.chmod(0o700)
            (root / "artifacts").mkdir()
            stale = root / "artifacts/socket-disjoint-100.json"
            stale.write_text("stale outcome")
            result = subprocess.run(["bash", "scripts/verify.sh", "bench"], cwd=root,
                env={**os.environ, "PATH": str(binary) + os.pathsep + os.environ["PATH"],
                     "CARGO_TARGET_DIR": str(build)},
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertIn("fixture timeout", result.stderr)
            self.assertEqual((root / "calls").read_text().splitlines(),
                ["1 shared", "1 disjoint", "10 shared", "10 disjoint", "100 shared", "100 disjoint"])
            self.assertEqual(stale.read_text(), "{}\n")
            self.assertIn("shared\t100\t7", (root / "artifacts/benchmark-status.tsv").read_text())
            self.assertEqual((root / "artifacts/benchmark-manifest.json").read_bytes(),
                             (root / "artifacts/benchmark-manifest-after.json").read_bytes())
            binaries = root / "artifacts/benchmark-binaries.json"
            self.assertEqual(json.loads(binaries.read_text())[0]["path"], str(target.parent / "agentd"))
            self.assertEqual(binaries.read_bytes(), (root / "artifacts/benchmark-binaries-after.json").read_bytes())
