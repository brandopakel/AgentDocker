"""Fault-test campaign orchestration without running a performance workload."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class BenchmarkCampaign(unittest.TestCase):
    def test_failed_workload_retains_identity_and_runs_each_remaining_scenario_once(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "scripts").mkdir()
            shutil.copy2(Path(__file__).parents[1] / "scripts/verify.sh", root / "scripts/verify.sh")
            (root / "scripts/benchmark_manifest.py").write_text('print(\'{"source":"fixture"}\')\n')
            binary = root / "bin"
            binary.mkdir()
            cargo = binary / "cargo"
            cargo.write_text("#!/bin/sh\nexit 0\n")
            cargo.chmod(0o700)
            target = root / "target/release/examples"
            target.mkdir(parents=True)
            workload = target / "socket_load"
            workload.write_text("#!/bin/sh\nprintf '%s %s\\n' \"$2\" \"$4\" >> calls\n"
                                "if [ \"$2 $4\" = '100 shared' ]; then echo 'fixture timeout' >&2; exit 7; fi\n"
                                "echo '{}'\n")
            workload.chmod(0o700)
            (root / "artifacts").mkdir()
            stale = root / "artifacts/socket-disjoint-100.json"
            stale.write_text("stale outcome")
            result = subprocess.run(["bash", "scripts/verify.sh", "bench"], cwd=root,
                env={**os.environ, "PATH": str(binary) + os.pathsep + os.environ["PATH"]},
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertIn("fixture timeout", result.stderr)
            self.assertEqual((root / "calls").read_text().splitlines(),
                ["1 shared", "1 disjoint", "10 shared", "10 disjoint", "100 shared", "100 disjoint"])
            self.assertEqual(stale.read_text(), "{}\n")
            self.assertIn("shared\t100\t7", (root / "artifacts/benchmark-status.tsv").read_text())
            self.assertEqual((root / "artifacts/benchmark-manifest.json").read_bytes(),
                             (root / "artifacts/benchmark-manifest-after.json").read_bytes())
