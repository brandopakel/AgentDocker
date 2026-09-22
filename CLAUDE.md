# AgentDocker — notes for coding agents

Native local orchestration for AI agents: a per-host daemon (`agentd`) plus a CLI (`agentdocker`) that give agents of any vendor a registry, supervised processes, messaging, and time-limited leases on shared resources. Read `README.md` for the model, `docs/README.md` for what each document is for, `docs/ARCHITECTURE.md` for the protocol and semantics and `docs/REMAINING-WORK.md` for the road to v1.

## Layout

- `crates/core` — `agentdocker-core`: data model, wire protocol, pure coordination logic. No I/O, no async, no clocks (pass `now`). All semantics are unit-tested here.
- `crates/host` — `agentdocker-host`: host-side I/O both binaries need (project discovery from a working directory, process inspection). Stateless helpers; no daemon state.
- `crates/agentd` — the daemon, as a library (`agentd::main`); the `agentd` binary itself is built by `crates/cli` so one install ships both. `daemon.rs` one synchronous state mutex covering memory, SQLite writes and ordered event publication + handlers, `watcher.rs` per-checkout observations, `server.rs` socket loop + streaming, `supervisor.rs` process spawning + log capture, `store.rs` SQLite write-through persistence (JSON blobs; bump `SCHEMA_VERSION` only when a stored meaning changes).
- `crates/cli` — the `agentdocker` package: the `agentdocker` CLI and the `agentd` binary (`src/bin/agentd.rs`, one line). `client.rs` talks the protocol and starts the daemon on demand, `service.rs` installs it as a launchd/systemd user service, `format.rs` renders output, `mcp.rs` is the stdio MCP server (hand-rolled JSON-RPC), `hooks.rs` the Claude Code hooks adapter. Both talk to the daemon through the `Backend` trait in `client.rs`, whose test mock lets them be tested without a daemon.
- `crates/ui` — `agentdocker-ui`, the Iced desktop app with the tiny-skia software renderer. `app/shell.rs` holds state transitions, `app/view.rs` presentation, `catalog.rs` private project preferences, and `controls.rs`/`accessibility.rs` keyboard and native accessibility support. The existing blocking socket client and bounded request/event/terminal workers remain. No HTTP; `agentdocker ui` launches it. See `docs/ICED-DESIGN.md` for interaction contracts and validation.

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Run an isolated daemon for manual testing: `AGENTDOCKER_HOME=/tmp/ad-test agentd` and use the same env var with the CLI (or just run the CLI with that env var: a client that cannot connect starts the daemon itself; `AGENTDOCKER_NO_AUTOSTART=1` turns that off).

## Conventions

- Rust 2024 edition, `resolver = "3"`. Dependencies are declared once in the workspace `Cargo.toml` and inherited.
- Core stays pure: if a change needs I/O, a clock, or async, it belongs in `agentd` or the CLI.
- `agentd` locks at most one mutex at a time and never across an `.await`. Keep it that way.
- Work in a private worktree, made with `agentdocker worktree-create --branch <name> [--from <ref>]` so commits you make there with git are journaled as yours; a worktree made by hand is yours while you hold `path:<its directory>` or `branch:<its branch>`, and `external` otherwise. Hold `path:` on any checkout you are editing, released with a summary when done: the daemon can refuse a peer's edit only while someone holds the path.
- Every daemon state change emits an `EventKind`. New behaviour = new event variant.
- Protocol changes: update `protocol.rs`, the table in `docs/ARCHITECTURE.md`, and the CLI in the same PR.
- Errors returned to clients use `ErrorCode`; add a variant rather than overloading `Internal`.

## Documentation contract

The docs are the record of what the repository does and how far it is delivered — sixteen documents, each the current contract for one thing, and no dated plans, audits or reviews (those were cut on September 21, 2026; history keeps them). A new document needs a reason a line in an existing one cannot serve. Every change that alters behaviour, a contract or a delivery status updates them in the same PR, whoever makes it:

- `docs/ARCHITECTURE.md` for a protocol, event, error code, schema or semantic change (the request/response table, the events list, the phase rows).
- `docs/REMAINING-WORK.md` for the disposition of an open item: what is now in source, what evidence exists, what is still open. Close a row only with evidence, and say what remains.
- `docs/README.md` (the docs index): a new document is linked there, and the audit table's row for a document changes when that document's delivery state changes.
- One line in `docs/verification/INDEX.md` for a trial on real binaries (date, what, source sha, result); the full evidence stays where the trial ran and in the PR, not in the repository.
- `docs/GUIDE.md`, `docs/DESKTOP-UX.md` or the root `README.md` when a command, tool or screen changes for the person using it.

`python3 scripts/docs_check.py` runs in `scripts/verify.sh check` and in CI: the docs index must list every document, every relative link must resolve, and a change under `crates/`, `scripts/`, `packaging/`, `.github/`, `install.sh` or `Makefile` must come with a documentation change or with a commit whose message has a line starting `Docs:` saying why none is due (`Docs: unchanged, a rename with no behaviour change`). That line is a statement to reviewers, not a way around the contract.

## Standard verification workflow

Use `bash scripts/verify.sh check` before opening or updating a ready PR. The standard suite is nextest (zero retries, JUnit), separate doctests, formatting, strict Clippy, installer tests, packaging and release build. Use targeted tests while editing. Each worktree keeps its own Cargo target directory. Before direct Cargo commands, run `python3 scripts/build_storage.py`. One local build campaign runs at a time, and the agents on this machine hold the exclusive lease `task:local-cargo-campaign` while theirs does: `verify.sh` takes and releases it for you (waiting up to `AGENTDOCKER_CAMPAIGN_WAIT` seconds, 600 by default, then stopping rather than starting on top of another's; a lease you already hold is kept; it renews the lease while the run lasts and stops the run, ending only what it started, if a renewal fails; when the daemon cannot tell which session is calling — a shell tool's child, say — the run registers itself as an agent, `verify-<pid>`, for its duration; where a daemon answers, a run holds the lease or does not run), and before direct `cargo` runs you take it yourself — `agentdocker claim task:local-cargo-campaign --wait 600 --note "what and where"`, then `agentdocker release <lease id> --summary …` (the lease id from the claim, not the resource) as soon as the run ends. A lease you took through your MCP tools is yours already: run `AGENTDOCKER_CAMPAIGN_LEASE=off bash scripts/verify.sh check` under it. The script drops a managed session's `AGENTDOCKER_AGENT_ID`, `AGENTDOCKER_SOCKET` and `AGENTDOCKER_HOME` from the suites' environment; do the same before direct `cargo test` runs, since the hooks and service tests expect none of them. Nobody stops another agent's processes; the lease is what says whose turn it is. Keep at most two debug caches; preserve reports and remove inactive generated caches before they accumulate. Never clean another session’s active build directory.

Use `bash scripts/verify.sh coverage` to inspect untested branches; `bench` for Criterion and native Unix-socket workloads with code/environment provenance; `fuzz` for bounded nightly protocol/resource-key campaigns. Add meaningful Proptest scenarios for coordination state transitions and retain minimized failures. See `docs/TESTING-AND-BENCHMARKS.md` for tools, contracts and reporting. Never describe a benchmark from different source content or an image as validation of the current state.

GitHub CI and CodeRabbit remain the review workflow. Address valid review findings, explain declined suggestions with evidence, and wait for checks on the final commit before integration. Performance data is initially advisory; correctness remains blocking. Bencher reporting runs only on trusted branch jobs with configured credentials; fork PRs still produce local artifacts. Docker and Podman get separate real-engine evidence.
