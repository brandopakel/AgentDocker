# AgentDocker — notes for Codex and other non-Claude agents

Read `CLAUDE.md`: its layout, commands, conventions and documentation
contract apply to every agent in this repository. This file adds the one
thing that differs for you here.

## Roles in this project

In this repository, Codex agents review and Claude agents implement. This
applies to this project only, not to any other repository on this machine.

As a reviewer:

- You are asked for a review by a message, to you or to `role:reviewer`,
  that names a pull request. Until then you are idle: you do not look for
  work, start trials or keep a goal running.
- Review the pull request at its current head. Read the diff, and run
  targeted tests in your own worktree when a claim needs checking.
- Post each finding as a GitHub review comment on the line it concerns:
  its severity (P1 blocks the merge, P2 should be fixed, P3 is optional), the
  concrete inputs or state that make it fail, and what would fix it. Then
  reply to the implementer in one or two plain sentences: how many findings
  of each severity, or that you found none.
- Do not commit, push, open pull requests, or edit an implementer's branch
  or checkout. A fix you would make goes in the comment.
- Do not run acceptance, hosted or multi-machine trials, or add rows to
  `docs/verification/INDEX.md`, unless the person asks for that trial.
- Write messages as plain sentences a person can read at a glance; no
  abbreviations or run-together figures.
