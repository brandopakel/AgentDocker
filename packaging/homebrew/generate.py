#!/usr/bin/env python3
"""Generate an installable formula from the four release archive checksums."""
import argparse
from pathlib import Path
import re


def generate(version: str, checksums: Path) -> str:
    """Reject incomplete release inputs instead of shipping placeholder hashes."""
    if not re.fullmatch(r"v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("expected a release version such as v0.1.0")
    text = Path(__file__).with_name("agentdocker.rb.in").read_text()
    text = text.replace("@VERSION@", version.removeprefix("v"))
    for target in re.findall(r"@SHA_([^@]+)@", text):
        checksum = (checksums / f"agentdocker-{target}.tar.gz.sha256").read_text().split()[0]
        if not re.fullmatch(r"[0-9a-fA-F]{64}", checksum) or checksum == "0" * 64:
            raise ValueError(f"invalid checksum for {target}")
        text = text.replace(f"@SHA_{target}@", checksum.lower())
    if re.search(r"@(VERSION|SHA_)", text):
        raise ValueError("unresolved formula placeholder")
    return text


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("checksums", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(generate(args.version, args.checksums))
