#!/usr/bin/env python3
"""Keep one GitHub issue open while a scheduled workflow fails on main.

A scheduled run's failure is otherwise seen only by somebody asking for it
by name. This opens an issue on the first failing scheduled run, adds each
later failing run to it, and closes it when a scheduled run passes again: one
issue per failing episode, and a recurrence opens a new one.

Only the automation's own issue is ever touched: one whose body carries the
workflow's marker and whose author is `github-actions[bot]`, and only the
bot's own comments count as its receipts. Every effect names its run and
attempt. A result counts only when nothing newer, by run number and attempt,
has decided: in the Actions results (success, failure or timed out; a
cancelled or running attempt decides nothing, and its run's newest decisive
attempt is read instead) or in the automation's own receipts. A counted
result brings the issues to the state it calls for, so a retry finishes what
an earlier attempt left half done and a repeat changes nothing.

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


# A workflow run's conclusion that decides the schedule's state; any other
# (cancelled, skipped, neutral, still running) decides nothing.
DECISIVE = {"success": "success", "failure": "failure", "timed_out": "failure"}


def receipts(issue):
    """The run markers the automation itself wrote on an issue: in its body
    (the bot's, for an owned issue) and in the bot's own comments only."""
    texts = [issue["body"]] + [comment["body"] for comment in issue["comments"] if comment["author"] == BOT]
    return [(int(number), int(attempt), result) for text in texts for number, attempt, result in RUN.findall(text)]


def decide(workflow, run, history, issues):
    """What to do, as a list of effects, from this run ({number, attempt,
    result, url}), the decisive results of the workflow's other scheduled
    runs and attempts ([(number, attempt, result)]) and the repository's
    issues ([{number, open, author, title, body, comments: [{author,
    body}]}]). Pure.

    A result counts only when nothing newer has decided, by run number and
    attempt, in the Actions results or in the automation's own receipts.
    It then brings the issues to the state it calls for (an owned issue
    open and naming it, or none open), so a retry finishes what an earlier
    attempt left half done."""
    if run["result"] not in ("failure", "success"):
        return []
    owned = [issue for issue in issues if issue["author"] == BOT and marker(workflow) in issue["body"]]
    key = (run["number"], run["attempt"])
    seen = [(number, attempt) for number, attempt, _result in history]
    seen += [(number, attempt) for issue in owned for number, attempt, _result in receipts(issue)]
    if any(other > key for other in seen):
        return []  # a newer result has decided
    said = run_marker(run["number"], run["attempt"], run["result"])
    open_owned = sorted((issue for issue in owned if issue["open"]), key=lambda issue: issue["number"])
    names = lambda issue: any(receipt[:2] == key for receipt in receipts(issue))
    if run["result"] == "failure":
        text = f"Scheduled run {run['number']} (attempt {run['attempt']}) failed: {run['url']}\n\n{said}"
        if open_owned:
            target = open_owned[-1]
            return [] if names(target) else [("comment", target["number"], text)]
        if any(names(issue) for issue in owned):
            return []  # recorded, and somebody closed it since: left as they left it
        title = f"Scheduled {workflow} fails on main"
        body = (
            f"{marker(workflow)}\nThe scheduled `{workflow}` run on main fails. This issue stays open "
            f"while it does and is closed by the next scheduled run that passes.\n\n{text}"
        )
        return [("create", title, body)]
    text = f"Scheduled run {run['number']} (attempt {run['attempt']}) passed: {run['url']}\n\n{said}"
    effects = []
    for issue in open_owned:
        if (run["number"], run["attempt"], "success") not in receipts(issue):
            effects.append(("comment", issue["number"], text))
        effects.append(("close", issue["number"]))
    return effects


class Gh:
    """GitHub through `gh api`, for one repository."""

    def __init__(self, repository):
        self.repository = repository

    def lines(self, *args):
        """One JSON value per line, as `--jq` prints them."""
        done = subprocess.run(["gh", "api", *args], capture_output=True, text=True, timeout=60, check=True)
        return [json.loads(line) for line in done.stdout.splitlines() if line.strip()]

    def write(self, *args):
        subprocess.run(["gh", "api", *args], capture_output=True, text=True, timeout=60, check=True)

    ISSUE = '.[] | select(.pull_request == null) | {number, state, title, body, author: .user.login}'

    def issues(self, workflow):
        bot = "github-actions%5Bbot%5D"
        base = f"repos/{self.repository}/issues?creator={bot}"
        # Every open one, however many pages; and the recent closed ones,
        # whose receipts say what has already decided.
        found = self.lines("--paginate", f"{base}&state=open&per_page=100", "--jq", self.ISSUE)
        found += self.lines(f"{base}&state=closed&sort=updated&direction=desc&per_page=50", "--jq", self.ISSUE)
        issues = []
        for issue in found:
            body = issue.get("body") or ""
            if marker(workflow) not in body:
                continue
            comments = self.lines("--paginate", f"repos/{self.repository}/issues/{issue['number']}/comments",
                                  "--jq", ".[] | {author: .user.login, body}")
            issues.append({
                "number": issue["number"],
                "open": issue["state"] == "open",
                "author": issue["author"],
                "title": issue["title"],
                "body": body,
                "comments": [{"author": c.get("author"), "body": c.get("body") or ""} for c in comments],
            })
        return issues

    def history(self, workflow, number, attempt):
        """The newest decisive attempt of each recent scheduled run, walking
        back past a cancelled or still-running one; for this run, only its
        earlier attempts."""
        runs = self.lines(f"repos/{self.repository}/actions/workflows/{workflow}/runs"
                          "?event=schedule&branch=main&per_page=30",
                          "--jq", ".workflow_runs[] | {id, run_number, run_attempt, status, conclusion}")
        found = []
        for run in runs:
            latest = run["run_attempt"] if run["run_number"] != number else attempt - 1
            for each in range(latest, 0, -1):
                if run["run_number"] != number and each == run["run_attempt"]:
                    status, conclusion = run["status"], run["conclusion"]
                else:
                    try:
                        (past,) = self.lines(f"repos/{self.repository}/actions/runs/{run['id']}/attempts/{each}",
                                             "--jq", "{status, conclusion}")
                    except (subprocess.CalledProcessError, ValueError):
                        continue
                    status, conclusion = past["status"], past["conclusion"]
                if status == "completed" and conclusion in DECISIVE:
                    found.append((run["run_number"], each, DECISIVE[conclusion]))
                    break
        return found

    def apply(self, effect):
        kind = effect[0]
        if kind == "create":
            self.write(f"repos/{self.repository}/issues", "-f", f"title={effect[1]}", "-f", f"body={effect[2]}")
        elif kind == "comment":
            self.write(f"repos/{self.repository}/issues/{effect[1]}/comments", "-f", f"body={effect[2]}")
        elif kind == "close":
            self.write("-X", "PATCH", f"repos/{self.repository}/issues/{effect[1]}", "-f", "state=closed")


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
    history = client.history(args.workflow, run["number"], run["attempt"])
    effects = decide(args.workflow, run, history, client.issues(args.workflow))
    for effect in effects:
        client.apply(effect)
    print(f"scheduled_report: {len(effects)} effect(s) for run {run['number']}.{run['attempt']} ({run['result']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
