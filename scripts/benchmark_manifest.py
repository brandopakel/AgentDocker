"""Environment and source identity accompanying every benchmark artifact."""
import hashlib
import json
import os
import platform
import subprocess
import sys

fuzz = len(sys.argv) > 1 and sys.argv[1] == "fuzz"

def run(*args):
    return subprocess.check_output(args, stderr=subprocess.STDOUT).decode().strip()

source = hashlib.sha256()
paths = subprocess.check_output(["git", "ls-files", "-co", "--exclude-standard", "-z"]).split(b"\0")
for path in sorted(set(paths)):
    if not path:
        continue
    source.update(len(path).to_bytes(8, "big") + path)
    if os.path.islink(path):
        data = os.readlink(path)
        source.update(b"link\0" + len(data).to_bytes(8, "big") + data)
    elif os.path.isfile(path):
        mode = os.stat(path).st_mode & 0o111
        source.update(b"file\0" + str(mode).encode() + b"\0")
        source.update(os.stat(path).st_size.to_bytes(8, "big"))
        with open(path, "rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                source.update(chunk)
    else:
        source.update(b"missing\0")
    source.update(b"\0")
print(json.dumps({
    "commit": run("git", "rev-parse", "HEAD"),
    "source_sha256": source.hexdigest(),
    "dirty": bool(run("git", "status", "--porcelain")),
    "rustc": run("rustc", *(["+nightly"] if fuzz else []), "--version"),
    "cargo": run("cargo", *(["+nightly"] if fuzz else []), "--version"),
    **({"cargo_fuzz": run("cargo", "+nightly", "fuzz", "--version"),
        "fuzz_lock_sha256": hashlib.sha256(open("fuzz/Cargo.lock", "rb").read()).hexdigest()} if fuzz else {}),
    "os": platform.platform(),
    "architecture": platform.machine(),
    "processor": platform.processor(),
    "cpu_count": os.cpu_count(),
    "profile": "fuzz" if fuzz else "release",
    "container_engine": None,
    "workloads": {"targets": {"protocol": {"max_len": 65536}, "resource-keys": {"max_len": 4096},
                              "engine-metadata": {"max_len": 65536}, "token-filter": {"max_len": 65536}},
                  "input_timeout_seconds": 10, "rss_limit_mb": 1024,
                  "seconds_per_target": int(sys.argv[2])} if fuzz else {"lease_counts": [1, 100, 1000], "fingerprint_files": 100,
                  "fingerprint_bytes_per_file": 4096, "socket_clients": [1, 10, 100],
        "socket_checkout_kind": "plain-unwatched",
                  "socket_format": 2, "socket_workloads": ["shared", "disjoint"],
                  "socket_outcomes": ["claim_release", "claim_conflict"],
                  "socket_iterations_per_client": 100},
}, indent=2))
