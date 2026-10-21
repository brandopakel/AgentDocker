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
5. With --base <ref>: no line added to a document carries a specific (a
   date, a pull request, issue or CI run number, a commit, a home directory,
   a real agent's name, or a term from the file AGENTDOCKER_SPECIFICS_DENYLIST
   names). The documents describe the code, not a moment, a machine or a
   person. `--specifics` reports every specific already in the tree and
   exits 0.

Evidence from a trial on real binaries goes in its pull request, not in the
repository.
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
# Instruction files that are not Markdown but are read the same way.
INSTRUCTION_FILES = (".coderabbit.yaml",)
MONTH = r"(?:January|February|March|April|May|June|July|August|September|October|November|December)"
# What a document must not carry, each named for the report. A home
# directory is allowed for the placeholder users (`/Users/you`, `/Users/me`,
# `<user>`, `$USER`) and for CI's `runner`. A date in a code span is a
# literal value, such as a protocol revision, not a moment.
SPECIFICS = (
    ("a date", re.compile(r"\b20\d\d-[01]\d-[0-3]\d\b")),
    ("a date", re.compile(rf"\b{MONTH} [0-3]?\d\b")),
    ("a pull request or issue number", re.compile(r"(?<![\w#&/])#\d{2,5}\b|\b(?:PR|pull request|issue) ?#?\d+\b|/(?:pull|issues)/\d+")),
    ("a commit", re.compile(r"(?<![\w-])(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,64}(?![\w-])")),
    ("a CI run", re.compile(r"/actions/runs/\d+|\brun \d{8,}\b")),
    ("a home directory", re.compile(r"(?:/Users/|/home/|\b[A-Za-z]:\\{1,2}Users\\{1,2})(?!(?:you|me|runner|USER)\b|[<$%])[\w.-]+")),
    ("a real agent's name", re.compile(r"\b(?:claude-code|codex|opencode)-\d{3,}\b")),
)


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
        + "; update the affected doc (ARCHITECTURE.md for protocol or semantics, REMAINING-WORK.md for status, "
        "GUIDE.md or README.md for usage) or add a `Docs: ...` line to a commit message explaining why none is due"
    ]


def denied_terms():
    """The private names one setup keeps out of the tree, one per line in
    the file AGENTDOCKER_SPECIFICS_DENYLIST names; none when it is unset."""
    path = os.environ.get("AGENTDOCKER_SPECIFICS_DENYLIST")
    if not path:
        return ()
    lines = Path(path).expanduser().read_text().splitlines()
    return tuple(line.strip() for line in lines if line.strip() and not line.lstrip().startswith("#"))


def specifics(line, denied=()):
    """(what, text) for every specific in one line of a document."""
    code = [m.span() for m in re.finditer(r"`[^`]*`", line)]
    found = [
        (what, m.group(0))
        for what, pattern in SPECIFICS
        for m in pattern.finditer(line)
        if what != "a date" or not any(start < m.start() < end for start, end in code)
    ]
    for term in denied:
        pattern = re.escape(term)
        if term[:1].isalnum():
            pattern = r"\b" + pattern
        if term[-1:].isalnum():
            pattern += r"\b"
        found += [("a private name", m.group(0)) for m in re.finditer(pattern, line, re.IGNORECASE)]
    return found


def is_document(name):
    return name.endswith(".md") or name in INSTRUCTION_FILES


def added_document_lines(merge_base):
    """(path, line number, text) for every line added to a document since
    `merge_base`, committed or not, untracked documents whole."""
    added = []
    for diff in (["git", "diff", "--unified=0", "--no-color", f"{merge_base}...HEAD"],
                 ["git", "diff", "--unified=0", "--no-color", "HEAD"]):
        path, number = None, 0
        for line in subprocess.check_output(diff, cwd=ROOT, text=True, errors="replace").splitlines():
            if line.startswith("+++ "):
                name = line[4:].removeprefix("b/")
                path = name if name != "/dev/null" and is_document(name) else None
            elif line.startswith("@@"):
                number = int(re.match(r"@@ -\S+ \+(\d+)", line).group(1))
            elif line.startswith("+") and path:
                added.append((path, number, line[1:]))
                number += 1
    untracked = subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard"], cwd=ROOT, text=True).split()
    for name in filter(is_document, untracked):
        for number, line in enumerate((ROOT / name).read_text(errors="replace").splitlines(), 1):
            added.append((name, number, line))
    return added


def no_new_specifics(base):
    try:
        merge_base = subprocess.check_output(["git", "merge-base", base, "HEAD"], cwd=ROOT, text=True).strip()
    except subprocess.CalledProcessError:
        return [f"cannot find a merge base with {base}; fetch it or pass --base"]
    denied = denied_terms()
    return [
        f"{path}:{number} adds {what} ({text}); the documents describe the code, not a moment, a machine or a "
        "person: use a placeholder, or leave it to the pull request"
        for path, number, line in added_document_lines(merge_base)
        for what, text in specifics(line, denied)
    ]


def report_specifics():
    """Every specific already in the tracked documents, for the cleanup; not a gate."""
    denied = denied_terms()
    counts = {}
    names = [path for path in tracked_files("*.md", *INSTRUCTION_FILES)]
    for path in names:
        for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            for what, text in specifics(line, denied):
                print(f"{path.relative_to(ROOT)}:{number}: {what}: {text}")
                counts[path.relative_to(ROOT)] = counts.get(path.relative_to(ROOT), 0) + 1
    for name, count in sorted(counts.items(), key=lambda item: -item[1]):
        print(f"docs_check: {count} specifics in {name}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", help="git ref to diff against for the docs-considered and specifics checks")
    parser.add_argument("--specifics", action="store_true", help="report every specific in the tracked documents and exit 0")
    args = parser.parse_args()
    if args.specifics:
        report_specifics()
        return
    problems = index_completeness() + links_resolve() + pointers_resolve()
    if args.base:
        problems += docs_considered(args.base)
        problems += no_new_specifics(args.base)
    for problem in problems:
        print(f"docs_check: {problem}", file=sys.stderr)
    if problems:
        sys.exit(1)
    print(f"docs_check: ok ({len(markdown_files())} markdown files)")


if __name__ == "__main__":
    os.chdir(ROOT)
    main()
