#!/usr/bin/env python3
"""Name the workspace packages a change touches, for a targeted test run.

`scripts/verify.sh quick` runs the suites of the packages a branch changed
and of every package that depends on them, instead of the whole workspace.
This script decides which those are, from the files that differ between the
working tree and the merge base with a base ref (committed or not, plus
untracked files that are not ignored):

- a file under a workspace package's directory touches that package, and
  through the dependency graph (from `cargo metadata`) every package that
  depends on it; a renamed file touches both its old and its new place;
- a file the whole workspace depends on (the root manifest, the lockfile,
  the toolchain file, Cargo and nextest configuration) touches every package;
- any other file (documentation, scripts, packaging) touches no package.

It prints Cargo package selectors, one per line (`-p`, then the name), or
`--workspace` when everything is touched, or nothing when no package is;
`--output <file>` writes them there instead. The exit status is 0 when the
selection was made and 1 when Git or Cargo could not answer, so a caller
never mistakes a failure for "nothing changed".

    python3 scripts/touched_crates.py --base origin/main
    python3 scripts/touched_crates.py --base HEAD~1 --metadata metadata.json
"""
import argparse
import json
import os
import subprocess
import sys

# Files outside every package whose change rebuilds and retests all of them.
WORKSPACE_WIDE = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
    "clippy.toml",
    "rustfmt.toml",
}
WORKSPACE_WIDE_DIRS = (".cargo/", ".config/")


def workspace_packages(metadata):
    """The workspace's packages: name -> (directory relative to the root,
    names of the workspace packages it depends on)."""
    root = metadata["workspace_root"].rstrip("/") + "/"
    members = set(metadata.get("workspace_members", []))
    by_id = {p["id"]: p for p in metadata["packages"]}
    names = {by_id[m]["name"] for m in members if m in by_id}
    packages = {}
    for member in members:
        package = by_id.get(member)
        if package is None:
            continue
        manifest = package["manifest_path"]
        directory = os.path.dirname(manifest)
        if directory.startswith(root):
            directory = directory[len(root):]
        deps = {
            d["name"]
            for d in package.get("dependencies", [])
            if d["name"] in names and d["name"] != package["name"]
        }
        packages[package["name"]] = (directory.rstrip("/") + "/", deps)
    return packages


def dependents(packages):
    """name -> the workspace packages that depend on it, transitively."""
    direct = {name: set() for name in packages}
    for name, (_, deps) in packages.items():
        for dep in deps:
            direct[dep].add(name)
    closure = {}
    for name in packages:
        seen, frontier = set(), [name]
        while frontier:
            current = frontier.pop()
            for dependent in direct.get(current, ()):
                if dependent not in seen:
                    seen.add(dependent)
                    frontier.append(dependent)
        closure[name] = seen
    return closure


def touched(paths, packages):
    """The packages a list of changed paths (relative to the workspace root)
    touches, or None when the change is workspace-wide."""
    by_dir = sorted(packages.items(), key=lambda item: -len(item[1][0]))
    reverse = dependents(packages)
    result = set()
    for path in paths:
        if not path:
            continue
        if path in WORKSPACE_WIDE or path.startswith(WORKSPACE_WIDE_DIRS):
            return None
        for name, (directory, _) in by_dir:
            if path.startswith(directory):
                result.add(name)
                result.update(reverse[name])
                break
    return result


def git_paths(args, cwd=None):
    """A NUL-separated path listing from Git, as the exact paths."""
    out = subprocess.run(
        ["git", *args, "-z"], capture_output=True, check=True, cwd=cwd
    ).stdout
    return [p.decode("utf-8", "surrogateescape") for p in out.split(b"\0") if p]


def changed_paths(base, cwd=None):
    """Paths that differ from the merge base with `base`, committed or not,
    plus untracked files that are not ignored. Renames are not folded: a
    moved file counts at both its old and its new place."""
    merge_base = subprocess.run(
        ["git", "merge-base", "HEAD", base], capture_output=True, text=True, check=False, cwd=cwd
    )
    against = merge_base.stdout.strip() if merge_base.returncode == 0 else base
    changed = git_paths(["diff", "--name-only", "--no-renames", against], cwd)
    untracked = git_paths(["ls-files", "--others", "--exclude-standard"], cwd)
    return changed + untracked


def load_metadata(path):
    if path:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(out.stdout)


def selectors(names):
    """Cargo package selectors for a set of names, or `--workspace` for None."""
    if names is None:
        return ["--workspace"]
    args = []
    for name in sorted(names):
        args += ["-p", name]
    return args


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--base", default="origin/main", help="the ref the change is measured against")
    parser.add_argument("--metadata", help="a saved `cargo metadata` JSON file instead of running cargo")
    parser.add_argument("--output", help="write the selectors to this file instead of standard output")
    args = parser.parse_args()
    try:
        packages = workspace_packages(load_metadata(args.metadata))
        names = touched(changed_paths(args.base), packages)
    except (subprocess.CalledProcessError, OSError, KeyError, ValueError) as error:
        print(f"touched_crates: could not name the changed packages: {error}", file=sys.stderr)
        return 1
    text = "".join(f"{arg}\n" for arg in selectors(names))
    if args.output:
        with open(args.output, "w", encoding="utf-8") as handle:
            handle.write(text)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
