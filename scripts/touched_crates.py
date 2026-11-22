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
`--output <file>` writes them there instead. `--doc-output <file>` also
writes the selectors for the doctest step: the touched packages that have a
library target, since only a library has doctests and Cargo refuses `--doc`
for a selection with none (a change to the CLI alone); `--workspace` when
everything is touched; nothing when no touched package has a library. The
exit status is 0 when the selection was made and 1 when Git or Cargo could
not answer, so a caller never mistakes a failure for "nothing changed".

`--fuzz-output <file>` writes `true` or `false`: whether the change reaches
the fuzz targets, so `quick` compiles them. It is `true` for a file under
`fuzz/`, for a whole-workspace change, and for a selected package the fuzz
crate depends on directly or through another, from the fuzz workspace's
own `cargo metadata` (`--fuzz-metadata <file>`, the output the run already
saved when it resolved that workspace's lockfile). When that cannot be
read the answer is `true`: the targets are compiled rather than skipped.

    python3 scripts/touched_crates.py --base origin/main
    python3 scripts/touched_crates.py --base HEAD~1 --metadata metadata.json
"""
import argparse
import json
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
# The target kinds Cargo counts as a library (`Target::is_lib`).
LIBRARY_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}


def slashes(path):
    """A path with forward slashes, as Git prints paths on every platform;
    Cargo's metadata uses the platform separator."""
    return path.replace("\\", "/")


def workspace_packages(metadata):
    """The workspace's packages: name -> (directory relative to the root,
    names of the workspace packages it depends on)."""
    root = slashes(metadata["workspace_root"]).rstrip("/") + "/"
    members = set(metadata.get("workspace_members", []))
    by_id = {p["id"]: p for p in metadata["packages"]}
    names = {by_id[m]["name"] for m in members if m in by_id}
    packages = {}
    for member in members:
        package = by_id.get(member)
        if package is None:
            continue
        manifest = slashes(package["manifest_path"])
        directory = manifest.rsplit("/", 1)[0] if "/" in manifest else ""
        if directory.startswith(root):
            directory = directory[len(root):]
        deps = {
            d["name"]
            for d in package.get("dependencies", [])
            if d["name"] in names and d["name"] != package["name"]
        }
        packages[package["name"]] = (directory.rstrip("/") + "/", deps)
    return packages


def libraries(metadata):
    """The workspace packages with a library target: the only ones that
    have doctests."""
    members = set(metadata.get("workspace_members", []))
    return {
        package["name"]
        for package in metadata["packages"]
        if package["id"] in members
        and any(LIBRARY_KINDS & set(target.get("kind", [])) for target in package.get("targets", []))
    }


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


def doc_selectors(names, libs):
    """Selectors for the doctest step: the whole workspace for None, else
    the touched packages that have a library, possibly none."""
    if names is None:
        return ["--workspace"]
    return selectors(names & libs)


def fuzz_closure(metadata):
    """The path packages the fuzz crate reaches, directly or through
    another, by name, from its own workspace's `cargo metadata`; None when
    the graph cannot be read."""
    try:
        by_id = {package["id"]: package for package in metadata["packages"]}
        resolve = metadata["resolve"]
        root = resolve.get("root") or next(
            package["id"] for package in metadata["packages"] if package["name"] == "agentdocker-fuzz"
        )
        deps = {node["id"]: [dep["pkg"] for dep in node.get("deps", [])] for node in resolve["nodes"]}
    except (KeyError, TypeError, AttributeError, StopIteration):
        return None
    if root not in deps:
        return None  # the graph does not reach the fuzz crate: not told
    reached, stack = set(), [root]
    while stack:
        for dep in deps.get(stack.pop(), []):
            if dep not in reached:
                reached.add(dep)
                stack.append(dep)
    # A path package has no registry or Git source: the workspace's own.
    return {by_id[dep]["name"] for dep in reached if dep in by_id and by_id[dep].get("source") is None}


def fuzz_reached(paths, names, fuzz_metadata_path):
    """Whether a change reaches the fuzz targets; True when that cannot be
    told."""
    if any(path == "fuzz" or path.startswith("fuzz/") for path in paths):
        return True
    if names is None:
        return True
    if not names:
        return False
    try:
        with open(fuzz_metadata_path, encoding="utf-8") as handle:
            closure = fuzz_closure(json.load(handle))
    except (OSError, ValueError, TypeError):
        return True
    return closure is None or bool(names & closure)


def write(path, args):
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("".join(f"{arg}\n" for arg in args))


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--base", default="origin/main", help="the ref the change is measured against")
    parser.add_argument("--metadata", help="a saved `cargo metadata` JSON file instead of running cargo")
    parser.add_argument("--output", help="write the selectors to this file instead of standard output")
    parser.add_argument("--doc-output", help="also write the doctest step's selectors to this file")
    parser.add_argument("--fuzz-metadata", help="the fuzz workspace's saved `cargo metadata` JSON")
    parser.add_argument("--fuzz-output", help="write whether the change reaches the fuzz targets to this file")
    args = parser.parse_args()
    try:
        metadata = load_metadata(args.metadata)
        packages = workspace_packages(metadata)
        libs = libraries(metadata)
        paths = changed_paths(args.base)
        names = touched(paths, packages)
    except (subprocess.CalledProcessError, OSError, KeyError, ValueError) as error:
        print(f"touched_crates: could not name the changed packages: {error}", file=sys.stderr)
        return 1
    if args.output:
        write(args.output, selectors(names))
    else:
        sys.stdout.write("".join(f"{arg}\n" for arg in selectors(names)))
    if args.doc_output:
        write(args.doc_output, doc_selectors(names, libs))
    if args.fuzz_output:
        reached = fuzz_reached(paths, names, args.fuzz_metadata) if args.fuzz_metadata else True
        with open(args.fuzz_output, "w", encoding="utf-8") as handle:
            handle.write("true\n" if reached else "false\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
