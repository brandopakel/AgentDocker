#!/usr/bin/env python3
"""Keep the documentation an accurate record of the repository.

Checks, each of which fails the gate when it does not hold:

1. Every Markdown file under docs/ is linked from docs/README.md, the index.
2. Every relative link in every Markdown file resolves to a file that exists.
3. Every `docs/<Name>.md` named anywhere else in the tree (a source comment,
   an error message, the front page's HTML anchors) exists, unless the text
   says it is in git history.
4. With --base <ref>: a change under crates/, scripts/, packaging/,
   install.sh, Makefile or .github/ is accompanied by a change under docs/,
   README.md or CLAUDE.md, or a commit in the range says why not with a
   line starting `Docs:` (for example `Docs: unchanged, a refactor with no
   behaviour change`). The check asks that documentation was considered on
   every change; it does not demand an edit where none is due.

A trial on real binaries is recorded as one line in docs/verification/INDEX.md;
that file is a document like any other and needs nothing generated.
"""
import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DOCS = ROOT / "docs"
INDEX = DOCS / "README.md"
CODE_PATHS = ("crates/", "scripts/", "packaging/", ".github/")
CODE_FILES = ("install.sh", "Makefile", "Cargo.toml", "Cargo.lock")
DOC_PATHS = ("docs/",)
DOC_FILES = ("README.md", "CLAUDE.md")
LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")
# The documents are named in capitals; a lower-case `docs/x.md` in a test is an example, not a pointer.
POINTER = re.compile(r"docs/((?:verification/)?[A-Z][A-Z0-9-]*\.md)")


def markdown_files():
    return tracked_files("*.md")


def tracked_files(*patterns):
    tracked = subprocess.check_output(["git", "ls-files", "-z", "--", *patterns], cwd=ROOT).split(b"\0")
    return sorted(ROOT / name.decode() for name in tracked if name)


def index_completeness():
    text = INDEX.read_text()
    linked = set(re.findall(r"\]\(([^)#]+\.md)", text))
    problems = []
    for path in sorted(DOCS.glob("*.md")) + sorted((DOCS / "verification").glob("*.md")):
        relative = path.relative_to(DOCS).as_posix()
        if path.name != "README.md" and relative not in linked:
            problems.append(f"docs/{relative} is not linked from docs/README.md")
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


def pointers_resolve():
    """A document named outside the docs tree, by path, must exist: the
    Markdown link check does not see a source comment, an error message or
    the front page's HTML anchors."""
    problems = []
    for path in tracked_files("*.md", "*.py", "*.rs", "*.toml", "*.sh", "*.yml", "*.yaml", "Makefile"):
        if path.is_relative_to(DOCS):
            continue
        text = path.read_text(errors="replace")
        for match in POINTER.finditer(text):
            name = match.group(1)
            if "in git history" in text[match.end():match.end() + 40]:
                continue
            if not (ROOT / "docs" / name).exists():
                problems.append(f"{path.relative_to(ROOT)} names docs/{name}, which does not exist")
    return problems


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
        "GUIDE.md or README.md for usage, a line in docs/verification/INDEX.md for a trial) or add a `Docs: ...` line to a commit message explaining why none is due"
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", help="git ref to diff against for the docs-considered check")
    args = parser.parse_args()
    problems = index_completeness() + links_resolve() + pointers_resolve()
    if args.base:
        problems += docs_considered(args.base)
    for problem in problems:
        print(f"docs_check: {problem}", file=sys.stderr)
    if problems:
        sys.exit(1)
    print(f"docs_check: ok ({len(markdown_files())} markdown files)")


if __name__ == "__main__":
    os.chdir(ROOT)
    main()
