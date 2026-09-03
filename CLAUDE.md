# AgentDocker — notes for coding agents

Docker-style control plane for AI agents: a per-host daemon (`agentd`) plus a CLI (`agentdocker`) that give agents of any vendor a registry, supervised processes, messaging, and time-limited leases on shared resources. Read `README.md` for the model and `docs/ARCHITECTURE.md` for the protocol and semantics.

## Layout

- `crates/core` — `agentdocker-core`: data model, wire protocol, pure coordination logic. No I/O, no async, no clocks (pass `now`). All semantics are unit-tested here.
- `crates/agentd` — the daemon. `daemon.rs` state + handlers, `server.rs` socket loop + streaming, `supervisor.rs` process spawning + log capture.
- `crates/cli` — `agentdocker`. `client.rs` talks the protocol, `format.rs` renders output.

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Run an isolated daemon for manual testing: `AGENTDOCKER_HOME=/tmp/ad-test agentd` and use the same env var with the CLI.

## Conventions

- Rust 2024 edition, `resolver = "3"`. Dependencies are declared once in the workspace `Cargo.toml` and inherited.
- Core stays pure: if a change needs I/O, a clock, or async, it belongs in `agentd` or the CLI.
- `agentd` locks at most one mutex at a time and never across an `.await`. Keep it that way.
- Every daemon state change emits an `EventKind`. New behaviour = new event variant.
- Protocol changes: update `protocol.rs`, the table in `docs/ARCHITECTURE.md`, and the CLI in the same PR.
- Errors returned to clients use `ErrorCode`; add a variant rather than overloading `Internal`.
