# AgentDocker — notes for coding agents

Read `README.md` for the model, `docs/README.md` for what each document is for, `docs/ARCHITECTURE.md` for the protocol and semantics and `docs/REMAINING-WORK.md` for what is still open.

## Layout

- `crates/core` — `agentdocker-core`: data model, wire protocol, pure coordination logic. All semantics are unit-tested here.
- `crates/host` — `agentdocker-host`: host-side I/O both binaries need (project discovery from a working directory, process inspection). Stateless helpers; no daemon state.
- `crates/agentd` — the daemon, as a library (`agentd::main`); the `agentd` binary itself is built by `crates/cli` so one install ships both. `daemon.rs` holds one synchronous state mutex covering memory, SQLite writes and ordered event publication; `store.rs` is SQLite write-through persistence (JSON blobs; bump `SCHEMA_VERSION` only when a stored meaning changes).
- `crates/cli` — the `agentdocker` package: the CLI and the `agentd` binary (`src/bin/agentd.rs`, one line). The MCP server and hooks adapter talk to the daemon through the `Backend` trait in `client.rs`, whose test mock lets them be tested without a daemon.
- `crates/ui` — `agentdocker-ui`, the Iced desktop app with the tiny-skia software renderer. No HTTP; `agentdocker ui` launches it. See `docs/ICED-DESIGN.md` for interaction contracts and validation.

## Commands

Run an isolated daemon for manual testing: `AGENTDOCKER_HOME=/tmp/ad-test agentd` and use the same env var with the CLI (or just run the CLI with that env var: a client that cannot connect starts the daemon itself; `AGENTDOCKER_NO_AUTOSTART=1` turns that off).

## Conventions

- Core stays pure: if a change needs I/O, a clock, or async, it belongs in `agentd` or the CLI.
- `agentd` locks at most one mutex at a time and never across an `.await`. Keep it that way.
- Every daemon state change emits an `EventKind`. New behaviour = new event variant.
- Protocol changes: update `protocol.rs`, the table in `docs/ARCHITECTURE.md`, and the CLI in the same PR.
- Errors returned to clients use `ErrorCode`; add a variant rather than overloading `Internal`.
- When several agents share a checkout through AgentDocker, work in your own worktree (`agentdocker worktree-create --branch <name> [--from <ref>]`) and hold `path:` on what you edit, released with a summary when done.
- Nothing personal belongs in the tree: no home paths, host or machine names, account details, session or agent ids from a real run, or trial evidence. Fixtures use made-up names.

## Documentation contract

The docs are the record of what the repository does and how far it is delivered — each document the current contract for one thing, and no dated plans, audits or reviews. A new document needs a reason a line in an existing one cannot serve. Every change that alters behaviour, a contract or a delivery status updates them in the same PR:

- `docs/ARCHITECTURE.md` for a protocol, event, error code, schema or semantic change (the request/response table, the events list, the phase rows).
- `docs/REMAINING-WORK.md` for what an open item now has in source and what is still open.
- `docs/README.md` (the docs index): a new document is linked there.
- `docs/GUIDE.md`, `docs/DESKTOP-UX.md` or the root `README.md` when a command, tool or screen changes for the person using it.
- Evidence from a trial on real binaries goes in the pull request, never in the repository.

`python3 scripts/docs_check.py` runs in `scripts/verify.sh check` and in CI: the docs index must list every document, every relative link must resolve, and a change under `crates/`, `scripts/`, `packaging/`, `.github/`, `install.sh` or `Makefile` must come with a documentation change or with a commit whose message has a line starting `Docs:` saying why none is due (`Docs: unchanged, a rename with no behaviour change`).

## Verification

Run `bash scripts/verify.sh check` before opening or updating a ready PR: nextest (zero retries), doctests, formatting, strict Clippy, installer tests, packaging and the release build. Use targeted tests while editing, each worktree with its own Cargo target directory; `python3 scripts/build_storage.py` reports cache sizes. Run Cargo without a session's `AGENTDOCKER_AGENT_ID`, `AGENTDOCKER_SOCKET` or `AGENTDOCKER_HOME` in the environment: the hooks and service tests expect none (`verify.sh` drops them).

When several agents build on one machine, one build runs at a time under the exclusive lease `task:local-cargo-campaign`: `verify.sh` takes and releases it itself, and before a direct `cargo` run take it with `agentdocker claim task:local-cargo-campaign --wait 600 --note "…"` and release it as soon as the run ends.

`bash scripts/verify.sh coverage`, `bench` and `fuzz` run the other suites; see `docs/TESTING-AND-BENCHMARKS.md`. Release artifacts are built in CI only: a local release build carries the builder's paths.

GitHub CI and code review are the review workflow: address valid findings, explain declined ones with evidence, and wait for checks on the final commit before merging.
