# AgentDocker — notes for coding agents

Read `README.md` for the model, `docs/README.md` for what each document is for and `docs/ARCHITECTURE.md` for the protocol and semantics.

## Layout

- `crates/core` — `agentdocker-core`: data model, wire protocol, pure coordination logic. All semantics are unit-tested here.
- `crates/host` — `agentdocker-host`: host-side I/O the CLI and the daemon both need (project discovery from a working directory, process inspection). Stateless helpers; no daemon state.
- `crates/agentd` — the daemon, as a library (`agentd::main_from`), run by the one `agentdocker` executable when invoked as `agentd`. `daemon.rs` holds one synchronous state mutex covering memory, SQLite writes and ordered event publication, with each area's handlers in a module under `daemon/`; `store.rs`, with its areas under `store/`, is SQLite write-through persistence (JSON blobs; bump `SCHEMA_VERSION` only when a stored meaning changes).
- `crates/cli` — the `agentdocker` package: one executable that is the CLI, and the daemon when invoked as `agentd` (the hard link packages ship beside it; `scripts/daemon_link.py` makes it in a build directory) or as `agentdocker daemon run`. `main` dispatches on `argv[0]`; `agentdocker_host::program` names which program a process runs and how to start one, since macOS may report a hard-linked file under either name. The MCP server and hooks adapter talk to the daemon through the `Backend` trait in `client.rs`, whose test mock lets them be tested without a daemon.
- `crates/ui` — `agentdocker-ui`, the Iced desktop app with the tiny-skia software renderer. No HTTP; `agentdocker ui` launches it. See `docs/ICED-DESIGN.md` for interaction contracts and validation.

## Commands

Run an isolated daemon for manual testing: `AGENTDOCKER_HOME=/tmp/ad-test agentdocker daemon run` (or `agentd`, where the link is installed) and use the same env var with the CLI (or just run the CLI with that env var: a client that cannot connect starts the daemon itself; `AGENTDOCKER_NO_AUTOSTART=1` turns that off).

A machine that has never built this needs, before its first gate: Rust at the `rust-version` in `crates/ui/Cargo.toml` with its `rustfmt` and `clippy` components, Python 3.11 or later, on macOS the Xcode command-line tools, and `cargo-nextest` at the version `docs/TESTING-AND-BENCHMARKS.md` pins (that section owns every tool pin; coverage and fuzzing name theirs there). `make install` then builds and installs the app, the CLI and the daemon from the checkout (`docs/LOCAL-BUILD.md`). The board, the project's memory, who holds which role and any role a project added are in the daemon's home, not in the repository: a new machine's daemon starts without them.

## Conventions

- Core stays pure: if a change needs I/O, a clock, or async, it belongs in `agentd` or the CLI.
- `agentd` locks at most one mutex at a time and never across an `.await`. Keep it that way.
- Every daemon state change emits an `EventKind`. New behaviour = new event variant.
- Protocol changes: update `protocol.rs`, the table in `docs/ARCHITECTURE.md`, and the CLI in the same PR.
- Errors returned to clients use `ErrorCode`; add a variant rather than overloading `Internal`.
- When several agents share a checkout through AgentDocker, work in your own worktree (`agentdocker worktree-create --branch <name> [--from <ref>] <path>`) and hold `path:` on what you edit, released with a summary when done.
- Nothing personal belongs in the tree: no home paths, host or machine names, account details, session or agent ids from a real run, or trial evidence. Fixtures use made-up names.

## Documentation contract

Each document is the current contract for one thing: what the code does, how to use it, and what it will not do. The documents carry no state. No dates, versions under trial, delivery status, plans, audits, reviews or trial evidence, and nothing about a particular person, machine, host, project, agent or session. Open work lives on the project's board or issue tracker, and evidence from a trial on real binaries stays in its pull request. Examples use placeholders (`<project>`, `~/work/app`, `agent-1`), never names, paths or ids from a real run. A new document needs a reason a section in an existing one cannot serve.

Every change that alters behaviour or a contract updates the document that states it, in the same PR:

- `docs/ARCHITECTURE.md` for a protocol, event, error code, schema or semantic change (the request/response table, the events list).
- `docs/GUIDE.md`, `docs/DESKTOP-UX.md` or the root `README.md` when a command, tool or screen changes for the person using it.
- `docs/README.md` (the docs index): a new document is linked there.

`python3 scripts/docs_check.py` runs in `scripts/verify.sh check` and in CI. The docs index must list every document and every relative link must resolve. A change under `crates/`, `scripts/`, `packaging/`, `.github/`, `install.sh` or `Makefile` must come with a documentation change or with a commit whose message has a line starting `Docs:` saying why none is due (`Docs: unchanged, a rename with no behaviour change`). No document may carry a specific: a date, a pull request, issue or CI run number, a commit, a home directory or a real agent's name. Names private to one setup (a machine, a host, a project) are listed one per line in a file named by `AGENTDOCKER_SPECIFICS_DENYLIST`, kept outside the repository.

## Verification

Two local gates, both through `scripts/verify.sh`, which takes the machine's build slot itself:

- `bash scripts/verify.sh check` before a pull request's first push: the documentation checks, formatting, strict Clippy, nextest (zero retries), doctests and the installer tests, on the whole workspace.
- `bash scripts/verify.sh quick` before every later push to that pull request (a fix round, a merge of main): the same checks on the packages the change touches and the packages that depend on them (`scripts/touched_crates.py` names them from `cargo metadata`). CI is the full check for every pushed head, on Linux and macOS, with packaging and the release build in a job of their own; nobody waits for a second local full run.

While editing, run targeted tests (`cargo nextest run -p <crate> <filter>`), each worktree with its own Cargo target directory. `python3 scripts/build_storage.py` reports cache sizes, and `--prune --dry-run` names the inactive worktrees whose caches `--prune` would remove (no lease, branch merged, no build running; never the current worktree's). Run Cargo without a session's `AGENTDOCKER_AGENT_ID`, `AGENTDOCKER_SOCKET` or `AGENTDOCKER_HOME` in the environment: the hooks and service tests expect none (`verify.sh` drops them).

When several agents build on one machine, one build runs at a time under the exclusive lease `task:local-cargo-campaign`: `verify.sh` takes and releases it itself, and before a direct `cargo` run take it with `agentdocker claim task:local-cargo-campaign --wait <seconds> --note "…"` and release it as soon as the run ends (`agentdocker release <lease>`, the id `claim` printed; `agentdocker leases` shows it again). The slot is the machine's scarcest resource: keep in flight only as many branches as the machine can gate in turn without one waiting long behind another (one or two on a small machine), each taken from start to merge before the next starts, rather than many that queue for it.

`bash scripts/verify.sh package` runs crate packaging and the release build (for a packaging or release change); `coverage`, `bench` and `fuzz` run the other suites; see `docs/TESTING-AND-BENCHMARKS.md`. Release artifacts are built in CI only: a local release build carries the builder's paths.

## Review and merging

Roles are the person's to give (`agentdocker role --as <agent> <role>`, from the person's own terminal or the app): the lead hands out the board's cards and merges, the implementer writes the change and runs its gate, the reviewer reads the branch and gives the verdict. What each role does, command by command, is kept in the project's memory for that role's holders (`agentdocker memory profile`), not here. A reviewer is any agent holding the role; `agentdocker codex --name reviewer` starts a Codex that a message can wake.

The review is the reviewer agent's: it reads the branch at the head it is given, records its verdict on that commit (`agentdocker verdict <branch> --head <commit> --pass|--changes`) and sends its findings; the author posts each finding on the pull request as a review thread on its line, and answers it there. GitHub holds the code and runs CI as a check on main after the fact, not as a gate in front of a merge. A review round is for findings that must change the code before it merges (P1, P2); a smaller finding (P3, a nit, a wording, a bot's remark) goes on the pull request as a thread the author answers in the same push or with the follow-up card that tracks it, never as another round. Address valid findings, explain declined ones with evidence.

A pull request merges as soon as it is approved at its head (`agentdocker gate <PR>` answers `approved`: a pass counts only for the commit it names, so a later push reads `stale` until the reviewer passes the new head, while a request for changes stands across pushes and across another reviewer's pass until the reviewer who made it gives a later word; `agentdocker gate <PR> --publish` writes a summary of the answer to that commit's `agentdocker/review` status for whoever reads the pull request, and the verdict itself stays in the daemon that recorded it), its local gate was green on that head (`scripts/verify.sh check` for the first push, `quick` for each later one, a merge-only push included) and no review thread is open: `gh pr merge N --merge --match-head-commit <head>`, then main is installed. A thread is open when it is unresolved and on a line the head still has; one GitHub marks outdated no longer blocks (`python3 scripts/pr_threads.py N` lists the open ones and exits 1 while any remain). A push that moves a line does not answer what was found on it: before the reviewer passes a head, every P1 and P2 of its earlier rounds has the author's answer on its thread, outdated threads included (`--include-outdated` lists them), and a merge-only recheck does not lift that. Nothing waits for CI. Merge main into a branch only when it conflicts or when its base has moved enough to matter for the review; a merge-only push runs the short gate and asks the reviewer for a merge-only recheck.

CI runs on every push to main and to a pull request, for what a local gate cannot cover: the other platforms, the packaging, the release build, and the merged result of changes that each passed alone. A red run on main, a scheduled one included, is fixed forward at once, by a pull request under the same rule, before anything else merges; nothing reports a scheduled failure by itself, so the lead reads the latest run of each workflow on main at the start of a session and before each merge, the scheduled ones asked for by name (`gh run list --branch main --event schedule` beside `gh run list --branch main`, whose newest runs alone can hide an older scheduled failure). That rule is for the macOS and Linux runs: the Windows workflow's result holds no merge, and a red Windows run is not fixed forward. A pull request that conflicts with main gets no pull-request run until the conflict is gone. The macOS packages and CLI archives, whose runners are few, are built for main, for a `batch/<name>` branch and on demand; a `batch/<name>` branch (main with several approved pull requests merged in) is for wanting those packages for several changes at once, not a step any pull request waits for. A push to a pull request cancels its earlier runs, so push once per round, not per commit.
