#!/usr/bin/env python3
"""Keep one GitHub issue open while a scheduled workflow fails on main.

A scheduled run's failure is otherwise seen only by somebody asking for it
by name. This opens an issue on the first failing scheduled run, adds each
later failing run to it, and closes it when a scheduled run passes again: one
issue per failing episode, and a recurrence opens a new one.

Only the automation's own issue is ever touched: one whose body carries the
workflow's marker and whose author is `github-actions[bot]`. Every effect
names its run and attempt, so a repeated attempt changes nothing. A result is
acted on only while no later scheduled run of the workflow has completed, so
a rerun of an older run can neither close a newer failure nor reopen one a
newer pass closed; a later attempt of the same run may settle its own result.

    python3 scripts/scheduled_report.py --workflow fuzz.yml --result failure

It reads `GITHUB_REPOSITORY`, `GITHUB_RUN_NUMBER`, `GITHUB_RUN_ATTEMPT`,
`GITHUB_RUN_ID` and `GITHUB_SERVER_URL`, and talks to GitHub through `gh`.
"""
import argparse
import json
import os
import re
import subprocess
import sys

BOT = "github-actions[bot]"


def marker(workflow):
    return f"<!-- agentdocker:scheduled-incident {workflow} -->"


def run_marker(number, attempt, result):
    return f"<!-- run {number}.{attempt} {result} -->"


RUN = re.compile(r"<!-- run (\d+)\.(\d+) (failure|success) -->")


def decide(workflow, run, completed, issues):
    """What to do, as a list of effects, from this run ({number, attempt,
    result, url}), the workflow's other completed scheduled runs
    ([(number, attempt, conclusion)]) and the repository's issues
    ([{number, open, author, title, body, comments}]). Pure."""
    if run["result"] not in ("failure", "success"):
        return []
    owned = [
        issue for issue in issues if issue["author"] == BOT and marker(workflow) in issue["body"]
    ]
    named = f"<!-- run {run['number']}.{run['attempt']} "
    for issue in owned:
        if any(named in text for text in [issue["body"], *issue["comments"]]):
            return []  # this attempt has had its effect
    if any(number > run["number"] for number, _attempt, _conclusion in completed):
        return []  # a later scheduled run decided since
    open_owned = sorted((issue for issue in owned if issue["open"]), key=lambda issue: issue["number"])
    said = run_marker(run["number"], run["attempt"], run["result"])
    if run["result"] == "failure":
        text = f"Scheduled run {run['number']} (attempt {run['attempt']}) failed: {run['url']}\n\n{said}"
        if open_owned:
            return [("comment", open_owned[-1]["number"], text)]
        title = f"Scheduled {workflow} fails on main"
        body = (
            f"{marker(workflow)}\nThe scheduled `{workflow}` run on main fails. This issue stays open "
            f"while it does and is closed by the next scheduled run that passes.\n\n{text}"
        )
        return [("create", title, body)]
    if not open_owned:
        return []
    text = f"Scheduled run {run['number']} (attempt {run['attempt']}) passed: {run['url']}\n\n{said}"
    number = open_owned[-1]["number"]
    return [("comment", number, text), ("close", number)]


class Gh:
    """GitHub through `gh api`, for one repository."""

    def __init__(self, repository):
        self.repository = repository

    def api(self, *args):
        done = subprocess.run(["gh", "api", *args], capture_output=True, text=True, timeout=60, check=True)
        return json.loads(done.stdout) if done.stdout.strip() else None

    def issues(self, workflow):
        found = self.api("-X", "GET", f"repos/{self.repository}/issues", "-f", "state=all",
                         "-f", f"creator={BOT}", "-f", "per_page=100")
        issues = []
        for issue in found or []:
            body = issue.get("body") or ""
            if "pull_request" in issue or marker(workflow) not in body:
                continue
            comments = self.api(f"repos/{self.repository}/issues/{issue['number']}/comments",
                                "--paginate") or []
            issues.append({
                "number": issue["number"],
                "open": issue["state"] == "open",
                "author": issue["user"]["login"],
                "title": issue["title"],
                "body": body,
                "comments": [comment.get("body") or "" for comment in comments],
            })
        return issues

    def completed(self, workflow, current):
        found = self.api("-X", "GET", f"repos/{self.repository}/actions/workflows/{workflow}/runs",
                         "-f", "event=schedule", "-f", "branch=main", "-f", "status=completed",
                         "-f", "per_page=30")
        return [
            (run["run_number"], run["run_attempt"], run["conclusion"])
            for run in (found or {}).get("workflow_runs", [])
            if run["run_number"] != current
        ]

    def apply(self, effect):
        kind = effect[0]
        if kind == "create":
            self.api(f"repos/{self.repository}/issues", "-f", f"title={effect[1]}", "-f", f"body={effect[2]}")
        elif kind == "comment":
            self.api(f"repos/{self.repository}/issues/{effect[1]}/comments", "-f", f"body={effect[2]}")
        elif kind == "close":
            self.api("-X", "PATCH", f"repos/{self.repository}/issues/{effect[1]}", "-f", "state=closed")


def main(client=None, environ=None):
    environ = environ if environ is not None else os.environ
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--workflow", required=True)
    parser.add_argument("--result", required=True, help="the job's result: success, failure, cancelled, skipped")
    args = parser.parse_args()
    run = {
        "number": int(environ["GITHUB_RUN_NUMBER"]),
        "attempt": int(environ.get("GITHUB_RUN_ATTEMPT", "1")),
        "result": args.result,
        "url": f"{environ.get('GITHUB_SERVER_URL', 'https://github.com')}/{environ['GITHUB_REPOSITORY']}"
               f"/actions/runs/{environ['GITHUB_RUN_ID']}",
    }
    client = client or Gh(environ["GITHUB_REPOSITORY"])
    effects = decide(args.workflow, run, client.completed(args.workflow, run["number"]), client.issues(args.workflow))
    for effect in effects:
        client.apply(effect)
    print(f"scheduled_report: {len(effects)} effect(s) for run {run['number']}.{run['attempt']} ({run['result']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
