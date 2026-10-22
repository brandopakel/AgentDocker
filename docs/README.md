# Documentation

What each document is for.

## Using it

- [Root README](../README.md) — what it is, install, quick start, known limitations, how to report.
- [Guide](GUIDE.md) — every command, tool and screen, with recipes.
- [Desktop UX](DESKTOP-UX.md) — the app screen by screen: what every control does and will not claim.
- [Guided setup](GUIDED-SETUP.md) — `agentdocker setup`: preview, apply, undo, health, the coordination skill.
- [Local build](LOCAL-BUILD.md) — build and install the desktop from source; update, rollback, retention.
- [Trial issue template](../.github/ISSUE_TEMPLATE/trial-report.md) — what a useful report carries.

## How it works

- [Architecture](ARCHITECTURE.md) — the model, the wire protocol, events, storage and semantics.
- [Claude channel input](CLAUDE-CHANNEL-INPUT.md) — live messages into a Claude Code session; consent, receipts, reconnect.
- [Codex input](CODEX-INPUT.md) — the native Codex queue and receiver; hooks, receipts, recovery.
- [Remote connector](REMOTE-CONNECTOR.md) — clients of a person's Claude or ChatGPT account, through the opt-in MCP connector.
- [Iced design](ICED-DESIGN.md) — the desktop's interaction contracts and how they are validated.
- [Container engines](CONTAINER-ENGINES.md) — the optional Docker/Podman addition and how it is verified.
- [Windows port](WINDOWS-PORT.md) — what runs on Windows and what does not.
- [Coding instructions](../CLAUDE.md) — layout, conventions, the verification workflow, this documentation contract.
- [Portable coordination skill](../crates/cli/skills/agentdocker/SKILL.md) — the one instruction asset agents load.

## Testing and release

- [Testing and benchmarks](TESTING-AND-BENCHMARKS.md) — the standard gate, coverage, fuzzing, benchmarks, and how results are reported.
- [Trying a candidate](LOCAL-TRIAL.md) — how to try a build on a real machine, stage by stage.
- [Distribution setup](DISTRIBUTION-SETUP.md) — the tap, the release workflow, what a Developer ID is for.
- [Real-engine verification](../tests/containers/README.md) — how the Docker and Podman adapters are checked against real engines.
