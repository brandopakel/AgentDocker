#!/usr/bin/env python3
"""Keep the documentation an accurate record of the repository.

Checks, each of which fails the gate when it does not hold:

1. Every Markdown file under docs/ is linked from docs/README.md, the index.
2. Every relative link in every Markdown file resolves to a file that exists.
3. docs/verification/README.md lists every verification record exactly as
   `--write-index` would write it, so a new record is indexed with a line
   from its own status.
4. With --base <ref>: a change under crates/, scripts/, packaging/,
   install.sh, Makefile or .github/ is accompanied by a change under docs/,
   README.md or CLAUDE.md, or a commit in the range says why not with a
   line starting `Docs:` (for example `Docs: unchanged, a refactor with no
   behaviour change`). The check asks that documentation was considered on
   every change; it does not demand an edit where none is due.
"""
import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DOCS = ROOT / "docs"
INDEX = DOCS / "README.md"
RECORDS = DOCS / "verification"
RECORD_INDEX = RECORDS / "README.md"
CODE_PATHS = ("crates/", "scripts/", "packaging/", ".github/")
CODE_FILES = ("install.sh", "Makefile", "Cargo.toml", "Cargo.lock")
DOC_PATHS = ("docs/",)
DOC_FILES = ("README.md", "CLAUDE.md", "AGENTS.md")
LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")


def markdown_files():
    tracked = subprocess.check_output(["git", "ls-files", "-z", "--", "*.md"], cwd=ROOT).split(b"\0")
    return sorted(ROOT / name.decode() for name in tracked if name)


def index_completeness():
    text = INDEX.read_text()
    linked = set(re.findall(r"\]\(([^)#]+\.md)", text))
    problems = []
    for path in sorted(DOCS.glob("*.md")):
        if path.name != "README.md" and path.name not in linked:
            problems.append(f"docs/{path.name} is not linked from docs/README.md")
    return problems


def links_resolve():
    problems = []
    for path in markdown_files():
        for target in LINK.findall(path.read_text(errors="replace")):
            if "://" in target or target.startswith(("#", "mailto:")):
                continue
            target = target.split("#", 1)[0]
            if not target:
                continue
            resolved = (path.parent / target).resolve()
            if not resolved.exists():
                problems.append(f"{path.relative_to(ROOT)} links to {target}, which does not exist")
    return problems


def record_summary(path):
    """One line from the record itself: its status where it has one, else a
    nested result or scope, else its first listed change, else what and
    when it recorded."""
    try:
        record = json.loads(path.read_text())
    except (OSError, ValueError):
        return "(unreadable record)"
    if not isinstance(record, dict):
        return "(record is not an object)"

    def text_of(value):
        if isinstance(value, str) and value.strip():
            return value.strip()
        if isinstance(value, dict):
            for inner in ("status", "result", "summary", "scope", "text"):
                found = text_of(value.get(inner))
                if found:
                    return found
        if isinstance(value, list) and value and isinstance(value[0], str):
            return value[0].strip()
        return None

    text = None
    for key in ("status", "result", "summary", "conclusion", "scope"):
        text = text_of(record.get(key))
        if text:
            break
    if not text:
        for key, value in record.items():
            if isinstance(value, dict):
                nested = text_of(value.get("result")) or text_of(value.get("status"))
                if nested:
                    text = f"{key}: {nested}"
                    break
    if not text:
        text = text_of(record.get("changes"))
    if not text:
        when = record.get("date") or record.get("recorded_on") or record.get("recorded_at") or "undated"
        what = record.get("pr") or record.get("workflow") or record.get("head") or "no summary field"
        text = f"recorded {when}; {what}"
    first = re.split(r"(?<=\.)\s", text, maxsplit=1)[0]
    first = " ".join(first.split())
    if len(first) > 220:
        first = first[:217].rstrip() + "..."
    return first.replace("|", "\\|")


def render_record_index():
    lines = [
        "# Verification records",
        "",
        "One line per record, taken from the record's own status; regenerate with",
        "`python3 scripts/docs_check.py --write-index` after adding one. The check",
        "in the gate fails when this file and the records disagree. A record keeps",
        "its original source, date and outcome; a later merge does not rewrite it.",
        "",
        "| Record | Says |",
        "| --- | --- |",
    ]
    for path in sorted(RECORDS.glob("*.json")):
        lines.append(f"| [{path.name}]({path.name}) | {record_summary(path)} |")
    return "\n".join(lines) + "\n"


def record_index_current():
    expected = render_record_index()
    actual = RECORD_INDEX.read_text() if RECORD_INDEX.exists() else ""
    if expected != actual:
        return ["docs/verification/README.md is not current; run `python3 scripts/docs_check.py --write-index`"]
    return []


def changed_files(base):
    try:
        merge_base = subprocess.check_output(["git", "merge-base", base, "HEAD"], cwd=ROOT, text=True).strip()
    except subprocess.CalledProcessError:
        return None, None
    names = subprocess.check_output(["git", "diff", "--name-only", f"{merge_base}...HEAD"], cwd=ROOT, text=True).split()
    # Uncommitted work counts as part of what is being checked.
    names += subprocess.check_output(["git", "diff", "--name-only", "HEAD"], cwd=ROOT, text=True).split()
    names += subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard"], cwd=ROOT, text=True).split()
    messages = subprocess.check_output(["git", "log", "--format=%B", f"{merge_base}..HEAD"], cwd=ROOT, text=True)
    return sorted(set(names)), messages


def docs_considered(base):
    names, messages = changed_files(base)
    if names is None:
        return [f"cannot find a merge base with {base}; fetch it or pass --base"]
    code = [n for n in names if n.startswith(CODE_PATHS) or n in CODE_FILES]
    docs = [n for n in names if n.startswith(DOC_PATHS) or n in DOC_FILES]
    if not code or docs:
        return []
    if re.search(r"^Docs:\s*\S", messages, re.MULTILINE):
        return []
    return [
        "code changed with no documentation change and no commit saying why: "
        + ", ".join(code[:8])
        + (" ..." if len(code) > 8 else "")
        + "; update the affected doc (ARCHITECTURE.md for protocol or semantics, REMAINING-WORK.md and the docs/README.md row for status, "
        "GUIDE.md or README.md for usage, a verification record for a trial) or add a `Docs: ...` line to a commit message explaining why none is due"
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", help="git ref to diff against for the docs-considered check")
    parser.add_argument("--write-index", action="store_true", help="regenerate docs/verification/README.md")
    args = parser.parse_args()
    if args.write_index:
        RECORD_INDEX.write_text(render_record_index())
        print(f"wrote {RECORD_INDEX.relative_to(ROOT)}")
    problems = index_completeness() + links_resolve() + record_index_current()
    if args.base:
        problems += docs_considered(args.base)
    for problem in problems:
        print(f"docs_check: {problem}", file=sys.stderr)
    if problems:
        sys.exit(1)
    print(f"docs_check: ok ({len(markdown_files())} markdown files, {len(list(RECORDS.glob('*.json')))} verification records)")


if __name__ == "__main__":
    os.chdir(ROOT)
    main()
