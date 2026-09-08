#!/usr/bin/env python3
"""Check the release workflow's actual downloads and executable payloads."""
import importlib.util
import json
from pathlib import Path
import sys


def check(binary_dir, archive_dir):
    """Share desktop limits and keep CLI-only payloads within 30 MiB."""
    spec = importlib.util.spec_from_file_location("desktop_package", Path(__file__).resolve().parents[1] / "packaging/desktop/package.py")
    package = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(package)
    cli_bytes = sum((binary_dir / name).stat().st_size for name in ("agentdocker", "agentd"))
    if cli_bytes > 30 * 1024 ** 2:
        raise ValueError("CLI payload exceeds its 30 MiB size budget")
    archives = [path for path in archive_dir.iterdir() if path.suffix in {".gz", ".zip"}]
    if not archives:
        raise ValueError("release has no download archives")
    if any(path.stat().st_size > 40 * 1024 ** 2 for path in archives):
        raise ValueError("release download exceeds its 40 MiB size budget")
    result = {"cli_payload_bytes": cli_bytes, "archive_bytes": {p.name: p.stat().st_size for p in archives}}
    app = binary_dir / "AgentDocker.app"
    if app.exists():
        result["desktop"] = package.measure_sizes(app, archives)
    return result


if __name__ == "__main__":
    print(json.dumps(check(Path(sys.argv[1]), Path(sys.argv[2])), indent=2))
