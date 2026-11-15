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
    version it names and the components it asks for, in file order. Both
    are read from the step's own `with` mapping and nowhere else, so an
    `env` of the step, or a later step, cannot stand in for an input."""
    found = []
    lines = text.splitlines()

    def column(line):
        return len(line) - len(line.lstrip())

    def blank(line):
        return not line.strip() or line.lstrip().startswith("#")

    for at, line in enumerate(lines):
        if "dtolnay/rust-toolchain@" not in line or blank(line):
            continue
        # The column the step's keys sit at: two past its dash.
        keys = column(line) + 2 if line.lstrip().startswith("- ") else column(line)
        start = at
        while start > 0 and not (column(lines[start]) == keys - 2 and lines[start].lstrip().startswith("- ")):
            start -= 1
        step = [lines[start].replace("- ", "  ", 1)]
        for inner in lines[start + 1:]:
            if not blank(inner) and column(inner) < keys:
                break
            step.append(inner)
        version, components, inside, inputs = None, [], False, None
        for inner in step:
            if blank(inner):
                continue
            if column(inner) == keys:
                inside = inner.strip() == "with:"
                continue
            if not inside:
                continue
            inputs = column(inner) if inputs is None else inputs
            if column(inner) != inputs:
                continue
            key, _, value = inner.strip().partition(":")
            if key == "toolchain":
                version = value.strip().strip("\"'")
            elif key == "components":
                components = [name.strip() for name in value.split(",") if name.strip()]
        found.append((version, components))
    return found


def uncommented(line):
    """A line of a workflow without its comment: a `#` that opens the line
    or follows a space starts one, unless it is inside quotes. A quote
    opens a quoted value only where a value begins (after a colon, a dash,
    a comma or a bracket), so the apostrophe in a plain `don't` opens
    nothing; inside single quotes a doubled one is the character itself."""
    quote, before, at = None, "", 0
    while at < len(line):
        char = line[at]
        if quote:
            if char == quote == "'" and line[at + 1:at + 2] == "'":
                at += 1
            elif char == quote:
                quote = None
        elif char in "\"'" and before in ("", ":", "-", ",", "[", "{"):
            quote = char
        elif char == "#" and (at == 0 or line[at - 1] in " \t"):
            return line[:at].rstrip()
        if not char.isspace():
            before = char
        at += 1
    return line.rstrip()


def rows(text):
    """The lines of a workflow that say something, each as (column, what
    it says without its comment, its line number from zero)."""
    found = []
    for number, line in enumerate(text.splitlines()):
        said = uncommented(line)
        if said.strip():
            found.append((len(said) - len(said.lstrip()), said.strip(), number))
    return found


def under(found, at):
    """The range of the rows under row `at`: those that follow it, up to
    the first that is not indented further."""
    end = at + 1
    while end < len(found) and found[end][0] > found[at][0]:
        end += 1
    return at + 1, end


def pairs(found, start, end):
    """The `key: value` pairs of the block mapping in rows start to end,
    read at the column of its first row and no deeper, values unquoted. A
    mapping written any other way reads as empty."""
    read = {}
    for column, said, _ in found[start:end]:
        key, colon, value = said.partition(":")
        if column == found[start][0] and colon:
            read[key.strip()] = value.strip().strip("\"'")
    return read


def env_under(found, start, end, column):
    """The `env` mapping among the keys at `column` in rows start to end."""
    for at in range(start, end):
        if found[at][0] == column and found[at][1] == "env:":
            return pairs(found, *under(found, at))
    return {}


def workflow_jobs(text):
    """A workflow's jobs, in file order: (name, env, steps, lines). `env` is
    what the job's steps start from, the workflow's own `env` with the
    job's over it; `steps` is the `env` mapping of each of its steps; and
    `lines` is the job as written, for `toolchain_steps`. Everything is
    read from the mappings themselves, so a comment sets nothing."""
    found, lines = rows(text), text.splitlines()
    shared = env_under(found, 0, len(found), 0)
    jobs = []
    for at, (column, said, _) in enumerate(found):
        if column != 0 or said != "jobs:":
            continue
        start, end = under(found, at)
        if start == end:
            continue
        named = [each for each in range(start, end) if found[each][0] == found[start][0]]
        for which, job in enumerate(named):
            inside, done = under(found, job)
            keys = found[inside][0] if inside < done else None
            steps = []
            for key in range(inside, done):
                if found[key][0] != keys or found[key][1] != "steps:":
                    continue
                first, last = under(found, key)
                items = [each for each in range(first, last)
                         if found[each][0] == found[first][0] and (found[each][1] + " ").startswith("- ")]
                for item, opened in enumerate(items):
                    closed = items[item + 1] if item + 1 < len(items) else last
                    dash = found[opened][0]
                    # The step's first key sits on the line of its dash.
                    step = [(dash + 2, found[opened][1][2:].strip(), found[opened][2]), *found[opened + 1:closed]]
                    steps.append(env_under(step, 0, len(step), dash + 2))
            ends = found[named[which + 1]][2] if which + 1 < len(named) else (found[end][2] if end < len(found) else len(lines))
            jobs.append((found[job][1].rstrip(":"), {**shared, **env_under(found, inside, done, keys)}, steps,
                         "\n".join(lines[found[job][2]:ends])))
    return jobs


def unpinned(workflows, channel, wanted):
    """What is wrong with the Rust the workflows install, given the
    version and components rust-toolchain.toml names: one line for each
    step that installs another version, or leaves a component out. Nightly
    is the fuzzing workflow's alone. The job that installs it says so in
    its own `env`, for every Cargo call it makes, and no step of that job
    says otherwise in its own."""
    problems, pinned = [], 0
    for name, text in sorted(workflows.items()):
        if name == "fuzz.yml":
            placed = 0
            for job, env, steps, lines in workflow_jobs(text):
                nightly = sum(version == "nightly" for version, _ in toolchain_steps(lines))
                placed += nightly
                if not nightly:
                    continue
                said = env.get("RUSTUP_TOOLCHAIN")
                if said != "nightly":
                    problems.append(f"{name} installs nightly in job {job}, whose env "
                                    + (f"sets RUSTUP_TOOLCHAIN to {said}" if said is not None else "does not set RUSTUP_TOOLCHAIN")
                                    + ": the toolchain file would decide its plain cargo calls")
                for other in (step["RUSTUP_TOOLCHAIN"] for step in steps if step.get("RUSTUP_TOOLCHAIN", "nightly") != "nightly"):
                    problems.append(f"{name}: a step of job {job} sets RUSTUP_TOOLCHAIN to {other}, over the job's nightly: "
                                    "its cargo calls would build with another Rust")
            if placed != sum(version == "nightly" for version, _ in toolchain_steps(text)):
                problems.append(f"{name} installs nightly in a step that is in no job this check can read: "
                                "whether its job sets RUSTUP_TOOLCHAIN cannot be told")
        for version, components in toolchain_steps(text):
            if version == "nightly" and name == "fuzz.yml":
                continue
            if version != channel:
                problems.append(f"{name} installs Rust {version}; rust-toolchain.toml names {channel}")
                continue
            pinned += 1
            for component in wanted:
                if component not in components:
                    problems.append(f"{name} installs Rust {version} without {component}, which "
                                    "rust-toolchain.toml lists: the first Cargo run would have to fetch it")
    if not pinned:
        problems.append("no workflow installs the pinned Rust")
    return problems


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

    def workflows(self):
        return {place.name: place.read_text() for place in sorted((ROOT / ".github/workflows").glob("*.yml"))}

    def toolchain_file(self):
        pinned = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]
        return pinned["channel"], pinned["components"]

    def test_every_workflow_installs_the_rust_the_toolchain_file_names(self):
        # One Rust for the local gates and for CI: a workflow that asked
        # for `stable` would build with whatever was released last, and a
        # new release could fail main with no change of ours.
        channel, wanted = self.toolchain_file()
        self.assertRegex(channel, r"^[0-9]+\.[0-9]+\.[0-9]+$", "rust-toolchain.toml names one version, not a channel that moves")
        self.assertEqual(unpinned(self.workflows(), channel, wanted), [])

    def test_a_workflow_that_drifts_from_the_toolchain_file_is_refused(self):
        channel, wanted = self.toolchain_file()
        real = self.workflows()
        quoted = f'toolchain: "{channel}"'
        self.assertIn(quoted, real["ci.yml"])

        def with_ci(text):
            return {**real, "ci.yml": text}

        # Back to the channel that moves, in one step of one workflow.
        drifted = unpinned(with_ci(real["ci.yml"].replace(quoted, "toolchain: stable", 1)), channel, wanted)
        self.assertEqual(drifted, [f"ci.yml installs Rust stable; rust-toolchain.toml names {channel}"])
        # Nightly is the fuzzing workflow's, not CI's.
        nightly = unpinned(with_ci(real["ci.yml"].replace(quoted, "toolchain: nightly", 1)), channel, wanted)
        self.assertEqual(nightly, [f"ci.yml installs Rust nightly; rust-toolchain.toml names {channel}"])
        # A component the file lists, left out of a step.
        bare = unpinned(with_ci(real["ci.yml"].replace("components: rustfmt, clippy", "components: rustfmt", 1)), channel, wanted)
        self.assertEqual(len(bare), 1, bare)
        self.assertIn("without clippy", bare[0])
        # The fuzz job must say nightly for all of its Cargo calls.
        silent = {**real, "fuzz.yml": real["fuzz.yml"].replace("RUSTUP_TOOLCHAIN: nightly", "RUSTUP_TOOLCHAIN: other")}
        self.assertTrue(any("RUSTUP_TOOLCHAIN" in problem for problem in unpinned(silent, channel, wanted)))
        # An input is an input: an `env` of the same name does not pin.
        masked = real["ci.yml"].replace(
            f"          {quoted}\n", "          toolchain: stable\n", 1).replace(
            "      - uses: Swatinem/rust-cache", f"        env:\n          toolchain: \"{channel}\"\n      - uses: Swatinem/rust-cache", 1)
        self.assertIn(f"ci.yml installs Rust stable; rust-toolchain.toml names {channel}", unpinned(with_ci(masked), channel, wanted))

    def test_the_fuzz_job_says_nightly_in_its_own_env_and_no_step_says_otherwise(self):
        channel, wanted = self.toolchain_file()
        real = self.workflows()
        fuzz = real["fuzz.yml"]
        said = "      RUSTUP_TOOLCHAIN: nightly\n"
        self.assertEqual(fuzz.count(said), 1)

        def problems(text):
            return unpinned({**real, "fuzz.yml": text}, channel, wanted)

        # The job's value set to stable, with nightly left in a comment
        # above it and in one after it: the words are in the file, and the
        # job no longer says them.
        commented = fuzz.replace(said, "      # RUSTUP_TOOLCHAIN: nightly\n      RUSTUP_TOOLCHAIN: stable # RUSTUP_TOOLCHAIN: nightly\n")
        self.assertEqual(problems(commented), [
            "fuzz.yml installs nightly in job fuzz, whose env sets RUSTUP_TOOLCHAIN to stable: "
            "the toolchain file would decide its plain cargo calls"])
        # Only the comment left.
        self.assertEqual(problems(fuzz.replace(said, "      # RUSTUP_TOOLCHAIN: nightly\n")), [
            "fuzz.yml installs nightly in job fuzz, whose env does not set RUSTUP_TOOLCHAIN: "
            "the toolchain file would decide its plain cargo calls"])
        # The job says nightly, and a step of it that runs Cargo says stable
        # in its own env, which is the one that step's commands see.
        step = "        env:\n          FUZZ_SECONDS: '120'\n"
        self.assertEqual(fuzz.count(step), 1)
        self.assertEqual(problems(fuzz.replace(step, step + "          RUSTUP_TOOLCHAIN: stable\n")), [
            "fuzz.yml: a step of job fuzz sets RUSTUP_TOOLCHAIN to stable, over the job's nightly: "
            "its cargo calls would build with another Rust"])
        # Said in another job's env, it is not said in the one that installs nightly.
        elsewhere = fuzz.replace(said, "      OTHER: value\n") + (
            "  report:\n    runs-on: ubuntu-latest\n    env:\n      RUSTUP_TOOLCHAIN: nightly\n    steps:\n      - run: echo done\n")
        self.assertEqual(len(problems(elsewhere)), 1, problems(elsewhere))
        self.assertIn("in job fuzz, whose env does not set RUSTUP_TOOLCHAIN", problems(elsewhere)[0])
        # What changes nothing is not refused: quotes, a step that says
        # nightly again, and the workflow's own env when the job has none.
        self.assertEqual(problems(fuzz.replace(said, '      RUSTUP_TOOLCHAIN: "nightly"  # the same, quoted\n')), [])
        self.assertEqual(problems(fuzz.replace(step, step + "          RUSTUP_TOOLCHAIN: nightly\n")), [])
        self.assertEqual(problems(fuzz.replace(said, "      OTHER: value\n").replace(
            "permissions:\n", "env:\n  RUSTUP_TOOLCHAIN: nightly\npermissions:\n", 1)), [])
        # And the job's own word is over the workflow's.
        self.assertEqual(len(problems(fuzz.replace(said, "      RUSTUP_TOOLCHAIN: stable\n").replace(
            "permissions:\n", "env:\n  RUSTUP_TOOLCHAIN: nightly\npermissions:\n", 1))), 1)

    def test_a_workflows_jobs_are_read_with_their_env_and_their_steps(self):
        text = ("name: Example # not a job\n"
                "env:\n"
                "  SHARED: 'yes'\n"
                "  OVERRIDDEN: workflow\n"
                "jobs:\n"
                "  first:\n"
                "    runs-on: ubuntu-latest\n"
                "    env:\n"
                "      # NOT_SET: commented\n"
                "      OVERRIDDEN: job # NOT_THIS: either\n"
                "      QUOTED: \"a # that is no comment\"\n"
                "      NESTED:\n"
                "        DEEPER: ignored\n"
                "    steps:\n"
                "      - uses: dtolnay/rust-toolchain@abc\n"
                "        with:\n"
                "          toolchain: nightly\n"
                "          env: not-an-env\n"
                "      - name: Build\n"
                "        run: |\n"
                "          env:\n"
                "            IN_A_SCRIPT: ignored\n"
                "          cargo build\n"
                "        env:\n"
                "          STEP: one\n"
                "      - env:\n"
                "          STEP: two\n"
                "        run: cargo test\n"
                "      -\n"
                "        run: echo a dash alone opens a step too\n"
                "        env:\n"
                "          STEP: three\n"
                "\n"
                "  second:\n"
                "    steps:\n"
                "      - run: echo done\n"
                "other:\n"
                "  env:\n"
                "    NOT_A_JOB: value\n")
        jobs = workflow_jobs(text)
        self.assertEqual([(name, env, steps) for name, env, steps, _ in jobs], [
            ("first", {"SHARED": "yes", "OVERRIDDEN": "job", "QUOTED": "a # that is no comment", "NESTED": ""},
             [{}, {"STEP": "one"}, {"STEP": "two"}, {"STEP": "three"}]),
            ("second", {"SHARED": "yes", "OVERRIDDEN": "workflow"}, [{}]),
        ])
        # Each job's lines are its own: the toolchain it installs is found
        # in it and not in its neighbour.
        self.assertEqual([toolchain_steps(lines) for _, _, _, lines in jobs], [[("nightly", [])], []])
        self.assertEqual(uncommented("  key: 'it''s # kept' # dropped"), "  key: 'it''s # kept'")
        self.assertEqual(uncommented("# all of it"), "")
        self.assertEqual(uncommented("  run: echo a#b"), "  run: echo a#b")
        self.assertEqual(uncommented("  NOTE: don't # a comment after an apostrophe"), "  NOTE: don't")

    def test_a_toolchain_step_is_read_from_its_with_mapping_only(self):
        text = ("    steps:\n"
                "      - uses: dtolnay/rust-toolchain@abc # master\n"
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
                "          toolchain: not-a-toolchain-step\n"
                # An env of the same names is not an input, before or after.
                "      - uses: dtolnay/rust-toolchain@abc\n"
                "        env:\n"
                "          components: clippy\n"
                "        with:\n"
                "          toolchain: stable\n"
                "        env2:\n"
                "          toolchain: \"1.2.3\"\n"
                # A step that opens with its name, and one whose inputs come first.
                "      - name: Install Rust\n"
                "        uses: dtolnay/rust-toolchain@abc\n"
                "        with:\n"
                "          toolchain: 4.5.6\n"
                "          components: rustfmt\n"
                "      - with:\n"
                "          toolchain: 7.8.9\n"
                "        uses: dtolnay/rust-toolchain@abc\n"
                "      # - uses: dtolnay/rust-toolchain@abc, in a comment\n"
                "      - run: echo done\n")
        self.assertEqual(toolchain_steps(text),
                         [("1.2.3", ["llvm-tools-preview", "rustfmt", "clippy"]), ("nightly", []),
                          ("stable", []), ("4.5.6", ["rustfmt"]), ("7.8.9", [])])

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
                    "import sys\nopen(sys.argv[sys.argv.index('--output') + 1], 'w').write('-p\\nfixture\\n')\n"
                    "open(sys.argv[sys.argv.index('--doc-output') + 1], 'w').write('-p\\nfixture\\n')\n")
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
