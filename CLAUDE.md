# AgentDocker — notes for coding agents

Docker-style control plane for AI agents: a per-host daemon (`agentd`) plus a CLI (`agentdocker`) that give agents of any vendor a registry, supervised processes, messaging, and time-limited leases on shared resources. Read `README.md` for the model and `docs/ARCHITECTURE.md` for the protocol and semantics.

## Layout

- `crates/core` — `agentdocker-core`: data model, wire protocol, pure coordination logic. No I/O, no async, no clocks (pass `now`). All semantics are unit-tested here.
- `crates/host` — `agentdocker-host`: host-side I/O both binaries need (project discovery from a working directory, process inspection). Stateless helpers; no daemon state.
- `crates/agentd` — the daemon, as a library (`agentd::main`); the `agentd` binary itself is built by `crates/cli` so one install ships both. `daemon.rs` state + handlers, `server.rs` socket loop + streaming, `supervisor.rs` process spawning + log capture, `watcher.rs` the per-checkout filesystem watcher feeding the ledger and branch refreshes, `store.rs` SQLite write-through persistence (JSON blobs; bump `SCHEMA_VERSION` only when a stored meaning changes).
- `crates/cli` — the `agentdocker` package: the `agentdocker` CLI and the `agentd` binary (`src/bin/agentd.rs`, one line). `client.rs` talks the protocol and starts the daemon on demand, `service.rs` installs it as a launchd/systemd user service, `format.rs` renders output, `mcp.rs` is the stdio MCP server (hand-rolled JSON-RPC), `hooks.rs` the Claude Code hooks adapter. Both talk to the daemon through the `Backend` trait in `client.rs`, whose test mock lets them be tested without a daemon.

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
- Every daemon state change emits an `EventKind`. New behaviour = new event variant.
- Protocol changes: update `protocol.rs`, the table in `docs/ARCHITECTURE.md`, and the CLI in the same PR.
- Errors returned to clients use `ErrorCode`; add a variant rather than overloading `Internal`.
