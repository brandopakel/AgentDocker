#!/usr/bin/env python3
"""Build the three desktop executables and bind them to exact source inputs."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]
BINARIES = ("agentdocker", "agentd", "agentdocker-ui")


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT)


def inputs():
    names = sorted(set(git("ls-files", "-c", "-o", "--exclude-standard", "-z").split(b"\0")) - {b""})
    digest = hashlib.sha256()
    for name in names:
        path = ROOT / name.decode()
        digest.update(name + b"\0")
        if path.is_symlink():
            digest.update(str(path.readlink()).encode())
        elif path.is_file():
            digest.update(path.read_bytes())
        else:
            digest.update(b"<absent>")
        digest.update(b"\0")
    return {
        "source_commit": git("rev-parse", "HEAD").decode().strip(),
        "source_tree": git("rev-parse", "HEAD^{tree}").decode().strip(),
        "source_input_sha256": digest.hexdigest(),
        "source_dirty": bool(git("status", "--porcelain", "--untracked-files=all")),
    }


def build(target):
    rustc = subprocess.check_output(["rustc", "-Vv"], text=True)
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    target = target or host
    before = inputs()
    argv = ["cargo", "build", "--locked", "--release", "-p", "agentdocker", "-p", "agentdocker-ui", "--bins", "--message-format=json-render-diagnostics"]
    if target != host:
        argv += ["--target", target]
    # Cargo reports the actual artifact paths, including target-dir overrides.
    output = subprocess.check_output(argv, cwd=ROOT, text=True)
    executables = {}
    for line in output.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            name = message["target"]["name"]
            if name in BINARIES:
                executables[name] = Path(message["executable"])
    if set(executables) != set(BINARIES) or len({path.parent for path in executables.values()}) != 1:
        raise RuntimeError("Cargo did not report all three desktop executables in one directory")
    if inputs() != before:
        raise RuntimeError("source changed during the native build; no provenance manifest written")
    directory = executables["agentdocker"].parent
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    schema = re.search(r"const SCHEMA_VERSION: (?:i64|u32) = (\d+);", (ROOT / "crates/agentd/src/store.rs").read_text())
    if schema is None:
        raise RuntimeError("cannot identify the built daemon's state schema")
    result = {
        "format": 1, **before, "target": target, "version": version, "rustc": rustc,
        "state_schema": int(schema[1]), "binary_directory": str(directory),
        "binary_sha256": {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in BINARIES},
    }
    (directory / "native-build.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target")
    print(json.dumps(build(parser.parse_args().target), indent=2))
