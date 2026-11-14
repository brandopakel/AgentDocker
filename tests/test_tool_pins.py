"""The development tools are pinned in one place, scripts/tool_pins.txt,
and the Rust they run on in another, rust-toolchain.toml. Every other
statement of a version (a workflow's install step, the testing standard)
must say the same, and verify.sh stops before the build slot and before
anything is built when a tool its mode needs is missing."""
import os
import re
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOOL = re.compile(r"\bcargo-[a-z][a-z-]*[a-z]\b")
VERSION = re.compile(r"--version[ =]([0-9][0-9A-Za-z.+-]*)")


def installs(text):
    """Every `cargo install` in a text, however its options are ordered:
    the tool it names and the version it gives, None when it gives none."""
    found = []
    for line in text.splitlines():
        for command in line.split("cargo install")[1:]:
            tool = TOOL.search(command)
            if tool:
                version = VERSION.search(command)
                found.append((tool.group(0), version.group(1) if version else None))
    return found


def toolchain_steps(text):
    """Every step of a workflow that installs a Rust toolchain: the
    version it names and the components it asks for, in file order."""
    found = []
    lines = text.splitlines()
    for at, line in enumerate(lines):
        if "dtolnay/rust-toolchain@" not in line:
            continue
        step = len(line) - len(line.lstrip())
        version, components = None, []
        for inner in lines[at + 1:]:
            indent = len(inner) - len(inner.lstrip())
            if inner.strip() and indent <= step:
                break
            key, _, value = inner.strip().partition(":")
            if key == "toolchain":
                version = value.strip().strip("\"'")
            elif key == "components":
                components = [name.strip() for name in value.split(",") if name.strip()]
        found.append((version, components))
    return found


def pins():
    found = {}
    for line in (ROOT / "scripts/tool_pins.txt").read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            tool, version = line.split()
            found[tool] = version
    return found


class ToolPins(unittest.TestCase):
    def test_every_stated_install_names_the_pinned_version(self):
        pinned = pins()
        self.assertTrue(pinned, "scripts/tool_pins.txt pins nothing")
        places = sorted((ROOT / ".github/workflows").glob("*.yml")) + [ROOT / "docs/TESTING-AND-BENCHMARKS.md"]
        stated = set()
        for place in places:
            for tool, version in installs(place.read_text()):
                stated.add(tool)
                self.assertIn(tool, pinned, f"{place.relative_to(ROOT)} installs {tool}, which scripts/tool_pins.txt does not pin")
                self.assertEqual(version, pinned[tool], f"{place.relative_to(ROOT)} installs {tool} at {version}; scripts/tool_pins.txt pins {pinned[tool]}")
        self.assertEqual(stated, set(pinned), "a pinned tool that nothing installs, or the reverse")

    def test_every_workflow_installs_the_rust_the_toolchain_file_names(self):
        # One Rust for the local gates and for CI: a workflow that asked
        # for `stable` would build with whatever was released last, and a
        # new release could fail main with no change of ours.
        pinned = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]
        channel, wanted = pinned["channel"], pinned["components"]
        self.assertRegex(channel, r"^[0-9]+\.[0-9]+\.[0-9]+$", "rust-toolchain.toml names one version, not a channel that moves")
        seen = 0
        for place in sorted((ROOT / ".github/workflows").glob("*.yml")):
            for version, components in toolchain_steps(place.read_text()):
                where = place.relative_to(ROOT)
                if version == "nightly":
                    continue
                seen += 1
                self.assertEqual(version, channel, f"{where} installs Rust {version}; rust-toolchain.toml names {channel}")
                for component in wanted:
                    self.assertIn(component, components,
                                  f"{where} installs Rust {version} without {component}, which rust-toolchain.toml lists: "
                                  "the first Cargo run would have to fetch it")
        self.assertTrue(seen, "no workflow installs the pinned Rust")

    def test_a_toolchain_step_is_read_with_its_version_and_components(self):
        text = ("      - uses: dtolnay/rust-toolchain@abc # master\n"
                "        with:\n"
                "          toolchain: \"1.2.3\"\n"
                "          components: llvm-tools-preview, rustfmt, clippy\n"
                "          targets: ${{ matrix.target }}\n"
                "      - uses: dtolnay/rust-toolchain@abc\n"
                "        if: inputs.flag\n"
                "        with:\n"
                "          toolchain: nightly\n"
                "      - run: cargo build\n"
                "        with:\n"
                "          toolchain: not-a-toolchain-step\n")
        self.assertEqual(toolchain_steps(text),
                         [("1.2.3", ["llvm-tools-preview", "rustfmt", "clippy"]), ("nightly", [])])

    def test_an_install_is_found_however_its_options_are_ordered(self):
        self.assertEqual(installs("run: cargo install cargo-nextest --locked --version 0.9.142"),
                         [("cargo-nextest", "0.9.142")])
        self.assertEqual(installs("`cargo install --locked cargo-fuzz --version=0.13.2`, and `cargo install cargo-llvm-cov`"),
                         [("cargo-fuzz", "0.13.2"), ("cargo-llvm-cov", None)])

    def run_verify(self, mode, tools, aliased=""):
        """verify.sh in a folder of its own, with a stand-in cargo that
        records being run and only the named tools beside it."""
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/verify.sh", root / "scripts/verify.sh")
            shutil.copy2(ROOT / "scripts/tool_pins.txt", root / "scripts/tool_pins.txt")
            (root / "scripts/build_storage.py").write_text("print('{}')\n")
            binary = root / "bin"; binary.mkdir()
            ran = root / "cargo-ran"
            cargo = binary / "cargo"
            # As Cargo does: a subcommand it cannot find fails when asked
            # for its version (unless this fixture says an alias reaches it).
            cargo.write_text("#!/bin/sh\n"
                             "if [ \"$2\" = --version ]; then [ \"$1\" = \"$CARGO_ALIASED\" ] && exit 0; exit 101; fi\n"
                             f"echo \"$@\" >> '{ran}'\n")
            cargo.chmod(0o700)
            claimed = root / "slot-asked"
            cli = binary / "agentdocker"
            cli.write_text(f"#!/bin/sh\necho \"$@\" >> '{claimed}'\nexit 1\n")
            cli.chmod(0o700)
            for tool in tools:
                stub = binary / tool
                stub.write_text("#!/bin/sh\n"); stub.chmod(0o700)
            # Only the stand-ins and the system's own tools: a cargo-nextest
            # installed on this machine must not be found.
            env = {"PATH": f"{binary}:/usr/bin:/bin", "HOME": str(root), "CARGO_HOME": str(root / "no-cargo-home"),
                   "CARGO_ALIASED": aliased}
            result = subprocess.run(["bash", "scripts/verify.sh", mode], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=30)
            return result, ran.exists(), claimed.exists()

    def test_a_tool_only_cargo_can_reach_counts(self):
        # Installed outside PATH and Cargo's bin directory, reached through
        # an alias in Cargo's configuration: only Cargo can say it is there.
        result, cargo_ran, _ = self.run_verify("test", [], aliased="nextest")
        self.assertNotEqual(result.returncode, 69, result.stderr)
        self.assertNotIn("is not installed", result.stderr)
        self.assertTrue(cargo_ran, "the run went on to its cargo steps")

    def test_a_missing_tool_stops_the_run_before_the_slot_and_before_anything_is_built(self):
        for mode in ("check", "quick", "test"):
            result, cargo_ran, slot_asked = self.run_verify(mode, [])
            self.assertEqual(result.returncode, 69, result.stderr)
            self.assertIn(f"cargo install --locked cargo-nextest --version {pins()['cargo-nextest']}", result.stderr)
            self.assertIn("the build slot was not taken", result.stderr)
            self.assertFalse(cargo_ran, f"{mode}: cargo ran without its tools")
            self.assertFalse(slot_asked, f"{mode}: the build slot was asked for without its tools")

    def test_every_missing_tool_of_a_mode_is_named_at_once(self):
        result, cargo_ran, _ = self.run_verify("coverage", [])
        self.assertEqual(result.returncode, 69, result.stderr)
        for tool in ("cargo-nextest", "cargo-llvm-cov"):
            self.assertIn(f"cargo install --locked {tool} --version {pins()[tool]}", result.stderr)
        self.assertFalse(cargo_ran)
        result, cargo_ran, _ = self.run_verify("fuzz", [])
        self.assertEqual(result.returncode, 69, result.stderr)
        self.assertIn(f"cargo install --locked cargo-fuzz --version {pins()['cargo-fuzz']}", result.stderr)
        self.assertIn("rustup toolchain install nightly", result.stderr)
        self.assertFalse(cargo_ran)

    def test_fuzzing_without_the_nightly_toolchain_stops_before_the_slot(self):
        # cargo-fuzz is there; the toolchain its campaigns build with is not.
        result, cargo_ran, slot_asked = self.run_verify("fuzz", ["cargo-fuzz"])
        self.assertEqual(result.returncode, 69, result.stderr)
        self.assertIn("the nightly toolchain is not installed", result.stderr)
        self.assertIn("rustup toolchain install nightly", result.stderr)
        self.assertNotIn("cargo install", result.stderr)
        self.assertIn("the build slot was not taken", result.stderr)
        self.assertFalse(cargo_ran, "fuzz: cargo ran without the nightly toolchain")
        self.assertFalse(slot_asked, "fuzz: the build slot was asked for without the nightly toolchain")
        # With both, the preflight passes and the run goes on to the slot.
        result, _, slot_asked = self.run_verify("fuzz", ["cargo-fuzz"], aliased="+nightly")
        self.assertNotEqual(result.returncode, 69, result.stderr)
        self.assertNotIn("is not installed", result.stderr)

    def test_a_tool_in_cargos_own_bin_directory_counts_and_the_run_goes_on(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/verify.sh", root / "scripts/verify.sh")
            (root / "scripts/build_storage.py").write_text("print('{}')\n")
            binary = root / "bin"; binary.mkdir()
            ran = root / "cargo-ran"
            cargo = binary / "cargo"
            cargo.write_text(f"#!/bin/sh\necho \"$@\" >> '{ran}'\n"); cargo.chmod(0o700)
            home = root / "cargo-home/bin"; home.mkdir(parents=True)
            stub = home / "cargo-nextest"; stub.write_text("#!/bin/sh\n"); stub.chmod(0o700)
            env = {"PATH": f"{binary}:/usr/bin:/bin", "HOME": str(root), "CARGO_HOME": str(root / "cargo-home"),
                   "AGENTDOCKER_CAMPAIGN_LEASE": "off"}
            result = subprocess.run(["bash", "scripts/verify.sh", "test"], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(ran.read_text().splitlines()), 2, "nextest and the doctests")

    def test_the_gates_resolve_the_fuzz_lockfile_before_they_compile(self):
        for mode in ("check", "quick"):
            with tempfile.TemporaryDirectory() as folder:
                root = Path(folder).resolve()
                (root / "scripts").mkdir()
                shutil.copy2(ROOT / "scripts/verify.sh", root / "scripts/verify.sh")
                (root / "scripts/build_storage.py").write_text("print('{}')\n")
                (root / "scripts/docs_check.py").write_text("")
                (root / "scripts/touched_crates.py").write_text(
                    "import sys\nopen(sys.argv[sys.argv.index('--output') + 1], 'w').write('-p\\nfixture\\n')\n")
                (root / "tests").mkdir()
                # The gate ends with the Python suite, which must find a test.
                (root / "tests/test_fixture.py").write_text(
                    "import unittest\nclass Fixture(unittest.TestCase):\n    def test_it(self):\n        pass\n")
                binary = root / "bin"; binary.mkdir()
                ran = root / "cargo-ran"
                cargo = binary / "cargo"
                cargo.write_text(f"#!/bin/sh\necho \"$@\" >> '{ran}'\n"); cargo.chmod(0o700)
                stub = binary / "cargo-nextest"; stub.write_text("#!/bin/sh\n"); stub.chmod(0o700)
                env = {"PATH": f"{binary}:{os.environ['PATH']}", "HOME": str(root),
                       "AGENTDOCKER_CAMPAIGN_LEASE": "off"}
                result = subprocess.run(["bash", "scripts/verify.sh", mode], cwd=root, env=env,
                                        capture_output=True, text=True, timeout=60)
                self.assertEqual(result.returncode, 0, f"{mode}: {result.stderr}")
                calls = ran.read_text().splitlines()
                lock = "metadata --locked --manifest-path fuzz/Cargo.toml --format-version 1"
                self.assertIn(lock, calls, f"{mode}: {calls}")
                compiling = [i for i, call in enumerate(calls) if call.startswith(("clippy", "nextest", "test"))]
                self.assertTrue(compiling, f"{mode}: {calls}")
                self.assertLess(calls.index(lock), compiling[0], f"{mode}: the lock is resolved after a compile step")


if __name__ == "__main__":
    unittest.main()
