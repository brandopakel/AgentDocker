#!/usr/bin/env python3
"""Record a local gate run of scripts/verify.sh (check, quick or test) with
the daemon, as its runner reports it.

verify.sh calls this three times, all within the run's own campaign folder:

  start           the run's key, its start time, branch and start boundary
  nextest-config  a copy of the checked-in nextest config whose store is the
                  campaign folder, so the run's JUnit report is its own
  finish          the end boundary, the steps as logged, the counts the run's
                  own artifacts establish, and one bounded send

Nothing here changes a gate's result: every failure is one line on stderr,
and `finish` never takes longer than its deadline.
"""
import argparse
import datetime
import json
import os
import re
import signal
import subprocess
import sys
import tomllib
import uuid
import xml.etree.ElementTree as ElementTree
from pathlib import Path

# The whole of `finish`, the send included.
DEADLINE = 10
# The CLI's exits that say nothing was kept: a usage error before anything
# was sent, or a refusal the daemon makes before the write (invalid, not
# found, conflict, forbidden). Any other exit may follow a record the
# daemon kept: 1 (no answer, a broken connection), and 6 too, since an
# unavailable store or a transfer latched by another writer after this
# write committed replaces its answer.
REFUSED = (2, 3, 4, 5)
FULL = re.compile(r"^[0-9a-f]{40}([0-9a-f]{24})?$")


def iso(seconds: float) -> str:
    moment = datetime.datetime.fromtimestamp(int(seconds), datetime.timezone.utc)
    return moment.strftime("%Y-%m-%dT%H:%M:%SZ")


def git(*args: str):
    """Git's answer, stripped, or None when it could not be read."""
    try:
        done = subprocess.run(["git", *args], capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if done.returncode != 0:
        return None
    return done.stdout.strip()


def boundary() -> dict:
    """The head and whether the tree is clean; unknown when Git fails."""
    head = git("rev-parse", "--verify", "HEAD^{commit}")
    status = git("status", "--porcelain")
    if head is None or not FULL.match(head):
        return {"status": "unknown"}
    if status is None:
        return {"head": head, "status": "unknown"}
    return {"head": head, "status": "clean" if status == "" else "dirty"}


def start(folder: Path) -> None:
    branch = git("symbolic-ref", "--short", "-q", "HEAD")
    state = {
        "run_key": uuid.uuid4().hex,
        "started_at": iso(datetime.datetime.now().timestamp()),
        "checkout": os.getcwd(),
        "branch": branch or None,
        "start": boundary(),
    }
    (folder / "run-start.json").write_text(json.dumps(state))


def nextest_config(folder: Path, source: Path) -> None:
    """The checked-in policy, unchanged, with this run's own store: its
    JUnit report is then written where nothing else writes."""
    text = source.read_text()
    policy = tomllib.loads(text)
    if "store" in policy:
        raise SystemExit("the nextest config already names a store")
    store = folder / "nextest"
    merged = (
        text.rstrip("\n")
        + "\n\n# Written for one verify.sh run: its reports go to its own folder.\n[store]\ndir = "
        + json.dumps(str(store))
        + "\n"
    )
    read = tomllib.loads(merged)
    if read.pop("store", None) != {"dir": str(store)} or read != policy:
        raise SystemExit("the relocated nextest config does not read back as the original")
    (folder / "nextest.toml").write_text(merged)


def classify(argv: list):
    """A step's name from what it ran, and the program of one not in the
    table."""
    if argv[:1] == ["record_stderr"]:
        argv = argv[2:]
    head = argv[:2]
    if head == ["python3", "scripts/docs_check.py"]:
        return "docs", None
    if head == ["python3", "scripts/touched_crates.py"]:
        return "selection", None
    if argv[:3] == ["python3", "-m", "unittest"]:
        return "installer", None
    if head == ["cargo", "fmt"]:
        return "fmt", None
    if head == ["cargo", "metadata"] and "fuzz/Cargo.toml" in argv:
        return "fuzz_lock", None
    if head == ["cargo", "check"] and "fuzz/Cargo.toml" in argv:
        return "fuzz_build", None
    if head == ["cargo", "clippy"]:
        return "clippy", None
    if head == ["cargo", "nextest"]:
        return "nextest", None
    if head == ["cargo", "test"] and "--doc" in argv:
        return "doctests", None
    return "other", (argv[0] if argv else "?")[:200]


def logged_steps(folder: Path) -> list:
    path = folder / "steps.tsv"
    steps = {}
    if not path.exists():
        return []
    for line in path.read_text().splitlines():
        parts = line.split("\t")
        if parts[0] == "start" and len(parts) >= 3:
            order = int(parts[1])
            name, program = classify(parts[3:])
            steps[order] = {"order": order, "name": name, "started_at": iso(int(parts[2]))}
            if program is not None:
                steps[order]["program"] = program
        elif parts[0] == "end" and len(parts) == 4 and int(parts[1]) in steps:
            steps[int(parts[1])].update(finished_at=iso(int(parts[2])), status=int(parts[3]))
    return [steps[order] for order in sorted(steps)]


def junit_counts(path: Path):
    """What the run's own JUnit report establishes: the cases it lists and
    which failed. Skipped cases are not reported by the policy, so no
    skipped count is claimed. None when the report is absent or partial."""
    try:
        root = ElementTree.parse(path).getroot()
    except (OSError, ElementTree.ParseError):
        return None
    if root.tag not in ("testsuites", "testsuite"):
        return None
    run = passed = failed = 0
    for case in root.iter("testcase"):
        run += 1
        if case.find("failure") is not None or case.find("error") is not None:
            failed += 1
        elif case.find("skipped") is None:
            passed += 1
    return {"run": run, "passed": passed, "failed": failed}


def unittest_counts(path: Path):
    """What unittest's own summary says: how many ran, none failed on
    `OK`, and how many were skipped when it says so. A failed summary's
    counts (failures and errors, subtests among them) are not test counts,
    so none is claimed for it."""
    try:
        text = path.read_text(errors="replace")
    except OSError:
        return None
    ran = re.findall(r"^Ran (\d+) tests? in ", text, re.M)
    ends = re.findall(r"^(OK|FAILED)(?: \((.*)\))?$", text, re.M)
    if not ran or not ends:
        return None
    word, detail = ends[-1]
    fields = {}
    for part in (detail or "").split(","):
        key, _, value = part.strip().partition("=")
        if value.isdigit():
            fields[key] = int(value)
    counts = {"run": int(ran[-1])}
    if word == "OK":
        counts["failed"] = 0
    if "skipped" in fields:
        counts["skipped"] = fields["skipped"]
    return counts


def scope(folder: Path):
    """A quick run's base and selections, when all three were written."""
    try:
        base = (folder / "base").read_text().strip()
        packages = (folder / "packages").read_text().split()
        docs = (folder / "doc-packages").read_text().split()
    except OSError:
        return None
    if not FULL.match(base):
        return None
    found = {"base": base, "packages": packages, "doc_packages": docs}
    # Whether the change reached the fuzz targets, only when the selection
    # said: a missing word leaves the run incomplete, never assumed.
    try:
        word = (folder / "fuzz").read_text().strip()
    except OSError:
        word = None
    if word in ("true", "false"):
        found["fuzz"] = word == "true"
    return found


def report(folder: Path, mode: str, exit_status: int) -> dict:
    state = json.loads((folder / "run-start.json").read_text())
    steps = logged_steps(folder)
    owned = (folder / "nextest.toml").exists()
    for step in steps:
        counts = None
        if step["name"] == "nextest" and owned:
            counts = junit_counts(folder / "nextest" / "ci" / "junit.xml")
        elif step["name"] == "installer":
            counts = unittest_counts(folder / "installer.err")
        if counts is not None and step.get("status") == 0 and counts.get("failed", 0) > 0:
            counts = None
        if counts is not None:
            step["counts"] = counts
    run = {
        # The steps this verify.sh's modes require: 2 since the fuzz
        # targets are compiled.
        "gate_version": 2,
        "run_key": state["run_key"],
        "checkout": state["checkout"],
        "mode": mode,
        "start": state["start"],
        "end": boundary(),
        "started_at": state["started_at"],
        "finished_at": iso(datetime.datetime.now().timestamp()),
        "exit": exit_status,
        "completed": (folder / "completed").exists(),
        "steps": steps,
    }
    if state.get("branch"):
        run["branch"] = state["branch"]
    if mode == "quick":
        found = scope(folder)
        if found is not None:
            run["scope"] = found
    return run


def finish(args) -> int:
    folder = Path(args.dir)
    run = report(folder, args.mode, args.exit)
    path = folder / "run.json"
    path.write_text(json.dumps(run))
    command = ["agentdocker", "gate-run", "record", "--file", str(path)]
    if args.by:
        command += ["--as", args.by]
    if args.runner:
        command += ["--runner", args.runner]
    if args.infer_caller:
        command += ["--infer-caller"]
    if args.socket:
        command += ["--socket", args.socket]
    environment = dict(os.environ, AGENTDOCKER_NO_AUTOSTART="1")
    try:
        done = subprocess.run(
            command, env=environment, capture_output=True, text=True, timeout=DEADLINE - 2
        )
    except subprocess.TimeoutExpired:
        print(f"verify.sh: recording this run was not confirmed (no answer within {DEADLINE - 2}s)", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"verify.sh: this run was not recorded ({error})", file=sys.stderr)
        return 1
    said = (done.stderr.strip().splitlines() or ["no reason given"])[-1]
    if done.returncode == 0:
        print(f"verify.sh: recorded this run as gate run {done.stdout.strip()}", file=sys.stderr)
        return 0
    if done.returncode in REFUSED:
        print(f"verify.sh: this run was not recorded ({said})", file=sys.stderr)
        return 1
    # 1 (no answer, a broken connection, an unconfirmed change) or anything
    # else: the daemon may have kept it before the answer was lost.
    print(f"verify.sh: recording this run was not confirmed ({said})", file=sys.stderr)
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    begin = commands.add_parser("start")
    begin.add_argument("--dir", required=True)
    policy = commands.add_parser("nextest-config")
    policy.add_argument("--dir", required=True)
    policy.add_argument("--source", default=".config/nextest.toml")
    end = commands.add_parser("finish")
    end.add_argument("--dir", required=True)
    end.add_argument("--mode", required=True, choices=["check", "quick", "test"])
    end.add_argument("--exit", type=int, required=True)
    end.add_argument("--by")
    end.add_argument("--runner")
    end.add_argument("--infer-caller", action="store_true")
    end.add_argument("--socket")
    args = parser.parse_args()
    # The whole of it, bounded: a hang anywhere is a run not recorded.
    signal.signal(signal.SIGALRM, lambda *_: (_ for _ in ()).throw(TimeoutError()))
    signal.alarm(DEADLINE)
    try:
        if args.command == "start":
            start(Path(args.dir))
            return 0
        if args.command == "nextest-config":
            nextest_config(Path(args.dir), Path(args.source))
            return 0
        return finish(args)
    except TimeoutError:
        print(f"verify.sh: recording this run was not confirmed (it took more than {DEADLINE}s)", file=sys.stderr)
        return 1
    except SystemExit as stop:
        if isinstance(stop.code, str):
            print(f"verify.sh: {stop.code}; this run's counts will not be recorded", file=sys.stderr)
            return 1
        raise
    except Exception as error:  # noqa: BLE001 - recording never stops a gate
        print(f"verify.sh: this run was not recorded ({error})", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
