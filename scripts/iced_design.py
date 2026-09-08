#!/usr/bin/env python3
"""Capture the optional Iced preview and retain a local macOS review app.

Every window contains fictional in-memory data. This driver starts only the
supplied preview binary; it does not connect to or configure the daemon.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SCENES = {
    "projects-light": [],
    "session-details": ["--details"],
    "inbox": ["--page", "inbox"],
    "connections-dark": ["--page", "connections", "--dark"],
    "narrow-details": ["--width", "720", "--details"],
    "narrow-inbox": ["--width", "720", "--page", "inbox"],
    "narrow-connections": ["--width", "720", "--page", "connections"],
    "narrow-settings": ["--width", "720", "--page", "settings"],
    "empty": ["--empty"],
    "disconnected": ["--offline", "--page", "inbox"],
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def capture(binary, output):
    """Save actual rendered windows and provenance without replacing old evidence."""
    binary = binary.resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError("preview binary must be executable")
    output = output.absolute()
    output.mkdir(mode=0o700)
    report = {
        "scope": "Iced design preview with fictional data; no operational GUI parity claim",
        "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "source_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT)),
        "platform": platform.platform(),
        "binary_bytes": binary.stat().st_size,
        "input_binary_sha256": digest(binary),
        "source_sha256": {},
        "captures": [],
        "result": "failed",
    }
    sources = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", Path(__file__), ROOT / "crates/ui/src/icon.png"]
    sources += sorted((ROOT / "crates/ui-iced").rglob("*.rs"))
    sources += [ROOT / "crates/ui-iced/Cargo.toml"]
    report["source_sha256"] = {str(p.relative_to(ROOT)): digest(p) for p in sources}
    try:
        captures = output / "captures"
        captures.mkdir()
        for name, arguments in SCENES.items():
            png = captures / (name + ".png")
            with (captures / (name + ".log")).open("w") as log:
                subprocess.run([str(binary), *arguments, "--screenshot", str(png)], cwd=ROOT,
                               stdout=log, stderr=subprocess.STDOUT, timeout=25, check=True)
            if not png.is_file() or png.stat().st_size < 1024:
                raise RuntimeError("native window capture is missing")
            report["captures"].append({"scene": name, "file": str(png.relative_to(output)), "sha256": digest(png)})
        if platform.system() == "Darwin":
            app = output / "AgentDocker Design.app"
            macos = app / "Contents/MacOS"
            resources = app / "Contents/Resources"
            macos.mkdir(parents=True)
            resources.mkdir()
            executable = macos / "agentdocker-design"
            shutil.copy2(binary, executable)
            executable.chmod(0o755)
            info = {"CFBundleName": "AgentDocker Design", "CFBundleDisplayName": "AgentDocker Design",
                    "CFBundleIdentifier": "dev.agentdocker.design-preview", "CFBundleExecutable": "agentdocker-design",
                    "CFBundleIconFile": "AgentDocker", "CFBundlePackageType": "APPL",
                    "CFBundleVersion": "1", "CFBundleShortVersionString": "0.1.0",
                    "NSHighResolutionCapable": True, "LSMinimumSystemVersion": "11.0"}
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
            with tempfile.TemporaryDirectory(prefix="ad-design-icons-") as scratch:
                subprocess.run([sys.executable, str(ROOT / "scripts/icon.py"), scratch], check=True, stdout=subprocess.DEVNULL)
                subprocess.run(["iconutil", "-c", "icns", str(Path(scratch) / "AgentDocker.iconset"),
                                "-o", str(resources / "AgentDocker.icns")], check=True)
            subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(app)], check=True)
            subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
            archive = output / "agentdocker-iced-design-macos-arm64.zip"
            if platform.machine() != "arm64":
                archive = output / ("agentdocker-iced-design-macos-" + platform.machine() + ".zip")
            subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive)], check=True)
            report["app"] = {"file": app.name, "signing": "local-preview", "notarized": False,
                             "payload_bytes": sum(p.stat().st_size for p in app.rglob("*") if p.is_file()),
                             "binary_sha256": digest(executable), "archive": archive.name,
                             "archive_bytes": archive.stat().st_size, "archive_sha256": digest(archive)}
        report["result"] = "passed"
    finally:
        (output / "manifest.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(capture(args.binary, args.output), indent=2))
