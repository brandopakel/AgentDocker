#!/usr/bin/env python3
"""List a pull request's open review threads: the ones still unresolved on
a line the current head still has.

A thread GitHub marks outdated sits on a line a later push changed; it no
longer blocks a merge, whether or not somebody pressed Resolve. This script
reads every review thread through the GitHub CLI (`gh`, already signed in)
and prints the open ones, one per line: the file and line, who started it,
and the first line of what they said. The exit status is 1 when any thread
is open, 2 when the thread list could not be read, and 0 otherwise, so a
merge step can gate on it.

    python3 scripts/pr_threads.py 123
    python3 scripts/pr_threads.py 123 --repo owner/name --include-outdated
"""
import argparse
import json
import subprocess
import sys

QUERY = """
query($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          isResolved
          isOutdated
          path
          line
          comments(first: 1) { nodes { author { login } body } }
        }
      }
    }
  }
}
"""


def open_threads(threads, include_outdated=False):
    """The threads that still need an answer: unresolved, and on a line the
    head still has unless outdated ones are asked for too."""
    return [
        t for t in threads
        if not t.get("isResolved") and (include_outdated or not t.get("isOutdated"))
    ]


def first_prose_line(body):
    """The first line of a comment that says something: not blank, not an
    HTML comment (a bot's marker), not a Markdown rule."""
    for line in body.splitlines():
        line = line.strip()
        if not line or line.startswith("<!--") or set(line) <= {"-", "*", "_"}:
            continue
        return line
    return ""


def describe(thread):
    first = (thread.get("comments") or {}).get("nodes") or [{}]
    comment = first[0] if first else {}
    author = ((comment.get("author") or {}).get("login")) or "unknown"
    summary = first_prose_line(comment.get("body") or "")
    line = thread.get("line")
    where = f"{thread.get('path') or '?'}:{line}" if line else (thread.get("path") or "?")
    return f"{where}  {author}: {summary}"


def repository_from_remote():
    out = subprocess.run(
        ["gh", "repo", "view", "--json", "owner,name"], capture_output=True, text=True, check=True
    )
    data = json.loads(out.stdout)
    return data["owner"]["login"], data["name"]


def fetch_threads(owner, name, number):
    threads, after = [], None
    while True:
        command = [
            "gh", "api", "graphql",
            "-f", f"query={QUERY}",
            "-F", f"owner={owner}", "-F", f"name={name}", "-F", f"number={number}",
        ]
        if after:
            command += ["-F", f"after={after}"]
        out = subprocess.run(command, capture_output=True, text=True, check=True)
        page = json.loads(out.stdout)["data"]["repository"]["pullRequest"]["reviewThreads"]
        threads.extend(page["nodes"])
        if not page["pageInfo"]["hasNextPage"]:
            return threads
        after = page["pageInfo"]["endCursor"]


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("number", type=int, help="the pull request number")
    parser.add_argument("--repo", help="owner/name; the current checkout's remote when absent")
    parser.add_argument(
        "--include-outdated", action="store_true",
        help="count unresolved threads on lines a later push changed as open too",
    )
    args = parser.parse_args()
    try:
        if args.repo:
            owner, name = args.repo.split("/", 1)
        else:
            owner, name = repository_from_remote()
        threads = fetch_threads(owner, name, args.number)
    except (subprocess.CalledProcessError, KeyError, ValueError, json.JSONDecodeError) as error:
        print(f"pr_threads: could not read the review threads of #{args.number}: {error}", file=sys.stderr)
        return 2
    pending = open_threads(threads, args.include_outdated)
    for thread in pending:
        print(describe(thread))
    if pending:
        print(f"#{args.number}: {len(pending)} open review thread(s)", file=sys.stderr)
        return 1
    print(f"#{args.number}: no open review threads", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
