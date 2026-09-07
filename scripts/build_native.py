#!/usr/bin/env python3
"""Build the three desktop executables and bind them to exact source inputs."""
import argparse
import hashlib
import json
from pathlib import Path
import shlex
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


def daemon_metadata(executable, target, version, runner):
    """Read the compiled contract without starting the daemon or opening state."""
    try:
        output = subprocess.check_output([*runner, str(executable), "--build-info"], text=True, timeout=30)
    except OSError as error:
        raise RuntimeError("cannot execute the built daemon; cross builds need a compatible --schema-runner") from error
    metadata = json.loads(output)
    schema = metadata.get("state_schema")
    os_name = "macos" if "apple-darwin" in target else "linux" if "linux" in target else None
    arch = target.split("-", 1)[0]
    if (metadata.get("format") != 1 or metadata.get("version") != version
            or os_name is None or metadata.get("os") != os_name or metadata.get("arch") != arch):
        raise ValueError("built daemon metadata does not match the requested version/platform")
    if type(schema) is not int or not 1 <= schema <= 0xFFFF_FFFF:
        raise ValueError("built daemon reports an invalid state schema")
    return metadata


def build(target, schema_runner=()):
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
    directory = executables["agentdocker"].parent
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    metadata = daemon_metadata(executables["agentd"], target, version, schema_runner)
    if inputs() != before:
        raise RuntimeError("source changed during the native build; no provenance manifest written")
    result = {
        "format": 1, **before, "target": target, "version": version, "rustc": rustc,
        "state_schema": metadata["state_schema"], "installation_lock": metadata.get("installation_lock", 0), "binary_directory": str(directory),
        "binary_sha256": {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in BINARIES},
    }
    (directory / "native-build.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target")
    parser.add_argument("--schema-runner", default="", help="Command prefix for executing the target daemon's --build-info, e.g. 'qemu-aarch64 -L /target/sysroot'; no shell is used")
    args = parser.parse_args()
    print(json.dumps(build(args.target, shlex.split(args.schema_runner)), indent=2))
