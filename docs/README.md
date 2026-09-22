# Documentation

AgentDocker runs agents natively on the person's own computer: a per-host
daemon, a CLI and a desktop app. Docker and Podman are an optional addition,
never a requirement. macOS has local acceptance and Linux has graphical/package
CI coverage.
Windows managed sessions, ConPTY and portable ZIP packaging are merged and
exercised on a native runner. A current coworker desktop release and independent
machine acceptance remain open on all three platforms.

Sixteen documents, each the current contract for one thing. Dated audits,
plans and reviews from the way here were removed on September 21, 2026 and
live in git history (`git log -- docs`); nothing is added back as a document
when a line in an existing one will do.

## Using it

- [Root README](../README.md) — what it is, install, quick start, known limitations, how to report.
- [Guide](GUIDE.md) — every command, tool and screen, with recipes and the changelog.
- [Desktop UX](DESKTOP-UX.md) — the app screen by screen: what every control does and will not claim.
- [Guided setup](GUIDED-SETUP.md) — `agentdocker setup`: preview, apply, undo, health, the coordination skill.
- [Local build](LOCAL-BUILD.md) — build and install the desktop from source; update, rollback, retention.
- [Trial issue template](../.github/ISSUE_TEMPLATE/trial-report.md) — what a useful report carries.

## How it works

- [Architecture](ARCHITECTURE.md) — the model, the wire protocol, events, storage, semantics, the roadmap table.
- [Claude channel input](CLAUDE-CHANNEL-INPUT.md) — live messages into a Claude Code session; consent, receipts, reconnect.
- [Codex input](CODEX-INPUT.md) — the native Codex queue and receiver; hooks, receipts, recovery.
- [Remote connector](REMOTE-CONNECTOR.md) — agents that work inside a browser, through the opt-in MCP connector.
- [Iced design](ICED-DESIGN.md) — the desktop's interaction contracts and how they are validated.
- [Container engines](CONTAINER-ENGINES.md) — the optional Docker/Podman addition and its acceptance.
- [Windows port](WINDOWS-PORT.md) — what runs on Windows, what does not yet, and the evidence.
- [Coding instructions](../CLAUDE.md) — layout, conventions, the verification workflow, this documentation contract.
- [Portable coordination skill](../crates/cli/skills/agentdocker/SKILL.md) — the one instruction asset agents load.

## Where it stands

- [Remaining work](REMAINING-WORK.md) — the road to v1 and every open row, with evidence; close a row only with evidence.
- [Testing and benchmarks](TESTING-AND-BENCHMARKS.md) — the standard gate, coverage, fuzzing, benchmarks, and how results are reported.
- [Local trial](LOCAL-TRIAL.md) — the first-run and sustained-use trials on real machines.
- [Distribution setup](DISTRIBUTION-SETUP.md) — the tap, the release workflow, what a Developer ID is for.
- [Verification records](verification/INDEX.md) — one line per trial on real binaries; a new trial is a line here, not a file.
- [Real-engine verification](../tests/containers/README.md) — the separate Docker/Podman evidence.
