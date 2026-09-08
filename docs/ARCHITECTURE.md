# AgentDocker architecture

This document describes implemented behavior and later design intent. The [September 6 audit](AUDIT-2026-09-06.md) records known exceptions and test evidence at a pinned revision; implementation does not imply release availability or completion of hardening.

The [product direction](PRODUCT-DIRECTION.md) sets the next delivery priorities: native local orchestration, automatic discovery and setup, an installed desktop GUI, and macOS/Linux/Windows support. Container engines are optional execution adapters. The historical phase order below does not make container expansion or a browser dashboard prerequisites for that desktop product.

## Goals

1. **Universal.** Any agent — any model, any vendor, any runtime — can participate with nothing more than the ability to write JSON to a socket. No SDK is required, though one may exist for convenience.
2. **Bounded coordination.** Claims expire, waiting is cancellable, and confirmed process/group exit releases protection. Uncertain writers retain protection according to the documented lifecycle and TTL rules. These are tested contracts, not a guarantee against arbitrary local processes.
3. **Familiar.** If you know Docker's mental model and CLI, you know AgentDocker's.
4. **Local first.** One host works with no network, no accounts, no cloud. Federation is layered on top later, not baked into the core.
5. **Observant.** The daemon derives working state from filesystem/process evidence and explicit hooks/MCP reports. Model/provider identity and task notes can be supplied by a launch specification or integration; discovery alone does not reveal them. Everything past Phase 1 is a derivation of that working set; see [The thesis](#the-thesis).

## Components

### `agentdocker-core` (`crates/core`)

Coordination types and state machines are pure; time-dependent operations accept `now: DateTime<Utc>`. The architectural target is no host I/O or environment lookup here. The current `paths` module still reads home/socket environment defaults; moving that host policy into `crates/host` is an audit follow-up. Core has no async runtime.

| Module | Contents |
|---|---|
| `agent` | `AgentId`, `AgentSpec` (the "image"), `AgentStatus`, `AgentRecord` (the "container") |
| `message` | `Envelope`, `Destination` (agent / topic / broadcast), `MessageId`, `topic_matches` |
| `lease` | `ResourceKey`, `LeaseMode`, `Lease`, `LeaseTable` — the claim/renew/release/expire state machine |
| `registry` | `Registry` — the agent table, name uniqueness, id/name/prefix resolution |
| `event` | `Event`, `EventKind` — everything the daemon announces |
| `protocol` | `Request`, `Response`, `ErrorCode` — the wire format |
| `project` | `ProjectRef`, `ProjectId`, `ProjectSource` — the project an agent works in and how it becomes an id |
| `paths` | Socket/data path calculations and legacy environment-default helpers |
| `runtime` | Curated tool inventory and supported setup formats |
| `working_set`, `change`, `journal` | Read marks, change records, journal entries and digest logic |
| `recovery`, `handoff` | Checkpoints, validation evidence and portable handoff records |
| `channel`, `contest` | Membership, reviews, evidence-backed entries and rankings |
| `wait`, `multiplexer` | FIFO/deadlock logic and reported session descriptors |

### `agentdocker-host` (`crates/host`)

Shared host I/O: project/path discovery, Git/content inspection, process identity, installed-runtime/config inventory, bounded subprocesses, terminal operations, notifications, multiplexer queries and optional container transports. It owns no daemon registry or durable coordination state.

### `agentd` (`crates/agentd`)

One process per host. It is a library crate whose `main` the `agentdocker` package wraps as the `agentd` binary, so a source install of the `agentdocker` package ships both CLI and daemon. It owns:

- **Registry** — in memory, guarded by a mutex.
- **Supervisor** — spawns managed agents with `tokio::process`, captures stdout/stderr to `<home>/logs/<id>.log` with timestamps and stream tags, and records the exit status.
- **Bus** — a `tokio::sync::broadcast` channel. Every message is published to it; each live subscription filters what it wants.
- **Inboxes** — per-agent queues for messages that arrive while the agent has no live subscription. Capped at 1000; oldest dropped first.
- **Lease table** — the core `LeaseTable`, plus a 1-second reaper that expires leases and emits events.
- **Events** — a second broadcast channel carrying `Event`s.
- **Projects** — the fingerprint cache per repository root; see [Projects](#projects).
- **Watcher** — one `notify` watcher over every checkout a live agent works in, feeding the ledger and branch refreshes; see [Watching and the ledger](#watching-and-the-ledger).
- **Store** — SQLite at `<home>/state.db`; see [Persistence](#persistence).

Locking discipline: one synchronous state mutex owns the registry, leases, inboxes, subscriptions, store and event sequence. A transition mutates memory, writes SQLite and publishes its events before releasing that guard. Host filesystem work and waits run outside the guard; no guard crosses an `.await`. This prevents older snapshots from overwriting newer state and keeps live event order aligned with persistence.

## Persistence

The intended write contract is to latch `storage_unavailable` on SQLite failure: the triggering request receives an error, subsequent coordination requests are refused, and events/messages are not published from the failed projection. `shutdown` remains available. Restart after repairing storage reloads the last durable state. This deliberately keeps the failed in-memory projection unavailable instead of trying to undo already-performed host effects. A multi-write operation can have committed a prefix before failing; clients must inspect/reconcile after restart rather than assume the whole request rolled back. A claim is never acknowledged after a detected write failure, and failed releases cannot admit a conflicting writer. Recovery IDs provide stronger idempotency where supported. Restore now commits its starting identity, required leases and `agent_restoring` events together before launch; failed preparation cannot start a writer. Native commands are now held before exec until their PID, exact process start identity, protection and lifecycle event commit. A failed completion closes the execution gate, so the command never starts. Native exit commits its status, lease deletion, journal entries and channel closure together; a failed write keeps their memory and durable projections unchanged. These fixes address the [September 6 audit](AUDIT-2026-09-06.md#blocking-findings); see [native delivery status](NATIVE-DELIVERY.md) for acceptance and release evidence.

Reads are served from memory; every mutation is written through to SQLite (`rusqlite`, bundled, WAL mode) before the response goes out. Rows are JSON blobs of the core types beside the few columns needed for lookups (`agents`, `leases`, `inbox`, `events`, `changes`, `journal` with its `journal_paths` and `journal_fts` indexes and `journal_cursors`; `projects` is the one plain table, a cache of fingerprints per repository root), so adding a field to a core type is not a migration. A `meta.schema_version` row guards against opening a database written by an incompatible build. Schema 8 upgrades schemas 1 through 7 on open and idempotently translates old `file:` leases using each holder's recorded checkout. Schema 8 makes a restore point durable launch intent, including an interrupted `created` restore; older daemons refuse it. Image-bound validation, runner deadlines, container lifetime and process-group tracking remain supported. Compatibility is checked before schema DDL or journal-mode changes. A downgrade requires the matching pre-upgrade state backup, not opening newer state with an older daemon.

On startup the daemon reloads agents, leases, and inboxes, tidying as it goes: a managed record still `created` without durable restore intent is recorded as failed, a second live record with an already-live name is recorded as exited, and a lease whose holder is not live is dropped — each written back so the store and the registry agree. Reloaded leases keep their original expiry. Opt-in snapshot restoration may acquire new leases from a recent restore point with new IDs and expiries; that is distinct from extending an existing lease.

Agents that were live when the previous daemon stopped are *adopted*: the new daemon has no `Child` handle for them, so a once-per-second liveness check inspects every unsupervised live agent that reported a pid and records an exit (releasing its leases) when the process is gone. "Gone" means signal 0 fails, *or* the process now behind that pid started at a different time than the one that registered — the daemon records the process start time (macOS `proc_pidinfo`, Linux `/proc/<pid>/stat`) so a recycled pid, typically after a reboot, is not mistaken for the agent. The same check covers externally registered agents, which is what makes a Claude Code session that dies without deregistering harmless. An external agent that registered without a pid can only leave by deregistering; a managed agent still being spawned is skipped.

Lease deletion and its `lease_released` or `lease_expired` event commit in one transaction, including startup cleanup, explicit release, agent exit and expiration. A failed event write retains the durable lease; restart never repeats an already committed cleanup event.

Persisted replay events carry a strictly increasing `seq`, continued across restarts. High-volume `file_changed` and `agent_stale` notifications instead use `seq: 0` and are live-only; retained ledger/read-set data supplies their recovery path. Events are appended to the store as they are emitted and trimmed to the newest 10,000 once a minute; `agentdocker events --replay N` shows the last N before streaming, and the server drops any live event whose `seq` the replay already covered, so an event emitted while the stream was being set up is delivered once. A persistence failure disables coordination and fails the request as described above; no failed event is published live. Inbox acknowledgment deletes and its event commit in one transaction.

### `agentdocker` (`crates/cli`)

A thin client. Each invocation opens one connection, sends one request, and prints the response(s). It exists so humans and shell hooks can participate; it is not the only way in.

### Starting the daemon

Nobody has to start `agentd` by hand. A client that cannot connect — no socket file, or nothing listening — starts the daemon itself, the way `ssh-agent` and `buildkitd` are started by their clients, then waits for the socket (3 s for the CLI and the MCP server, 1 s for the entire hook operation, which fails open past that). `AGENTDOCKER_NO_AUTOSTART=1` turns this off. The daemon it starts is the `agentd` beside the client's own binary when there is one, so a build in `target/` starts the matching daemon, else `agentd` on `PATH`; it runs in its own process group with stdout and stderr appended to `<home>/agentd.log`.

Exactly one daemon serves a socket, guaranteed by an advisory lock beside it (`agentd.sock` → `agentd.lock`). The daemon takes the lock for its lifetime before touching the socket, and exits at once, successfully, if it cannot. A client decides whether to spawn by taking the same lock for an instant: getting it means no daemon exists; not getting it means one is up or starting, so the client only waits. Two clients racing may both spawn a daemon, and the loser exits on the lock. The daemon's stale-socket check (remove the file if nothing answers on it) stays as a second line of defence.

**As a service.** On-demand start is enough for a laptop; `agentdocker daemon install` additionally runs `agentd` as a login service so it survives reboots and crashes and belongs to no terminal — a launchd agent (`~/Library/LaunchAgents/dev.agentdocker.agentd.plist`) on macOS, a systemd user unit (`~/.config/systemd/user/agentd.service`) on Linux. Both restart the daemon after a *failure* only, because a clean exit is what a service daemon does when an on-demand one already holds the lock; `install` therefore first asks any running daemon to exit (the `shutdown` request, which SIGTERMs managed agents exactly as Ctrl-C does) and then hands the socket to the service. `daemon uninstall`, `start`, `stop`, `restart`, and `status` do what they say, with `start` and `stop` falling back to the on-demand daemon when no service is installed; `--dry-run` on `install` and `uninstall` prints the files and commands instead. The service definition bakes in `--home` (and `--socket` when overridden) so it serves the same paths the CLI that installed it used. Files and command sequences are pure and unit-tested; only the final execution touches the system.

**Installing.** Installing the CLI package from a pinned Git tag/commit or checkout builds both binaries; `install.sh` at the repository root downloads the release archive for the host (`agentdocker-<target>.tar.gz`, four targets: macOS and Linux musl on x86_64 and aarch64, named without the version so `releases/latest/download/…` works) and drops them into `~/.local/bin`; `packaging/homebrew/agentdocker.rb.in` is the template for a tap formula, with a `brew services` block that runs the daemon. The release workflow builds and uploads archives with SHA-256 checksums on every protected `v*` tag, then generates `agentdocker.rb` from all four verified checksum inputs. The installer requires a valid matching checksum before extracting or replacing anything. Workspace dependencies include versions so `cargo package --workspace` packages all five crates; actual crates.io publication and tap publication remain release operations.

### `agentdocker mcp` (`crates/cli/src/mcp.rs`)

The universal adapter. An MCP host spawns `agentdocker mcp` as a stdio server; the server registers the host as an agent (pid = the host's, so the liveness check cleans up after a crash) and translates tool calls into daemon requests. The JSON-RPC surface is deliberately minimal — `initialize`, `ping`, `tools/list`, `tools/call`, notifications ignored — and hand-rolled, because that is a few hundred lines with tests versus a dependency on a fast-moving SDK. Supported protocol versions: `2025-06-18`, `2025-03-26`, `2024-11-05` (the client's choice is echoed when supported).

Design points:

- **Identity.** If `AGENTDOCKER_AGENT_ID` is set the host was started by `agentdocker run` and already *is* an agent; the server adopts that identity and does not deregister on exit. Otherwise it registers a new agent and deregisters when stdin closes.
- **Conflicts are results, not errors.** `claim` returns `{"claimed": false, "held_by": [...]}` with `isError: false`, because a conflict is information the model must reason about, whereas `isError: true` reads to most hosts as "the tool broke".
- **`wait_for_messages` polls the inbox** rather than holding a live subscription. A live subscription would mark the agent as online, so a message arriving between the server's last read and the socket closing would be pushed to a reader that is gone. Polling at 250 ms trades a little latency for zero loss.
- The `instructions` field returned from `initialize` tells the model when to claim, release, and read its inbox, so hosts that surface instructions need no extra prompting.

### `agentdocker hook` (`crates/cli/src/hooks.rs`)

Where the MCP server offers tools the model *may* call, hooks make coordination unconditional. `agentdocker hook install claude-code` merges six entries into Claude Code's `settings.json` (idempotently — entries whose command already runs `hook claude-code` are left alone), each running `agentdocker hook claude-code`, which reads the event JSON from stdin:

| event | daemon calls | output |
|---|---|---|
| `SessionStart` | register (or reuse) `claude-<session id prefix>`; list agents; peek inbox, flush output, then acknowledge delivered IDs | `additionalContext`: who else is live, how to talk to them, queued messages |
| `UserPromptSubmit`, `PostToolUse` | peek inbox, flush output, then acknowledge delivered IDs | `additionalContext` with the messages, or nothing |
| `PreToolUse` (Edit/Write/MultiEdit/NotebookEdit) | claim `path:<absolute file>` exclusive, 600 s, note "editing in Claude Code session …" | on conflict `permissionDecision: deny` with the holder and their note; otherwise nothing, so the user's own permission rules still apply |
| `Stop` | release all; unless `stop_hook_active` or `--no-wake`, peek inbox, flush output, then acknowledge delivered IDs | `decision: block` with the messages when any are waiting, so the model handles them before finishing |
| `SessionEnd` | release all; deregister | nothing |

Design points:

- **Identity is by name.** Hooks are separate processes with no shared state, so the agent is found by name (`claude-` + the first eight characters of the session id) via the daemon's name resolution; `AGENTDOCKER_AGENT_ID` wins when the session was started by `agentdocker run`. A hook that fires for an unregistered session (hooks installed mid-session) registers it on the spot.
- **The host pid, not the hook's.** Claude Code runs hooks under a shell, so the hook walks up the process tree past shells to find the host and registers that pid. The liveness check then cleans up a session that dies without `SessionEnd`.
- **Fail open.** Any daemon error is written to stderr and the hook exits 0 with no output, so an unreachable `agentd` never blocks an edit or a stop.
- **`Stop` is the delivery guarantee for chatty agents.** A session that never calls a tool again would otherwise finish without seeing replies; blocking the stop once (never when `stop_hook_active`) turns "you have messages" into the model's next instruction.

### `Agentfile.toml` and `agentdocker up` / `down` (`crates/cli/src/agentfile.rs`, `teams.rs`)

A TOML file with an optional `name` and an `[agents.<name>]` table per agent (`runtime`, `provider`, `model`, `command`, `workdir`, `env`, `labels`; unknown keys are rejected). `up` turns each entry into a `run` request in file order, skipping names that are already live, and labels every agent with `agentfile=<path>` and `team=<name>`; `down` stops the live agents named in the file. Relative `workdir`s resolve against the file's directory. There is deliberately no daemon-side notion of a team yet: the file is a client convenience over `run`/`stop`, so a team can also be assembled by hand or by another tool.

## Projects

Agents are grouped by the project they work in, and the project is **derived, never declared**: the daemon computes it from `spec.workdir` when an agent is created (`run` and `register` both default the working directory to the caller's; hooks and the MCP server record theirs) and stores it on the record as `project`. Nothing ever asks an agent which project it belongs to.

**Derivation** (`agentdocker_host::project::discover`): canonicalise the working directory, then walk up to the nearest ancestor holding `.git`. A `.git` *file* is a linked worktree or a submodule: a worktree's `gitdir` names a git directory whose `commondir` points at the main repository, so every worktree of one repository resolves to the same root while keeping its own path in `worktree`; a submodule has no `commondir` and is its own project. With no repository, the nearest ancestor holding an `Agentfile.toml` is the root, and failing that the working directory is its own project (`source: directory`).

**Identity.** A `ProjectRef` carries `root`, `worktree`, `source`, and for repositories a `fingerprint`: the lexicographically smallest root commit of `HEAD` (`git rev-list --max-parents=0 HEAD`; smallest so merged unrelated histories are stable). The `ProjectId` is the fingerprint when there is one, else a UUIDv5 of the root path — the same repository is one project across clones and, later, across hosts, and a plain directory is still one project for everyone in it. The fingerprint walks the whole history, so the daemon runs it once per root, in a blocking task with a 3-second timeout, and caches the result in the `projects` table; a lookup that fails (no `git`, no commits, timeout) is remembered in memory only, so every agent in that repository still shares a path-derived id this run and a restart retries. `project_discovered` fires the first time a repository is fingerprinted on this host.

**Discovery.** Agents that never register are still worth seeing. `discover` reads the process table once (`ps -axo pid=,ppid=,args=`, portable across macOS and Linux and complete enough to recognise `node …/@anthropic-ai/claude-code/cli.js` as well as a native `claude`), keeps the rows whose command line matches the known-runtime table in `agentdocker_host::procinfo` (`claude-code`, `codex`, `gemini-cli`, `cursor`, `aider`, `goose`, `copilot`, `amp`, `opencode`), drops pids that live agents already claim, and reads each survivor's working directory (`proc_pidinfo` on macOS, `/proc/<pid>/cwd` on Linux) to place it in a project — without a fingerprint, because this runs on every `ps` and a process nobody adopted should neither warm the cache nor announce a repository. `ps` appends them, dimmed on a terminal and plain in a pipe, under the name `adopt` would give them (`<runtime>-<pid>`) with status `unadopted`, and says so on stderr; `--no-discover` skips it. `adopt <pid>` registers the process with the runtime from the table (overridable), the working directory from the process, the pid for liveness, and the label `adopted=true`. An adopted agent runs no hooks, so it holds no leases and reports nothing, but it is visible, messageable — its inbox fills until something drains it — and counted in its project. It is a heuristic on-ramp and is presented as one.

**Branch and head.** Every agent with a working directory carries `vcs`: the branch (or none, detached), the commit HEAD points at (or none, unborn), and when it was observed. It is read from `.git` directly — `HEAD` and one ref file, packed refs as the fallback, the worktree's own git directory for a linked worktree — without a `git` process (`agentdocker_host::vcs`), using bounded regular-file reads and validated ref paths: the daemon reads it when an agent is created and again for every live agent every five seconds, which covers adopted agents and anything started with `run`; the Claude Code hooks additionally send it with `report` on `SessionStart`, `UserPromptSubmit`, and `PostToolUse`, so a `git checkout` run through the Bash tool shows up at once. Older observations are ignored. A real change is persisted and announced (`agent_vcs_changed`) in one serialized state transition; a fresh timestamp alone does not emit an event. `ps` shows `BRANCH` and `HEAD` so "are we even looking at the same code" is answered at a glance. Dirtiness stays unknown until something cheap can tell.

**What it gives you.** `ps` shows a `PROJECT` column (`repo`, or `repo@wt` inside a linked worktree) and sorts by project; `ps --project .` (any path inside the project) or `--project <id prefix>` filters, as does `-l key=value`; `BRANCH` and `HEAD` say what each agent's checkout is on; `list {project?, labels?}` is the request behind both. `send --to project` reaches everyone else working in the same project, with inbox fallback like broadcast, and a session's `SessionStart` orientation names the agents in its project before any others. `inspect` shows the full reference. Write leases use canonical physical paths (see [Leases](#leases)); `leases --resource <root>` lists protection in that physical checkout. Logical project-relative paths remain the basis for cross-checkout change and overlap analysis.

## Watching and the ledger

The daemon watches the filesystem of every checkout a live agent works in and keeps a ledger of what changed and who held it. This is the substrate for staleness notices and the change journal (Phase 3), and it is what makes branch tracking event-driven.

**Watching.** One `notify` watcher (FSEvents on macOS, inotify on Linux) covers each distinct checkout — the main root or a linked worktree — of every live agent whose project is a repository or an `Agentfile.toml` root. Plain directories are not watched: a recursive watch on a home directory is exactly what inotify cannot afford. Watches are reconciled against the registry once a second and on registration. Normal `run` waits up to 500 ms for checkout coverage before spawning; failed attachment prevents launch. `register` waits before reporting successful coverage, but an externally started process may already be writing. Snapshot restore uses the same coverage barrier, with the host listener bound and serving concurrently before restored commands run. An agent leaving needs no hook. Raw events are debounced for 100 ms and duplicates within a batch collapse; each path is filtered through the checkout's `.gitignore` so `target/` and `node_modules/` never reach the ledger; directories are skipped; and `.git/` is ignored except the files that say where HEAD is (`HEAD`, `refs/heads/**`, `packed-refs`, a worktree's `HEAD`), which trigger a branch re-read for the agents in that checkout instead of an entry. A linked worktree's own git directory, which lives under the main root, is watched too so its `HEAD` is seen.

**The ledger.** Each surviving change becomes a `Change`: project, worktree, checkout-relative path, kind (created, modified, removed, renamed), time, the checkout's HEAD, and an **attribution** — the holder of an unexpired exclusive lease on the physical checkout path (shared leases are not authorship evidence), else `external`: the user's editor, a git command, a build. Attribution is best-effort by construction and every rendering says so. Entries are persisted in the `changes` table (`seq`, indexed by project and by project + path, so "everything under `src/`" is a prefix range) and announced live as `file_changed`, which is deliberately *not* kept in the event history: change volume would crowd out everything else in that 10,000-event window. The newest 100,000 entries per project are kept, pruned once a minute.

**Reading it.** `changes {project, since_seq?, path?, agent?, limit?}` returns the newest `limit` entries oldest first; `agentdocker changes [--project .] [--since N] [--path P] [--agent A] [-n N]` prints them with agent names, and `agentdocker blame <path>` is the same query for one file. An absolute path in a query is made relative to the checkout containing it, so callers need not know the root.

## The journal

Where the ledger records every file change, the journal records *what happened and why*, one line at a time, per project: coarse, readable by models and humans, and what a newcomer is handed instead of the event stream. Entries, storage, the release write path, cursors, digests, and the adapters exist.

**Entries.** A `JournalEntry` carries project and per-project `seq`, time, the agent (or none, for a commit nobody is known to have made), its name and branch, the physical checkout and worktree, a kind, a summary with its source (`explicit`, `transcript`, `synthesised`), the released resources, up to 200 checkout-relative paths with the real count, HEAD before and after, and the ledger seq range for drill-down. Kinds: `release` (leases dropped; the entry says what changed under them), `note` (free text), `commit` (HEAD moved), `join`, `leave`, and `handoff` (Phase 4).

**Write path.** A `release` or `release_all` that freed at least one lease passes the *release barrier* when a released lease protects a path — the daemon asks the watcher to record whatever it is still debouncing, bounded at 500 ms including queue admission, so a change made a moment before the release is in the entry — then, for each released `path:` resource, runs one indexed prefix-range query on the ledger bounded by the lease's durable `change_seq` (numeric `acquired_at` fallback for legacy leases), restricted to the same physical checkout; unions, sorts, dedupes, and caps the paths; takes the explicit summary or synthesises one ("edited 3 files under src/: parser.rs, lexer.rs, mod.rs"); and writes the entry, its path index rows, and its search row **in the same transaction as the lease deletions and replay events**, so a crash can leave neither a released lease without its entry nor an entry for a lease still held. An explicit nonempty summary is journaled even when no lease was held. A release under which nothing changed and nothing was said writes no entry. Agents joining and leaving a project are journaled; so is a checkout's HEAD moving — once per checkout and HEAD however many agents share it, named through `git log`, attributed to the only agent in the checkout, else the holder of the `branch:` lease, else nobody. `journal_add` appends a note.

**Storage.** `journal` (rowid `id`, `project`, `seq`, `at`, `agent`, `branch`, `kind`, the JSON blob; unique on project + seq; indexed by project + branch + seq and project + agent + seq), `journal_paths` (project, path, seq — the index behind `--path`, a prefix range), `journal_fts` (FTS5, contentless, rowid = `journal.id`; when the SQLite build lacks FTS5 the daemon logs it once and `--grep` falls back to `LIKE`), and `journal_cursors`, created for row 9b. Entries are kept forever; `journal prune --before <seq>` deletes on demand, from all three tables together. The daemon keeps a ring of the newest 256 entries per active project, loaded on first use and dropped ten minutes after the project's last live agent leaves; a plain listing whose window lies in the ring never touches SQLite.

**Reading it.** `journal {project, since_seq?, until_seq?, agent?, branch?, kind?, path?, grep?, limit?}` returns the newest `limit` entries oldest first with the resolved project id; `agentdocker journal [--project .] [--since N] [--until N] [--agent A] [--branch B] [--kind K] [--path P] [--grep TEXT] [-n N] [--follow]` prints them, `journal add "…"` appends a note, `journal prune --before N` trims. `release --summary "…"` (and the MCP `release` tool's `summary`) is how an agent says what it did; the MCP `journal_note` tool is how it leaves a note. Every append is announced as `journal_appended`, which `--follow` streams (the stream is opened before the snapshot it continues, so nothing falls between them, and the listing's filters apply to streamed entries too).

**Reading it incrementally.** Every reader has a cursor per project — `journal_cursors (agent, project, seq, updated_at)`, cached in the daemon and written through only when it moves forward — recording the last entry it was shown. A registration seeds the newcomer's cursor: from a finished agent of the same name in the same project that left within seven days (a resumed Claude Code session keeps its `claude-<prefix>` name, so it continues where it left off), else at the newer of "24 hours ago" and "20 entries back". The human reads as `user`. A `journal` request with `digest: {reader, max_entries, max_chars, all_branches?, advance?}` answers `digest {text, head_seq, shown, collapsed, other_branches}` instead of a listing: entries after the reader's cursor (or after `since_seq` when given), the reader's own branch verbatim plus `join`/`leave`/`commit`/`handoff` from every branch, the newest within the budget rendered one per line, older ones folded into a leading "… N earlier entries" line, other branches into a trailing count; the reader's own `join`/`leave` lines are not news and are skipped. `advance` moves the cursor to `head_seq` when text was produced — everything the filter hid counts as seen too. An empty `project` means the reader's own. Served from the ring whenever the cursor lies inside it; otherwise the newest 1,000 entries after the cursor are read from the store. Every move is announced as `journal_read`. `agentdocker journal --new [--ack] [--all-branches] [--as AGENT]` prints the human's (or an agent's) digest.

**Adapters.** The Claude Code hooks hand the digest over as `additionalContext`: `SessionStart` with up to 20 entries or 2,000 characters (`--digest-entries`, `--digest-chars`), `UserPromptSubmit` only what is new and at most 5 entries or 500 characters (`--prompt-digest-entries`, `--prompt-digest-chars`), and nothing when nothing is new; `PostToolUse` never carries journal text. `Stop` reads the last 64 KB of the session transcript, takes the last assistant message with text — fenced code and headings dropped, markdown stripped, first paragraph, trimmed to 280 characters at a word boundary — and sends it as the `release_all` summary with `summary_source: transcript`; a transcript summary only ever describes leases actually released, whereas an explicit `--summary` is journaled even when nothing was held. The MCP `read_journal {since?, all_branches?}` tool returns the digest and advances the cursor.

## Wire protocol

Transport: newline-delimited JSON over a Unix domain socket at `$AGENTDOCKER_SOCKET` (default `~/.agentdocker/agentd.sock`, mode `0600`). A socket name is limited by the kernel (104 bytes on macOS and the BSDs, 108 on Linux), so a home whose path leaves no room for `container.sock` keeps both sockets in a private directory (`0700`, ours alone, ownership checked before binding) under `/tmp` — never an environment-dependent directory, which a service, a cron job and a shell can each see differently — named `agentdocker-<hash of the home's bytes>`; the home is canonicalized once (`agentdocker_host::dirs::home`) so a symlinked path spells the same directory everywhere; the daemon and every client compute the same place without a pointer file, an installed service is pinned to the resolved path with `--socket` and its commands use that socket, and `agentdocker daemon status` prints both paths. A path that still does not fit is refused up front by both daemon and client, naming the limit. The restricted container endpoint is optional: it announces `restricted_endpoint_listening` when it serves; if it cannot be served the daemon announces `restricted_endpoint_unavailable`, `ping` stops reporting it, `grant-access` answers `unavailable`, and the host socket carries on. A client that starts the daemon on demand watches the child it spawned, so a daemon that dies on startup fails the command at once with the log's last lines rather than after the start timeout. One request object per line, tagged by `"op"`; responses tagged by `"type"`.

```json
{"op":"claim","agent":"writer","resource":"path:/repo/src","mode":"exclusive","ttl_secs":300,"note":"refactoring"}
{"type":"lease","lease":{"id":"3f1c...","resource":"path:/repo/src","holder":"9a2b...","mode":"exclusive","acquired_at":"...","change_seq":42,"expires_at":"...","note":"refactoring"}}
```

| Request | Response | Notes |
|---|---|---|
| `worktree_create {agent, path, branch}` | `worktree {path, branch}` | host-only; new linked checkout at HEAD |
| `worktree_diff {agent}` | `diff {text}` | host-only tracked diff |
| `commit {agent, message, all?, push?}` | `committed {head, branch?, files, pushed}` | host-only; commits the agent's checkout and journals it against that agent. `all` stages tracked modifications first; `push` pushes afterwards, and a push that fails answers `error(unavailable)` with the commit's sha, since the commit was made. Nothing is written into the commit itself — the git author is unchanged and no trailer is added |
| `integrate {agent, source, validation, apply?}` | `integration {source_head, applied, clean, text}` | validated source; apply leaves merge uncommitted and target lease held |
| `grant_access {agent, container_root, ttl_secs?}` | `access {grant, token, socket, expires_at}` | host-only; TTL 1–86400 seconds, default 3600; CLI writes token privately and prints grant ID |
| `revoke_access {grant}` | `ok` | host-only; deny new requests, preserve leases |
| `authenticate {token}` | `ok` | restricted endpoint only; precedes one scoped request |
| `ping` | `pong` | version, uptime, restricted endpoint while serving |
| `build_image {spec: {engine, connection?, context, recipe, timeout_secs?}}` | `image_build {build}` | host-only Docker/Podman build from captured inputs; timeout defaults to 600 seconds, valid range 1–3600; immutable image ID and atomic provenance/event |
| `images` | `image_builds {builds}` | retained build evidence, including after restart |
| `run {spec}` | `agent` | spawns `spec.command`; child gets `AGENTDOCKER_SOCKET`, `AGENTDOCKER_AGENT_ID`, `AGENTDOCKER_AGENT_NAME`; `spec.restart` starts it again after it exits (`no` by default, cleared by `stop`); `spec.restore` brings it back under the same id after a daemon restart; `spec.in_pane` starts it in a new `tmux` session and registers it instead, so tmux owns the process and there is no captured log — it requires `spec.workdir` (tmux needs a directory to start in) and tmux 3.2 or newer (`new-session -e`, which is how the agent is told its own id), and is refused with `run_container` |
| `run_container {spec, build, options?}` | `agent` | host-only; retained image with durable identity/intent; opt-in checkout/scoped endpoint mounts, Podman VM transport, and bridge networking |
| `restart_container {agent}` | `agent` | host-only; new identity from same build after confirmed exit; `conflict` while exit is uncertain |
| `register {spec, pid?, session?}` | `agent` | external process; PID must be positive and fit i32; `spec.workdir` decides the project; `session` is the multiplexer the client can see it is in, read first-hand from its own environment |
| `deregister {agent}` | `agent` | marks an external agent exited |
| `discover` | `processes` | running processes of known agent runtimes that no live agent claims by pid, from the daemon's last scan (every five seconds; a healthy scan older than four seconds is redone; failed scans return `unavailable` and retain the previous snapshot) |
| `runtimes` | `runtimes {runtimes: RuntimeInfo[]}` | the agent tools on this machine — CLI and version, desktop apps, config directory, whether the MCP server and hooks are wired in — with the unregistered running processes of each |
| `adopt {pid, name?, runtime?}` | `agent` | registers such a process; `invalid` if a live agent already has the pid |
| `stop {agent, force?}` | `agent` | validated SIGTERM/SIGKILL; returns `stopping` until observed exit, retaining leases |
| `remove {agent}` | `ok` | forget a finished agent |
| `list {all?, project?, labels?}` | `agents` | live only unless `all`; `project` is an id prefix or an absolute path inside it; `labels` must all match |
| `inspect {agent}` | `agent` | |
| `heartbeat {agent}` | `ok` | bumps `last_seen` |
| `report {agent, vcs?}` | `ok` | what an adapter observed; a changed `vcs` is stored and announced |
| `observe {agent, paths}` | `reads {reads: ReadMark[]}` | capture content immediately before reading |
| `reads {agent}` | `reads {reads: ReadMark[]}` | durable observations |
| `stale {agent, paths?}` | `stale {stale: StalePath[]}` | compare current content; querying never clears staleness |
| `checkpoint {agent,key,task,assumptions?,next_steps?,release_leases?}` | `checkpoint` | persist context before optional release; retries are idempotent |
| `resume {agent,checkpoint,acknowledge?}` | `recovery` | verify and optionally accept a same-checkout handoff; accepting a bundle also moves its leases to the recipient when the sender asked, seeds the recipient's read set, and sets its journal cursor to the sender's |
| `checkpoints {agent?}` | `checkpoints` | list durable checkpoints |
| `handoff {agent, to?, task?, note?, transfer_leases?, key?}` | `handoff {bundle}` | a checkpoint addressed to `to` with the sender's state bundled around it, announced to `to` as a `handoff` message; leases are released unless they are to move at acceptance; without `to` the bundle is an export; retries with the same key return the same bundle |
| `handoffs {agent?}` | `handoffs {bundles}` | bundles sent by or addressed to the agent, oldest first; all of them without one |
| `import {agent, bundle}` | `handoff {bundle}` | a bundle exported on another host, re-homed to the agent's checkout and addressed to it, to accept with `resume` |
| `validate {agent,command,timeout_secs?}` | `validation` | execute and retain code-specific evidence |
| `validations {agent}` | `validations` | evidence for one session |
| `attach {agent, cols?, rows?}` | `events_ready`, then a stream of `output {data}` | connects to a managed agent's terminal; the client then sends `attach_input` and `attach_resize` on the same connection, and closing it detaches without disturbing the agent |
| `attach_input {data}` | — | keystrokes for an attached terminal, base64 because they are bytes |
| `attach_resize {cols, rows}` | — | the attached window changed size, so the agent gets `SIGWINCH` |
| `channels {project?, all?, agent?}` | `channels {channels: Channel[]}` | the rooms agents share in a project; open ones by default, `all` includes closed-but-unpruned, `agent` narrows to its own and lets `project` be empty |
| `channel_open {agent, task, members?}` | `channel {channel}` | a room for a task; members default to every other live agent in the project |
| `channel_close {agent, channel, resolution?}` | `channel {channel}` | the work is final; members are told and the journal says what it settled on |
| `channel_prune {project?, before_secs?}` | `pruned {removed}` | forget channels closed longer ago than that (a fortnight by default); an empty `project` prunes every project |
| `review_request {agent, channel, note?}` | `channel {channel}` | ask the other members to look at this agent's work |
| `review {agent, channel, of?, verdict, note?}` | `channel {channel}` | `approve`, `changes` or `comment` on another member's work; `of` defaults to the only other member; the reply carries the reviews so far |
| `overlap {project, since_seq?, agent?}` | `overlap {overlaps: Overlap[]}` | paths changed in more than one physical checkout of the project, from the newest 50,000 ledger rows: per path, each checkout with the agents attributed there, the count, the last change and its HEAD; with `agent`, only overlaps involving its checkout, and an empty `project` means its own |
| `changes {project, since_seq?, path?, agent?, limit?}` | `changes {changes: Change[]}` | the ledger, newest `limit` entries oldest first; `since_seq` is exclusive (`seq > since_seq`); `limit` defaults to 50 and is clamped to 1–10,000; empty, `.` and absolute checkout-root paths select all paths |
| `shutdown` | `ok` | the daemon exits after replying; managed agents get SIGTERM, as on Ctrl-C |
| `reload` | `unavailable` error | currently refuses replacement without changing the daemon or agents; safe live transfer remains unfinished |
| `send {from, to, kind, payload, reply_to?}` | `sent` | `to` is an agent ref, `project:<id prefix or absolute path>`, `topic:<name>`, or `all` |
| `subscribe {agent?, topics?}` | stream of `message` or `lagged {skipped: u64}` | flushes the inbox first, then live until the client disconnects |
| `inbox {agent, drain?}` | `messages` | |
| `ack_inbox {agent, messages: MessageId[]}` | `ok` | idempotently acknowledge specific delivered messages; emits `inbox_acknowledged` |
| `me {workdir?}` | `agent` | register the person at the keyboard as the agent `user`, runtime `human`, or return the one already registered; `workdir` moves them to that project. No pid, so liveness never expires it |
| `ask {from, to, question, timeout_secs?}` | `answer {message, from, text}` or `error(timeout)` | sends a `question` message and holds the connection until an answer names it; timeout defaults to 300 s and is clamped to 1–86,400 |
| `answer {from?, message, text}` | `sent` | reply to a waiting question by its id; who to reply to comes from the question, not the caller; `from` defaults to `user` |
| `questions {agent?}` | `questions {questions: Question[]}` | what is still waiting, newest first; `agent` narrows to the ones put to that agent |
| `claim {agent, resource, mode?, amount?, ttl_secs?, note?, wait_secs?}` | `lease`, `error(conflict)`, `error(deadlock)` or `error(forbidden)` | `amount` spends a `quota:` resource and is ignored for every other kind; policy is checked once, before the first attempt | `path:` uses canonical physical absolute keys; `file:` is a validated checkout alias; conflict `details.held_by` lists the blocking leases; `wait_secs` (max 600) queues in arrival order and retries when it is this waiter's turn; a wait that would close a cycle is refused at once with `details.cycle` |
| `activity {agent?, project?, all?}` | `activity {activity: AgentActivity[]}` | what each agent is doing — `starting`, `working`, `blocked {resource, held_by, since}`, `idle`, `finished` — blocked first; derived from the wait queue and last contact, never from terminal output |
| `waiting` | `waiting {waiting: Waiter[]}` | the claim queue, oldest first |
| `contest_open {agent, project?, task, metric, entrants?, channel?}` | `contest {contest, standing}` | announces a task and fixes the measure; opens a channel for the entrants unless told not to |
| `contest_enter {agent, contest}` | `contest {contest, standing}` | join an open contest; a latecomer is admitted to its channel too |
| `contest_submit {agent, contest, validation, score?}` | `contest {contest, standing}` | the validation must be the submitter's own and must have passed; a `validation_seconds` contest ignores `score` and uses the daemon's own timing |
| `contests {contest?, project?, agent?, all?}` | `contests {contests: Contest[]}` | newest first; open ones unless `all`; one `contest` is a lookup by id that ignores the other filters and answers `not_found` rather than an empty list |
| `contest_close {agent, contest, winner?, resolution?}` | `contest {contest, standing}` | opener or entrant only; without `winner` the ranking decides, and refuses with `conflict` when the entries are inside the noise floor |
| `renew {agent, lease, ttl_secs?}` | `lease` | responses may include `change_seq`, the durable acquisition boundary; absent on legacy leases |
| `release {agent, lease, summary?, summary_source?}` | `lease` | holder only; `summary` becomes the journal entry's text; `summary_source` is `explicit` (default) or `transcript` |
| `release_all {agent, summary?, summary_source?}` | `leases` | every lease the agent holds; the reply lists them |
| `journal_add {agent, summary}` | `journal_entry` | a note in the agent's project journal |
| `journal {project, since_seq?, until_seq?, agent?, branch?, kind?, path?, grep?, limit?, digest?}` | `journal` or `digest` | newest `limit` entries oldest first, with the project id and durable `head_seq` read under the same state lock (including pruned entries; absent on older daemons); with `digest {reader, max_entries, max_chars, all_branches?, advance?}` the reader's digest since its cursor instead (empty `project` = the reader's own) |
| `journal_prune {project, before_seq}` | `pruned` | drops entries below `before_seq` |
| `leases {agent?, resource?}` | `leases` | `resource` filter uses overlap, not equality; `file:` inputs resolve through the same physical checkout alias |
| `events {replay?,ready?}` | optional `events_ready`, then stream of `event` or `lagged {skipped: u64}` | replays the last `replay` stored events, then live until the client disconnects |
| `logs {agent, follow?, tail?}` | stream of `log`, then `end` | containers: verified engine snapshot, max 10,000 lines/4 MiB, no follow |

Any agent reference (`agent`, `from`, `to`) accepts a full id, a unique id prefix, or a name. Names resolve to the live agent with that name, or failing that to the most recently created finished one (so `logs` works after exit).

Errors: `{"type":"error","code":"conflict|deadlock|not_found|ambiguous|name_taken|forbidden|invalid|storage_unavailable|engine_unavailable|build_failed|unavailable|timeout|internal","message":"...","details":{...}?}`.

## Leases

A **resource key** is `kind:value`. The daemon interprets two kinds, `path` and `file`, which overlap hierarchically — `path:/repo/src` overlaps `path:/repo/src/lib.rs` and `path:/repo` — so claiming a directory protects everything under it. Every other kind (`branch:`, `task:`, `db:`, or anything you invent) overlaps only on exact match.

**Physical protection and logical overlap.** Write leases use canonical absolute `path:` keys independently of the holder's project. A directory claim covers its physical descendants, including files claimed by agents outside the project. Canonicalization resolves existing symlinks and normalizes missing suffixes, including `..`. An explicit `file:<project id>/<relative path>` input is an alias only when `agent` identifies a matching checkout; unsafe relative paths are rejected. Queries use the same normalization. Linked worktrees and clones can edit independently because they are different physical checkouts. Project id plus relative path describes logical overlap for the Phase 3 ledger and Phase 4 integration; it is not a second write-lock namespace. Containers use an authenticated mount mapping before their paths participate.

Two **modes**: `exclusive` conflicts with any lease on an overlapping resource held by someone else; `shared` conflicts only with exclusive leases held by someone else. An agent never conflicts with itself, and re-claiming a resource you hold in the same mode renews it instead of failing.

**TTL.** Every lease expires. The default is 300 s, the cap is 24 h. Long-running work should `renew` periodically rather than ask for a long TTL: a TTL is a liveness bound, not a reservation. A reaper runs every second; `leases` also expires before listing so its output is never stale.

**Exit.** Process-backed `stop` validates the PID and recorded process identity, sends the signal and records `stopping`. It retains leases until the supervisor or liveness check observes exit; force stop follows the same observation rule. New claims/renewals require `running`. An external agent may explicitly deregister; managed agents finish through supervision and cannot deregister themselves. PID zero and values beyond the positive signed PID range are rejected. Exit releases held leases once.

**Conflicts are informative.** A refused claim returns every blocking lease including its holder, mode, expiry, and note, and emits a `lease_conflict` event. Agents are expected to read the note, message the holder, or wait.

**Waiting.** `claim` with `wait_secs > 0` subscribes to the event stream *before* its first attempt, and on conflict takes a place in the wait queue and sleeps until an overlapping lease clears, a waiter ahead of it leaves, or the deadline passes. One `lease_conflict` event is emitted per request no matter how long it waits. Waiters are served in arrival order — a waiter attempts only when no older waiter wants something overlapping that would exclude it — so a newcomer cannot starve one already waiting; two shared waiters do not block each other. Closing a waiting connection cancels its request and gives up its place, so a vanished client never holds the head of a queue. A wait that would close a cycle is refused immediately with `error(deadlock)`. Liveness is checked under the state lock before every acquisition. Claim and renew expiration effects are persisted and announced using the same timestamp as the core operation.

## Messaging

An **envelope** carries `from` (an agent id, or `user` for CLI-injected messages), `to`, a free-form `kind`, a JSON `payload`, an optional `reply_to`, and a timestamp. Most payloads are opaque; built-in question/answer, handoff and notification paths interpret their documented fields.

Five destinations:

- **Agent** — one recipient, resolved by id/prefix/name before publishing.
- **Project** — every live agent in a project except the sender. `project:<selector>` takes an id (any unique prefix) or an absolute path inside the project; the CLI and MCP server turn a bare `project` into the caller's current directory, so `send --to project` needs no ids at all.
- **Topic** — a `/`-separated path like `repo/backend/reviews`. Subscribers give MQTT-style patterns: `+` matches one level, `#` matches the rest.
- **Broadcast** — every live agent except the sender.
- **Channel** — members of a named channel except the sender, with inbox fallback when a member has no live subscription.

**Delivery.** A message is pushed to every live subscription whose filter matches (a project delivery matches subscribers whose agent was in that project when it subscribed). For agent, project, channel, and broadcast destinations, each recipient *without* a live subscription gets the message queued in its inbox instead. Topic messages are live-only; whether they should ever queue is an [open question](#open-questions). When an agent opens a subscription its inbox is flushed into the stream first; a message that lands in the tiny window between "subscribed to the bus" and "inbox drained" is suppressed by id so it is not shown twice.

Destructive inbox reads and subscription startup commit removal of the queued message IDs and one `InboxAcknowledged` event in the same SQLite transaction before changing memory or live delivery routing. Failed removal/event persistence returns `storage_unavailable`, retains queued messages and does not register a new subscriber. That event records server-side queue removal; it does not prove the provider consumed the message.

Live delivery is at-most-once: a slow subscriber that falls more than 1024 messages behind is told it lagged and skips. Inboxes survive daemon restart, but `inbox --drain` and subscription handover remove queued messages before transport acknowledgement, so a broken connection can lose that delivery. Clients that require recovery should read without draining and explicitly acknowledge only processed message IDs through `ack_inbox`. A `lagged {skipped}` response explicitly reports skipped live items. The CLI warns and continues for messages; event streams exit with an error directing the caller to recover retained history.

## Channels

A lease keeps two agents out of one file. A channel is what happens when they are in it anyway.

The ledger already records which checkout changed which path. The second checkout to change a path is a collision, so the daemon opens a room for the agents behind those checkouts rather than wait for one of them to notice: `channel_opened`, a message to each member, and a `review` journal entry. One open contested channel per project, not one per file — agents colliding on `src/parser.rs` and then on `src/lexer.rs` are having one conversation, so paths accumulate on the same channel and any newly involved agent is admitted (`channel_joined`). A channel can also be opened deliberately for a task (`channel_open`), which is the case where agents are told to work on the same thing rather than discovered doing it.

A channel is a message destination, `channel:<id>`. Unlike a topic it has a membership rather than a subscription: an agent put in a channel hears it without asking, and offline members get the message in their inbox like any other. Nothing outside the membership sees it.

**Review is the tie-break.** Inside a channel an agent asks for review (`review_request`) and the others answer (`review`) with `approve`, `changes`, or `comment`. Only a reviewer's latest word on a given author counts, nobody reviews their own work, and a request for changes blocks until that same reviewer says otherwise — so `decision(author, required)` is `Blocked`, `Approved`, or `Pending`. That is deliberately the tie-break rather than a race: when two agents have both done the work, what settles it is what the other agents say about it, not who finished first. Verdicts record the reviewed checkout's HEAD, so a verdict can be read against the code it was actually given.

A channel closes when the work is final (`channel_close`, with a resolution that goes in the journal) or when its last live member leaves, which closes it as "everyone left". Closed channels stay readable until `channel_prune` forgets them, a fortnight by default.

## Contests

Several agents attempt one task, and the evidence decides. A channel is what happens when two agents collide by accident; a contest is the deliberate version, and it is the answer to "should they race, or should they review?" — they do both, in that order.

**Correctness gates first.** An entry is a *passing `validate` run of the entrant's own*, in the checkout it is submitting. A failing run is refused with the reason (`it timed out`, `the code changed while it ran`, `it exited 1`), and so is somebody else's passing run: the evidence has to name the submitting agent. So "best" can never mean "fastest to produce something broken", and never "quickest to borrow a green result".

**The measure is fixed before anyone starts.** `contest_open` records it and nothing changes it, so nobody picks the flattering number after seeing the results. Where the daemon can take the measurement itself it does: `validation_seconds` is how long the entry's own validation ran, as the daemon timed it, and a score reported alongside it is ignored — there is a test that submits `-999` and gets the real duration. Anything else is a `reported` measure with a name everyone uses, which the daemon cannot check and review can. `agentdocker contest show` says which kind it is, because one score is evidence and the other is a claim.

**A margin inside the noise floor is not a win.** Also declared up front. Everything within it is a tie, and closing a tied contest on the ranking is refused: the metric has said all it can, so the channel settles it and the closer names the winner explicitly. A contest opens its channel at the same time it opens, because a tie has to be argued somewhere and asking for a room after the numbers are in looks like a loser asking for a rematch. Entrants who join later are admitted to it, so a latecomer can argue its own case.

`contest_open {agent, project?, task, metric, entrants?, channel?}`, `contest_enter`, `contest_submit {validation, score?}`, `contests {project?, agent?, all?}` and `contest_close {winner?, resolution?}` are the protocol; `agentdocker contest open|enter|submit|show|close` and `agentdocker contests` are the CLI — `open` prints the new id and nothing else, as every other creating verb here does, so `contest=$(agentdocker contest open …)` captures an id rather than a report; MCP exposes `contests`, `enter_contest` and `submit_entry`. Closing is restricted to the opener and the entrants, the same boundary `channel_close` draws: settling somebody's work belongs to the people in it. Every step is announced (`contest_opened`, `contest_entered`, `contest_submitted`, `contest_closed`), the channel is told where things stand after each entry, and the result goes in the project journal, because a contest result is a decision somebody will want to read back.

## Events

The complete variant definitions and payloads are in [`EventKind`](../crates/core/src/event.rs). They cover discovery availability and PID/start-time changes; watcher and restricted-endpoint readiness; agent/process/restore transitions; messages/inbox acknowledgements; lease acquisition, waiting/deadlock and release; journal/read observations; worktrees and integration; channels/reviews/contests; credentials, images and containers; validation and handoff. Each event carries a timestamp. Persisted events can be replayed; readiness and lag handling must follow the stream contract above. `agent_id` and `project_ref` are record types, not event variants.

`file_changed` and `agent_stale` are also emitted on the live event stream with `seq:0`; they are not persisted in ordered event history. `changes` reads retained ledger observations, and `stale` checks current content directly after a missed live notification.

## Process supervision

`run` defaults to closed stdin and captured stdout/stderr; `--tty` instead supplies a controlling terminal with attach input/output. Captured log lines carry timestamps and stream tags. The child inherits the daemon's environment plus `spec.env`. It is deliberately *not* given the CLI caller's environment, so secrets don't silently travel through the registry; pass what the agent needs with `-e`. On daemon shutdown every managed agent receives SIGTERM.

For supervised native commands, the daemon overrides `AGENTDOCKER_HOME`, `AGENTDOCKER_SOCKET`, `AGENTDOCKER_AGENT_ID` and `AGENTDOCKER_AGENT_NAME` with its own context after applying `spec.env`. It removes `AGENTDOCKER_TOKEN_FILE` because these children use the host endpoint, and sets `AGENTDOCKER_NO_AUTOSTART=1` so an unavailable owner fails explicitly instead of starting a replacement from a child. The same rules apply to initial launches and restored commands, including PTY launches. Explicit container mounts use their separate scoped endpoint and credentials.

## Security model

The host control socket is mode `0600` and trusts the owning user. This is not authentication between mutually untrusted same-user processes. State and log directories are created with mode 0700, and database/log files with mode 0600. Existing owned 0755/0644 state is narrowed without truncation; foreign ownership, writable-by-others paths and symlink/hard-link file targets are refused. CLI and GUI autostart apply the same policy before opening the daemon log. Explicit home aliases are canonicalized before validating the state directory; managed internal files never follow symlinks. The separate `container.sock` also has mode `0600` but requires a scoped token: first `authenticate`, then exactly one operation, then close. Host request frames are bounded to `IMPORT_BYTES + 64 KiB` (8 MiB + 64 KiB, including the newline); the handoff bundle itself is limited to 8 MiB. Restricted-endpoint request frames are bounded to 1 MiB and restricted connections time out after 30 seconds. Tokens are stored hashed, scoped to one running agent and physical checkout, and checked for revocation/expiry on every operation. Only mapped path claims/reads, own inbox/lease operations, inspection, direct project-peer messaging, and mapped journal note writes/reads are allowed. Journal digest readers are bound to the token's agent, and cursor advancement is limited to that agent. Host process control, validation execution and credential administration are unavailable. Missing tokens never fall back to host authority. Credential grants and revocations commit their state with the corresponding replay event before reporting success. Failed writes or corrupt credential storage return `storage_unavailable` and disable coordination until restart; event failure leaves no partial grant/revocation. Existing leases survive revocation until normal release/expiry/observed exit. Engine sockets and the host control socket are never container mounts.

## Roadmap

The [product direction](PRODUCT-DIRECTION.md) defines current delivery priorities. The phases below retain the detailed engineering design; numbered delivery rows are not GitHub PR numbers.

Phases 0–2, read tracking, durable recovery, explicit worktree integration and scoped container transport are implemented in the feature stack; merge and public release status are tracked in GitHub. Engine-managed build/launch, authenticated workspace mounts, managed Podman VM transport and image-bound validation provenance are implemented in the container stack. Docker Desktop uses the engine-volume socket relay; actual Desktop verification is tracked separately from Linux engine tests. Unimplemented items in Phases 4–6 remain design intent, written at the level of detail needed to build it — data model, protocol, storage, CLI, events, and what "done" means — so that each item can become a PR without a second design pass. Phases are ordered by dependency, not importance; [Delivery order](#delivery-order) lists the PR sequence.

### Where AgentDocker sits

[Herdr](https://github.com/herdrdev/herdr) (Rust, Apache-2.0) is the runtime coding agents run *on*: it owns their terminals. Sessions persist across a closed lid, a dropped network, or a restart; every pane is marked working, blocked, or idle; agents spawn panes, prompt each other, and wait until another is genuinely blocked, through a CLI and a socket API. Its own README is explicit about the boundary — it "doesn't wrap them or replace them, it just owns their terminals."

AgentDocker owns the other half: not where an agent runs, but what it may touch, what it changed, and who else needs to know. Leases over canonical physical paths so two agents cannot edit one file; durable read sets so an agent is refused an edit against something it has not re-read; an attribution ledger; a per-project journal with a cursor per reader; a worktree per agent with validated integration; handoff bundles; channels with review as the tie-break. None of that appears in herdr, and nothing in it stops two agents writing the same file.

So they compose rather than compete, and the sharpest way to say it is that herdr answers *where does this agent live and survive* while AgentDocker answers *what does it know and share*. An agent in a herdr pane can register with `agentd` and get the whole working set. Recognising *which* pane or session it is in — so a person can be pointed back at it — is row 25, and is not built here.

**On reusing their work.** Apache-2.0 permits forking, modifying and redistributing herdr, including commercially, provided the licence travels with the copy, modified files record that they were changed, any `NOTICE` is preserved, and their marks are not used to describe this product. A fork is therefore allowed. It is not recommended: carrying a copy of a large, fast-moving codebase costs more than the design does, and re-implementing persistence in our own crates keeps the working set — the part nobody else has — at the centre rather than making us a derivative of somebody's terminal server. Where a component is cleanly separable and genuinely reusable, depend on it or vendor that component with its licence and attribution intact, rather than fork the repository. Row 23 is the design taken; no code has been copied.

### The thesis

Docker's moat was a layered filesystem plus namespaces: the daemon knew exactly what a container could see and change. AgentDocker's equivalent is the **working set**. For every agent the daemon observes the paths it read, the resources it holds, the paths it changed, the branch it is on, and the messages it exchanged. Nothing asks an agent to describe its own state: hooks and adapters report what happened, and the daemon derives the rest. From the working set the daemon can do what no single agent can — group agents by project, tell an agent that something it read has since moved, attribute every change to whoever made it, detect two agents waiting on each other, and package a handoff. That is the proprietary layer; each item in Phases 2–5 is one of those derivations. The rule for new features: prefer deriving from what the daemon already sees over asking agents to declare it.

### Phase 1 — adapters & persistence *(done)*

[Persistence](#persistence), [`agentdocker mcp`](#agentdocker-mcp-cratesclisrcmcprs), [`agentdocker hook`](#agentdocker-hook-cratesclisrchooksrs), [`Agentfile.toml`](#agentfiletoml-and-agentdocker-up--down-cratesclisrcagentfilers-teamsrs), and `claim --wait` all exist. A FIFO wait queue now exists (see [Wait queue, deadlock detection, and what an agent is doing](#wait-queue-deadlock-detection-and-what-an-agent-is-doing-done)). One thing the original design called for is still deliberately deferred: a daemon-side notion of a team (the Agentfile is a client convenience and team selection uses agent labels).

### Phase 2 — native install & projects

#### Native install *(done)*

`agentd` is a native, per-user host process — never a container. It supervises processes that use the user's repositories, credentials, and editor; its liveness check signals pids; its path leases canonicalise real paths. All of that requires the same kernel and filesystem namespace as the agents, which is also why `dockerd` runs natively. Sandboxing is an *agent* concern (Phase 4 runtimes), not a daemon one, and a shared system-wide daemon for several users belongs with federation.

- ~~**Artifacts.**~~ Done: the release workflow, `install.sh`, and the Homebrew formula (the tap repository itself, and publishing to crates.io, are release-time steps for the maintainer).
- ~~**Service.**~~ Done; see [Starting the daemon](#starting-the-daemon).
- ~~**Lazy start.**~~ Done; see [Starting the daemon](#starting-the-daemon). The quick start no longer needs `agentd &`.

#### Project identity *(done)*

See [Projects](#projects). Compared with the original plan, derivation lives only in the daemon (clients just send a working directory), which keeps one code path and one fingerprint cache so every agent in a repository gets the same id; `ps` shows a `PROJECT` column and sorts by project rather than printing headings, so its output still pipes into `awk`. The `project:` destination and project-aware hook orientation followed in PR 2.

#### Discovery and adoption *(done)*

See [Projects](#projects). The known-runtime table is code for now; making it configurable waits for the daemon config file that admission policy (Phase 5) introduces.

#### Branch and head *(done)*

See [Projects](#projects). `observe`, `reads` and `stale` provide content observations independently of VCS reports.

### Phase 3 — the working set

This phase replaces the earlier plan for a separate key/document context store. Grounding staleness in the filesystem — what agents actually read and change — covers the real case with no new concept for agents to learn; a document store can be added later if a need survives this.

#### Read-set tracking and staleness

- **Observation.** Claude hooks call `observe` before Read/Grep/Glob and check `stale` before editing. MCP clients explicitly call `observe_paths` before reading and `check_stale` before editing. Read/Grep/Glob results are not intercepted by the daemon.
- **Identity.** Read marks contain absolute physical paths, SHA-256 content versions, times and optional HEAD context. The durable set is capped at 1,000 marks; overflow is rejected rather than silently evicted. Separate worktrees and clones retain separate observations.
- **Verification.** Current content is compared directly, including uncommitted changes before watcher debounce. Queries do not acknowledge staleness. A newer read of a target can shadow an older directory observation when checking that target; broader directory checks still inspect the directory mark.
- **Watching and attribution.** The watcher covers registered Git/Agentfile checkouts, debounces events, honors hierarchical ignore rules, and emits explicit gaps for lost coverage. Attribution uses an unexpired exclusive lease on the physical checkout path; shared readers are not authorship evidence. Otherwise attribution is external, and all attribution remains best-effort.
- **Notices.** Changes warn readers of the same checkout. Hooks surface queued notices and deny stale edits until the affected content is observed and reread. Live ledger events carry their own ledger sequence; replayed daemon events use the ordered event sequence.
- **Durability.** Read sets are stored as versioned-content documents, survive daemon restart and remain available after session exit for recovery. The implemented wire contract appears in the table above and the content-observation section below.

*Implemented*: read → change → warning → stale-edit denial → reread, through supported hooks or explicit client calls. Generic adopted processes are not automatically observed.

#### Attribution ledger *(done)*

See [Watching and the ledger](#watching-and-the-ledger). The watcher, filtering, attribution through leases, the `changes` table, and `changes`/`blame` all exist; attribution uses physical exclusive leases and remains best-effort.

#### Change journal *(done)*

What exists is described under [The journal](#the-journal); the rest of this section retains the historical design sketch. The implemented wire types and contracts above take precedence where the sketch omits newer fields or indexes.

A per-project, append-only narrative of what changed and why: coarse where the ledger is fine-grained, readable by models and humans, cheap to read incrementally, and the thing a newcomer is handed instead of the event stream. The design below was settled decision by decision on 2026-09-04 (the list is at the end) and has since been implemented and extended.

**Entries.** One entry per *release request* — a `release` or `release_all` that freed at least one lease — never one per resource, so a `Stop` that drops twenty file leases yields one line, not twenty. An entry is written when the request freed a lease and either the ledger shows changes under those resources or a summary was given; a lease claimed and abandoned untouched leaves nothing.

```
JournalEntry {
  project, seq,                    // seq is per project, assigned by the daemon
  at, agent, agent_name,
  branch: Option<String>, worktree: Option<RelPath>,
  kind: Release | Note | Commit | Join | Leave | Handoff,
  summary: String, summary_source: Explicit | Transcript | Synthesised,
  resources: Vec<ResourceKey>,
  paths: Vec<RelPath>,             // deduplicated, sorted, project-relative, capped at 200
  paths_total: usize,              // the real count when the cap bit
  head_before: Option<String>, head_after: Option<String>,
  changes: Option<(u64, u64)>,     // ledger seq range for drill-down while those rows exist
}
```

| kind | source | summary |
|---|---|---|
| `release` | `release {…, summary?}` / `release_all {…, summary?}` — the release request gains an optional summary | explicit text if given; else the tail of the transcript (below); else synthesised from the ledger: "edited 3 files under src/: parser.rs, lexer.rs, mod.rs" |
| `note` | `journal_add {agent, summary}` — CLI `journal add "…"`, MCP `journal_note` | free text; no resources |
| `commit` | the project watcher sees `.git/HEAD` or a `refs/heads/*` move | "committed `abc123` on `feat/x`: <subject>"; attributed to the worktree's isolated agent, else the `branch:` lease holder, else external |
| `join` / `leave` | agent created in / exited from the project | "codex-1 joined (worktree `wt-2`, branch `feat/x`)" |
| `handoff` | Phase 4 | task and note from the bundle |

**Summaries without a model round-trip.** Claude Code's `Stop` hook receives `transcript_path`. The hook reads the last 64 KB of that JSONL file (seek from the end, so cost does not grow with the transcript), walks lines backwards to the last assistant message with text, strips markdown, takes the first paragraph, and trims to 280 characters at a word boundary. That text is sent as the `release_all` summary with `summary_source: transcript`, so renderers can quote it rather than assert it. An explicit summary — `release --summary`, `journal add`, the MCP tool — always wins; the ledger synthesis is the floor. Asking the model for a one-liner by blocking `Stop` is not done by default; it can be an opt-in hook flag later without changing the data model.

**Scope and filtering.** One journal and one cursor per project. Every entry records `branch` and `worktree`, and the digest filters rather than the storage partitioning: the reader's own branch verbatim, `join`/`leave`/`handoff`/`commit` from every branch, and one trailing line counting other-branch entries ("3 entries on other branches: `agentdocker journal --all-branches`"). `--all-branches` shows everything. Entries an agent skips because of the filter still count as seen: they were summarised in the count line.

**Cursors, not timestamps.** `journal_cursors (agent, project, seq, updated_at, PRIMARY KEY (agent, project))` records the last entry each agent was shown. "Since you joined" means *since your cursor*, so an agent that was heads-down for an hour is told what it missed, not what happened after an arbitrary time. A never-seen agent's cursor starts at the newer of "24 hours ago" and "20 entries back". At registration the cursor is **seeded by name**: if a finished agent with the same name exists in the same project, its cursor is copied, which is what makes a resumed Claude Code session (same session id, so the same `claude-<prefix>` name) continue where it left off instead of being told everything twice; pid-based names such as `codex-1234` are protected by the same-project rule and a 7-day limit on the finished record's age. The `user` agent has a cursor too, so `agentdocker journal --new` shows the human what they have not looked at. A cursor is written only when it moves.

**Storage.** Three tables plus an index for search, all created with `IF NOT EXISTS` (no `SCHEMA_VERSION` bump):

```sql
CREATE TABLE journal (
  id      INTEGER PRIMARY KEY,           -- rowid, needed by FTS5
  project TEXT NOT NULL, seq INTEGER NOT NULL,
  at TEXT NOT NULL, agent TEXT NOT NULL, branch TEXT, kind TEXT NOT NULL,
  json    BLOB NOT NULL,                 -- the whole JournalEntry, self-contained
  UNIQUE (project, seq)
);
CREATE INDEX journal_branch ON journal (project, branch, seq);
CREATE INDEX journal_agent  ON journal (project, agent, seq);
CREATE TABLE journal_paths (project TEXT, path TEXT, seq INTEGER, PRIMARY KEY (project, path, seq)) WITHOUT ROWID;
CREATE VIRTUAL TABLE journal_fts USING fts5 (summary, content='', contentless_delete=1);  -- rowid = journal.id
CREATE TABLE journal_cursors (agent TEXT, project TEXT, seq INTEGER NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY (agent, project)) WITHOUT ROWID;
```

The blob keeps a read to one row. `journal_paths` is the index behind `journal --path src/` — a prefix range on `path` — so path filters never scan JSON. `journal_fts` backs `--grep`; if FTS5 is missing from the SQLite build (`Store::init` checks) search falls back to `LIKE` over the blob. Entries are a few hundred bytes, a few kilobytes with a long path list; paths cost about 40 bytes each in the side table.

**Ledger coupling.** An entry inlines its paths (capped at 200, with `paths_total`) *and* carries the ledger seq range. Digests therefore never touch the ledger, and `journal show <seq>` can still expand to individual changes while the ledger rows exist; once the ledger is pruned (newest 100,000 rows per project) the range dangles harmlessly and the inline list is what remains.

**Write path.** First flush or finalize pending observations through a release barrier, so debouncing cannot make the release precede its changes in the ledger. In the release handler, after the lease table has dropped the leases: for each released physical `path:` resource, resolve its checkout and project-relative path and issue one indexed prefix-range query on the ledger bounded by `[acquired_at, now]`; union, sort, dedupe, cap; build the entry; assign `seq` from the per-project counter (loaded from `MAX(seq)` at startup, like event `seq`); write `journal`, `journal_paths`, and `journal_fts` in the *same transaction* as the lease deletions so a crash cannot leave a released lease with no entry or an entry for a lease still held; push to the ring; emit `journal_appended {entry}`. Sub-millisecond on the indexes above, and the response goes out after the write like every other mutation.

**Read path.** `journal {project, since_seq?, until_seq?, agent?, branch?, kind?, path?, grep?, limit?, digest?}` returns `journal {entries}`. With `digest: {reader, max_entries, max_chars, all_branches?, advance?}` it instead returns `digest {text, head_seq, shown, collapsed, other_branches}`: entries after the reader's cursor, filtered as above, rendered oldest to newest, one line each:

```
Since you last looked (9 entries):
… 4 earlier entries (agentdocker journal --since 1173)
- 1h ago   gemini-2 joined (worktree wt-2, branch feat/y)
- 18m ago  claude-a1b2 [main] committed 3f9c1e0: "Add lease transfer"
- 4m ago   codex-1 [feat/x] released src/parser.rs, src/lexer.rs (+3 more): "rewrote the tokenizer to handle unicode escapes"
3 entries on other branches: agentdocker journal --all-branches
```

When the budget bites, the *oldest* entries collapse into the leading "… N earlier entries" line and the newest stay verbatim. `advance: true` moves the reader's cursor to `head_seq` in the same request — one round trip per hook fire, and nothing is marked seen unless the text was produced.

**Budgets.** `SessionStart` requests up to 20 entries or 2,000 characters (about 500 tokens) with `advance`. `UserPromptSubmit` requests only what is past the cursor, at most 5 entries or 500 characters, and injects nothing when nothing is new. `PostToolUse` never carries journal text. Both budgets are hook flags (`--digest-entries`, `--digest-chars`). The CLI's `agentdocker journal` defaults to the last 50 entries of the project containing the current directory and takes `--since <seq|duration>`, `--agent`, `--branch` / `--all-branches`, `--path`, `--grep`, `--new` (since the human's cursor; `--ack` advances it), `--follow`, and `-n`.

**Caching.** The daemon keeps an in-memory ring of the newest 256 entries per project, created lazily on the first read or write for a project with live agents and dropped ten minutes after its last live agent leaves. Appends write through; a digest whose cursor lies inside the ring is served without touching SQLite, which is every hook fire in practice; `--since` older than the ring, `--path`, and `--grep` go to the tables. Rendering is cheap enough that digests themselves are not cached; if that ever changes the key is `(project, cursor, head_seq, budget, branch)`. Memory cost is about 256 KB per active project.

**Retention.** Entries are kept forever by default: the journal is the audit trail, a busy five-agent team writes roughly a megabyte a day at worst, and SQLite is comfortable at millions of rows. `agentdocker journal prune --before <duration|seq> [--project]` deletes on demand, and an optional `[journal] retention = "180d"` in the daemon config is applied by the once-a-minute tick, deleting `journal`, `journal_paths`, and `journal_fts` rows together in batches of 1,000 so the tick stays short; a cursor below the new floor is clamped on read. Freed pages are reused by SQLite; `agentdocker daemon vacuum` reclaims disk when someone wants it back. No roll-up summaries.

**Adapters.** Hooks: `SessionStart` and `UserPromptSubmit` inject the digest as `additionalContext`; `Stop` sends `release_all` with the transcript-tail summary. MCP: `read_journal {since?}` returns the digest with `advance`, `journal_note {summary}` appends a note, and the `release` tool accepts `summary`.

**Tests.** Core: rendering under both budget limits, the collapse rule, the branch filter, and cursor seeding (same name and project, within 7 days) are pure and unit-tested. Store: round trip, path-prefix and FTS queries, prune cascades, cursor clamp. Hooks: transcript-tail extraction against sample JSONL (markdown stripped, first paragraph, 280-char word boundary). Daemon: a release under which the ledger recorded changes produces one entry with those paths in the same transaction as the lease deletion.

*Done when* session A edits two files and stops; session B starting in the same project is handed one line naming both files and A's last message; A resumed sees nothing it was already shown; and `journal --path src/` returns the entry without scanning.

**Decisions (settled 2026-09-04).**

1. Granularity — one entry per release request; per-path lookups via `journal_paths`.
2. Synthesis — ledger paths always; transcript tail on `Stop` when no explicit summary; no blocking round-trip by default.
3. Cursor identity — per agent id, seeded from a same-name finished agent in the same project.
4. Scope — one journal per project with a `branch` column and a filtered digest.
5. Retention — keep forever; prune on demand or by an optional retention setting; no roll-ups.
6. Ledger coupling — inline capped path list plus the ledger seq range.
7. Digest budget — `SessionStart` 20 entries / 2,000 characters; `UserPromptSubmit` new-only, 5 / 500.

### Phase 4 — layers, sandboxes & handoff

#### Worktree integration

Implemented commands are `worktree-create --as <agent> <new-path> --branch <new-branch>`, `worktree-diff --as <agent>`, and `integrate --as <target-agent> <source-path> --validation <id> [--apply]`. Worktree creation uses the current HEAD and keeps existing files. Register or run the source session in the new checkout separately. Independent physical checkouts have independent write leases even when their relative paths overlap.

Integration requires linked worktrees, clean source/target trees and passing validation whose content identity and HEAD still match the source. Preview returns a diff; `--apply` claims the physical target checkout and runs a no-commit, no-fast-forward merge. The target lease stays held for review, including conflicts. The caller uses Git to inspect, commit or abort, then releases the lease. No automatic commit, force reset, branch deletion or worktree purge is performed.

`run --isolate` (or `isolate = true` in an `Agentfile.toml` entry) gives a managed agent its own writable layer without a separate command: before the process is spawned the daemon adds a linked worktree of the repository the working directory is in, under `<home>.worktrees/<project>/<name>` on branch `agent/<name>` — with the agent's id appended to both when an earlier run left them behind — points the agent's working directory at it, and announces `worktree_created`. If a launch fails before any writer can start, the daemon attempts normal Git cleanup of the new checkout and branch. Changed/dirty checkouts and advanced branches are retained; `worktree_cleanup` reports what was removed or kept. Engine-uncertain launches retain their checkout and protection. The agent is grouped with the repository like any worktree, its `join` line names the worktree and branch, and the worktree stays after the agent exits so its work can be integrated; purging is still by hand. Image runs require `--mount-checkout`; linked Git metadata is mounted separately with its original directory layout, and validation mounts both source and Git metadata read-only.

`overlap` is the ledger read across checkouts: paths that more than one physical checkout of the project changed, each with the agents attributed there, how often, and the last change and its HEAD, newest checkout first — merge conflicts before they happen, without diffing. `agentdocker overlap [--project] [--since N] [--as AGENT]` and the MCP `overlap` tool (this agent's checkout against the others). Semantic overlap — the same symbol touched from two sides — is a future extension.

#### Sandboxes and container engines

A sandbox is a property of an agent's runtime, and a container is one optional way to get one: nothing in the core, the CLI, the hooks or the MCP server depends on a container engine, and AgentDocker is complete without one. What follows is the design of that optional adapter.

Docker and Podman are equal targets for the container workstream. AgentDocker remains a native host daemon. It delegates image builds and container execution to an installed engine; its own responsibility is physical checkout identity, observed working state, authentication, and verified recovery. The agent runtime (`codex`, `claude-code`, or another adapter) is separate from the container engine (`docker` or `podman`). Apple's `container` remains a future adapter, pending equivalent capability and lifecycle tests.

The current implementation provides worktree operations and a separate authenticated container endpoint. Managed image build/launch, authenticated mounts, Podman VM transport and image-bound validation are implemented. The delivery and acceptance plan is [CONTAINER-ENGINES.md](CONTAINER-ENGINES.md).

The shared engine interface covers availability/capability checks, image build/inspection and managed container lifecycle, logs and exit observations. Implementations invoke the selected engine with structured arguments. Engine selection is explicit and persisted; a failed engine must never silently switch to another engine or the host. Record the engine, container ID, resolved image ID/digest and platform with the agent. A client process exiting does not prove the container stopped: engine inspection must establish termination before releasing its protection. An unavailable engine leaves status uncertain and protection governed by existing lease TTLs.

Build support uses a common Dockerfile/Containerfile and explicit context, with per-engine handling for unsupported features. Podman accepts both formats, but its `buildx` compatibility does not cover all Docker Buildx features ([Podman build reference](https://docs.podman.io/en/latest/markdown/podman-build.1.html)). Build provenance must record the source content identity, build recipe, engine/version, target platform, and resulting immutable image identity. Build success is distinct from test success; validation evidence also needs the image identity and command before it can be reused across container sessions.

The host control socket is never mounted in a container. `grant-access` creates an expiring credential scoped to one running agent and one physical checkout, writes the secret to a private token file, and supplies only `container.sock`. The CLI uses `AGENTDOCKER_TOKEN_FILE`; the endpoint requires authentication and rechecks identity, expiry and revocation for each operation. Container `/work` paths translate to canonical host paths before lease and read-set lookup. Token revocation denies new requests but does not free a running writer's leases. Runtime-native sandboxes may need their own transport configuration; hooks and MCP are not assumed to run outside every sandbox.

Engine adapters must handle rootless ownership and VM mount reachability explicitly. A host Unix socket is not presumed usable through a macOS VM bind mount. The VM bridge may expose only the authenticated endpoint, with a tested path mapping; no privileged engine socket is made available to an agent. Default container policy is no network, no inherited host environment, no engine socket, and only the selected checkout plus the scoped endpoint and token mounts. Network access and additional mounts are explicit configuration. Worktrees isolate edits; container engines provide the process/filesystem boundary.

#### Handoff bundles *(done)*

An agent handing work to another should not have to write its state down; the daemon already knows it. A handoff is a checkpoint addressed to someone. `handoff {agent, to?, task?, note?, transfer_leases?, key?}` makes the checkpoint through the checkpoint path — same release barrier, same idempotency by key, leases released with it unless they are to move — then assembles a `HandoffBundle { schema, id, from, from_name, to, project, task, note, assumptions, next_steps, checkout, version (the checkpoint's content identity), environment (image and execution provenance), vcs, leases (what the sender held when the bundle was made), transfer_leases, read_set, changes (the sender's ledger rows since it joined, at most 1,000), diff (for a sender in a linked worktree: the tracked patch, cut at 64 KB on a line boundary with the worktree named), unread_inbox, journal (the sender's own entries), journal_cursor, created_at, imported_at }`, stores it as a document under the checkpoint's id with `handoff_sent`, sends `to` a message `kind: handoff` whose payload names the id and the task, and appends a `handoff` journal entry ("codex-1 handed off to gemini-2: finish the parser"). Retrying with the same key returns the same bundle. Acceptance is `resume {…, acknowledge: true}` by the addressee — anyone else is refused — after the usual content verification, and it commits ownership in one transaction: with `transfer_leases` the still-live leases listed in the bundle that the sender still holds move to the recipient (`LeaseTable::transfer_selected`, one `lease_transferred {lease, from, to}` each), the read set is seeded from the bundle so staleness carries over, and the recipient's journal cursor is set to the sender's so it continues reading where the sender stopped. A bundle nobody was addressed to (`agentdocker export --as <agent> > bundle.json`) is the same structure; `agentdocker import --as <agent> < bundle.json` on another host re-homes it — the checkout becomes the importer's, read marks move with it, leases and the cursor stay behind — stores it with `handoff_imported`, and tells the importer; `resume` then accepts it only if the content identity matches exactly, as ever. The bundle carries schema 2; older importers must refuse it rather than drop image evidence. `agentdocker handoff <to> --as <from> --task "…" [--note …] [--transfer-leases]`, `agentdocker handoffs`, and the MCP `handoff` and `list_handoffs` tools.

### Phase 5 — the machine and the human

#### Runtime inventory, setup, and continuous discovery *(done)*

`agentdocker runtimes` lists the agent tools on this machine: for each known runtime (`claude-code`, `codex`, `gemini-cli`, `cursor`, `aider`, `goose`, `copilot`, `amp`, `opencode`) whether its CLI is on `PATH` or in a standard installation directory and which version, and its desktop registration where one is known (macOS bundles; Linux Cursor/Windsurf/VS Code desktop-entry IDs in XDG precedence order), its config directory, and whether AgentDocker is wired in: hooks installed for Claude Code, the MCP server registered in Claude Code's `~/.claude.json`, Codex's `~/.codex/config.toml`, Gemini's `~/.gemini/settings.json`, or Cursor's `~/.cursor/mcp.json`. `agentdocker setup [RUNTIME...] [--dry-run]` writes the missing registrations idempotently, keeping private numbered backups without replacing earlier backups, and prints what it changed. Direct JSON/TOML edits and hook installation use complete atomic replacements and refuse changed inputs. Hooks count as installed only when all adapter events and the full pre-tool matcher are covered by a verified command; executable paths are shell-quoted. Claude CLI registration has a 30-second command limit and cannot treat an unverified existing server as success. Existing reserved MCP entries that are disabled or cannot be verified produce a setup error and remain untouched. Runtime inventory reports `unverified` for these entries and unreadable or malformed MCP configuration, distinct from an absent (`missing`) registration; a verified alias cannot hide a conflicting reserved entry. The CLI and native window retain setup review for either state. A wrapper or arbitrary argument mentioning AgentDocker does not prove MCP wiring. Claude Desktop has a separate setup target from Claude Code, and the presence of VS Code does not prove Copilot CLI or an agent extension is installed. Codex CLI, Codex desktop and ChatGPT have independent rows; no desktop integration is inferred from the Codex CLI configuration. Standard CLI fallback directories let a GUI started without a login shell find installations; they do not change PATH or make a configured bare MCP command executable. Linux user `Hidden=true` entries mask system entries, `NoDisplay=true` remains installed, and desktop-entry `Version` is not an application version. Entry reads are bounded to 64 KiB regular UTF-8 files; `Exec` quoting, escaping and field codes are validated; `Exec` and `TryExec` are never run. These are recognized local registrations, not publisher authentication. Failed inventory returns `unavailable`; the GUI retains its last rows and reports the request error while keeping a working daemon connection. The inventory is host I/O in `agentdocker-host::runtimes`; the daemon serves it as `runtimes {}` so the desktop app and the CLI share one answer.

Discovery is continuous: the daemon scans every five seconds with a bounded process-table command. Concurrent requests join the current scan through an atomic flag and notification; no state lock is held while the scan runs. It reconciles results against the current registry under the same state lock as registration. Sessions are identified by PID and observed start time, so PID reuse emits the old disappearance before the replacement appears. Metadata changes emit a new `agent_discovered {pid, started_at?, runtime, project?, cwd?}`; departures emit `agent_vanished {pid, started_at?, runtime, adopted}`. A failed scan emits `discovery_unavailable {reason}`, preserves its last successful snapshot and never fabricates exits. The next successful scan emits `discovery_available`. These events have ordered persisted replay sequences; the live process cache is rebuilt after restart. `discover` and `runtimes` return `unavailable` when a required scan fails; `discover` answers from the last scan, and `adopt --all` registers every discovered process at once. Adopting automatically is a policy decision and waits for the policy file below.

#### Native desktop app *(done)*

Local desktop installation uses immutable payload versions and one activation
pointer. `desktop uninstall` removes owned launchers and deactivates the prefix;
`desktop prune` removes verified unused payloads. CLI and GUI previews carry a
plan hash rechecked at apply. Host installation helpers give each managed
binary a shared lifetime pin; cleanup holds the exclusive pin through deletion
and rechecks content identity. Pins outlive deleted versions to prevent inode
replacement races. Active, rollback, legacy unpinned and running versions remain;
an installed user service protects retained binaries and blocks uninstall.
These are local host operations, not daemon protocol mutations. They preserve
state/provider configuration and do not replace a running daemon. See
[desktop distribution](DESKTOP-DISTRIBUTION.md) for commands and limitations.

`agentdocker-ui` is a native window, not a web page: a Rust binary (`crates/ui`, egui/eframe) that talks to `agentd` over the same Unix socket as the CLI — a background thread for requests, one for the event stream — with nothing listening on HTTP. Screens: agents grouped by project — each project in a colour derived from its id, so it is the same colour every session and on every machine, shown as a dot on every row and named wherever rows from several projects mix, with a filter in the title bar for one project at a time — carrying status, branch, held leases and last activity; runtimes (installed, wired, running; adopt and set up from the app); the journal (per-project digest, follow); leases; the questions agents have put to you, each with the box you answer it in; a terminal, which is the same `attach` the CLI uses rendered by a vt100 emulator, so an interactive agent can be watched and typed at in the window, with the screen resized to the panel and scrollback replayed on attach; and a console that runs CLI commands with a 20-second limit and shows their output (long-running and streaming operations are not a persistent shell), because the command line keeps growing and a window that mirrored it in widgets would always lag behind. Desktop notifications for messages addressed to the human, questions included, come from the daemon rather than the app, so notification attempts do not depend on the window being open; OS permission or tool failure can prevent display, and display does not prove a person read it. `agentdocker ui` launches it; it ships beside the CLI. Windows follows once the daemon runs there. A ready event subscription restores connectivity even when no new agent event arrives; reconnects refresh agent, lease, runtime, discovery and selected journal snapshots. Stream lag is reported and forces a reconnect. Setup status includes the CLI diagnostics on stderr, and its subprocess is bounded. The app resolves the canonical daemon home and validates private fallback socket directories before connecting, as the CLI does. `AGENTDOCKER_NO_AUTOSTART` disables its startup attempts. Otherwise the app passes the resolved home and socket to the daemon, reports early child exit, and kills/reaps only its own child on startup failure; successful children remain alive and are reaped on eventual exit.

The native window's main command queue holds 32 requests and coalesces queued
snapshot refreshes by kind/project. Dispatch releases the key before I/O so a
change during a request can schedule one follow-up. Reply/event delivery uses
64 slots with backend backpressure; each logic pass drains at most 64 messages,
including when the window is hidden. UI submission never blocks. Queue rejection
preserves answer drafts, clears pending setup/installation controls and reports
user actions that were not admitted. The visible journal retains 200 entries,
console scrollback 256 KiB of UTF-8 tail, and recall 100 complete commands within
64 KiB; oversized commands execute normally but are not stored for recall.
Journal snapshots use their optional durable head to retain newer live entries
without resurrecting pruned rows. Older daemons keep snapshot replacement
semantics. These bounds do not cover terminal-input buffering or total payload
bytes/RSS, which remain resource-acceptance work.

#### Wait queue, deadlock detection, and what an agent is doing *(done)*

`claim --wait` used to be a retry loop and nothing more: on a conflict the request slept until an overlapping lease cleared, then raced every other sleeper for it. Making waiting a recorded fact fixes that and gives two more things for free, which is why rows 13 and 24 landed together.

- **FIFO queue.** `agentdocker_core::WaitQueue` (pure, no clock) records waiting requests in arrival order, with one order across the whole table rather than one per resource, so overlapping keys queue together. A waiter attempts a claim only when no *older* waiter wants something that overlaps and would exclude it; a newcomer therefore cannot take what somebody has been waiting minutes for. Two shared waiters do not block each other, so a queue of readers is not serialised by its own fairness rule — and the number published as `lease_waiting.position` counts by that same rule, so a shared waiter that may proceed at once is not shown as second in line. `lease_waiting {resource, requester, position}` is emitted on joining and `lease_wait_ended {resource, requester, outcome}` on leaving — `claimed`, `timeout`, `cancelled`, or `deadlock`. (The original design named only `lease_wait_timeout`; one event covering every way a wait ends says strictly more and is what waiters wake on.) The `deadlock` outcome is emitted even though the refused claim never joined, because a wait that never began still ended and a subscriber should not have to special-case the one outcome that skips the queue. The deadlock check and the join happen under one hold of the state lock, so two claims cannot each look, find nothing, and then both create the cycle they were both checking for. That one rests on reading the code rather than on a test: the fix removes the window, so a test that could enter it would need a hook the fix deletes, and a regression would have to restore the hook to be caught by it. What is tested is the invariant a reader cares about — a ring is broken, and broken exactly once, whichever order the two claims arrive in. A place is given up by an RAII guard, so a client that simply disconnects releases its place rather than starving the queue behind it: the request's future is dropped, and the guard with it. Waiters are connection-scoped and never persisted; a daemon restart drops every waiting client, which reconnects and takes a new place.
- **Deadlock detection.** `wait::deadlock` is a pure depth-first search over two slices the daemon already keeps — who holds what, and who waits for what — so there is no separate graph to keep in step with the lease table. Before a claim agrees to wait, it asks whether waiting would close a cycle; if it would, the claim is refused at once with `ErrorCode::Deadlock`, `details.cycle` naming every agent and resource in the ring, and a `lease_deadlock {cycle}` event. The newcomer is always the victim: deterministic, and it needs no priorities. TTLs already bounded how long a deadlock could last; detection makes it instant and explains it. Priority-based victim selection stays an open question.
- **Derived activity.** An agent in the wait queue is blocked **on a named resource held by named agents** — the thing a multiplexer can only guess at from terminal output. `activity {agent?, project?, all?}` answers `activity {activity: AgentActivity[]}`, where each is `starting`, `working`, `blocked {resource, held_by, since}`, `idle` or `finished`, blocked ones first because those are what somebody has to act on. Working means the agent acted through the daemon within two minutes: hooks report every tool run, the MCP server every call, and a file changing under a lease the agent holds now touches it too, so a runtime with no hooks at all is still read honestly. `agentdocker activity` prints it, `ps` carries it as a `DOING` column, `waiting` lists the queue itself, MCP exposes `activity`, and the app shows it beside each agent instead of repeating the process status.

#### The human as an agent *(done)*

Orchestration needs an escalation path, and it lives inside the same model rather than beside it. `agentdocker me` registers the person at the keyboard as a persistent agent named `user` with runtime `human` (the `from: user` convention already existed) and prints its id, so `export AGENTDOCKER_AGENT_ID=$(agentdocker me)` is the whole of joining as yourself. It is idempotent — a second call returns the same record, and moves it to the project the shell is in — and the record has no pid, so the liveness sweep has nothing to check and never expires it. From then on a person is addressable like anything else: messages queue in their inbox, `watch --me` streams them, the journal keeps their cursor, and `ps` lists them.

`ask {from, to, question, timeout_secs?}` sends a `question` message and holds the connection until an `answer` naming it arrives, or answers `error(timeout)`. The daemon keeps the outstanding questions in memory, because an answer names a question by id and only the question knows who is waiting on it; `answer {message, text}` is therefore a one-argument act whether a person or an agent does it. An answer is an ordinary message as well as the end of a wait, so it also reaches the asker's inbox: an `ask` that timed out still leaves the answer where the asker can read it. A question that nobody answers stays in the recipient's inbox after the asker gives up — a timeout is the asker giving up, not the message being withdrawn.

Delivery to the human: `agentdocker watch --me` streams questions, `agentdocker questions [--me]` lists what is waiting, `agentdocker answer <message id> "…"` replies, and the desktop app has a Questions screen where the question and the box you answer it in sit together. MCP exposes `ask_human`, `answer_question` and `open_questions`; hooks expose nothing, because a model asks in prose.

The one thing genuinely different about a person is that they are not polling a socket, so the daemon raises a desktop notification for any message that reaches an agent whose runtime is `human` — `terminal-notifier` then `osascript` on macOS, `notify-send` on Linux — throttled to one per sender per minute. It is best-effort by design: a headless box has none of these tools, and that is not an error, because the message is still queued, still in the inbox, still on the event stream. Notifications are built under the state lock and posted on another thread, so a desktop that is slow to draw one never delays a coordination request.

#### Admission policy and budgets *(done)*

Leases stop two agents editing one file. Policy stops one agent touching something it was never meant to. `<home>/policy.toml` is the machine owner's; `<root>/.agentdocker/policy.toml` belongs to a project.

**A project can narrow the host and never widen it.** That asymmetry is the whole reason there are two files: a project policy travels in a repository and could be written by anyone who can open a pull request, so the host is asked first and its refusal is final. A quota works the same way — a project may lower one, or set one the host did not, but the tighter of the two always wins.

Rules match agents by runtime, name glob, project, and labels; every field left out matches everything. Within a rule, `deny` and `allow` are action patterns like `claim:path:/repo/migrations/**`, `send:project:*`, `run:**`. Three steps decide, and the order is the point: a matching **deny** refuses outright, whatever else is written and in whatever order; otherwise, if any rule covering the agent carries an **allow** list, that agent is in whitelist mode and the action has to appear in one; otherwise it is permitted. A machine with no policy file allows everything, and a file that only denies leaves everything else alone.

Globs are the ones used everywhere else: `*` stays inside a segment and `**` crosses them, so `claim:path:/repo/src/*` is a directory and `claim:path:/repo/src/**` is the tree. A `:` is an ordinary character, so `send:project:*` reads as it looks, and a pattern is anchored at both ends rather than being a substring search.

**Path patterns are canonicalised when a policy loads.** The daemon canonicalises a resource before claiming it, so on macOS the action says `/private/tmp/...` while the person wrote `/tmp/...`; a rule that silently matched nothing would be the worst possible failure for a security boundary, because it reads as protection. Only the literal part before the first wildcard is resolved, walking up to the nearest ancestor that exists — a rule about a migrations directory is usually written before the directory is.

Evaluation is pure: `policy::check(host, project, agent, action) -> Ruling` in core, with no filesystem and no clock, so every rule about precedence is a unit test. Refusals use `ErrorCode::Forbidden` with the rule and action in `details`, and a `policy_denied {agent, action, rule}` event fires, so a refusal is explainable from the event stream alone. `claim` is checked once before its first attempt rather than on every retry, and `send` is checked against its destination. A newly registered project policy is loaded before its first governed action. `run:<agent-name>` is checked before native, tmux and container launch side effects, including isolated worktree creation. Native restore/restart paths recheck after asynchronous preparation and before committing execution. Pending tmux and container launches also recheck before start; a refused pending container retains stop intent for reconciliation. Policy changes do not retroactively stop a running process. These are cooperative daemon admission rules, not an OS sandbox restricting arbitrary external processes.

Policy files are checked on the one-second tick using identity, size and change metadata. Only regular files up to 1 MiB are accepted; symlinks, FIFOs, directories, oversized files, unreadability and malformed TOML are errors. Errors retain the last valid rules. If no valid policy has been loaded, an error refuses policy-governed actions until the file is repaired or confirmed absent. Confirmed removal clears that scope. Reads happen outside the state lock and are bounded in size; filesystem metadata latency is not a hard wall-clock guarantee. `policy_updated {project, rules, quotas, error, using_last_good}` commits before changed admission state becomes visible. Failed event persistence retains prior rules and disables coordination. Diagnostics omit file contents.

**Budgets** ride the lease primitive as a quantitative resource kind. `claim quota:tokens --amount 50000` succeeds while the sum of live amounts fits the capacity set in policy, and fails with a `conflict` naming what is left when it does not. A quota is shared by construction — it is spent, not occupied, so exclusivity would make every budget a lock on itself — and the arithmetic happens under the same lock as the lease table, so two claims cannot both see room for the last of it. `Lease.amount` carries the quantity, zero for every other kind, which is what makes their arithmetic ignore it. A quota nobody set a capacity for is unlimited, so a typo in a quota name loosens nothing that was not already loose.

#### Supervision policy and dashboard *(done)*

**Restart policies.** `run --restart no | always | on-failure | on-failure:<n>`, and `restart = "..."` in an `Agentfile.toml`. The default is `no`, because a supervisor that restarts by default turns a command that fails immediately into a loop. `on-failure` counts, and only counts failures: a clean zero ends it whatever the limit, while a signal, a nonzero code and a spawn that never started all count as failures worth retrying. The decision is pure — `RestartPolicy::restarts(status, already)` in core — so every rule about it is a unit test rather than a daemon run.

Three properties make it safe to leave on. An agent **stopped on purpose stays stopped**: `stop` clears the policy on the record, so the reason it will not come back is visible in `inspect` rather than hidden in the daemon, the same rule `--restore` follows. Restarts **back off**, doubling from a fifth of a second and capped at half a minute, because the other case is a command that fails every time and the daemon should not spend a core discovering that. And the agent comes back **under its own id**, as a restored one does, so its read set, journal cursor, leases and ledger attribution continue to describe it — a restart is the same agent running again, not a new one with the same name. `agent_restarted {agent, pid, attempt}` announces each one, and `AgentRecord.restarts` carries the count, because a reader deserves to know an agent has died nine times.

Only a managed agent can have a policy: the daemon has to own the process to start it again, so an adopted or in-pane agent is left alone whatever its spec says.

Restart completion commits the updated identity, attempt count and `agent_restarted` event atomically. Failed completion stops and reaps its owned process; a cleanup timeout leaves supervision responsible for it. Failed storage does not expose a new Running record or schedule another retry. Failed spawn attempts record their count/status with an exit event atomically. Launch validation also rechecks the restart policy and attempt count after asynchronous preparation. Initial launch, snapshot restore and automatic restart now use the same pre-exec gate. Closing the gate or losing its owner denies execution; after activation the PID and exact birth identity are already durable. A failed exec records failure through owned supervision. Live-reload acceptance and full crash/reboot timing coverage remain unfinished.

**`depends_on`.** Names in an `Agentfile.toml` that must be running before an agent starts. `agentdocker up` orders the file by dependency — preserving the order things were written in wherever a dependency does not decide it — and then *waits* for each dependency to reach `running`, because ordering alone is not enough: an agent that is `created` has not run its first line, and the one about to start may be its client. The file is validated when it is read, so a name that is not in it, an agent depending on itself, and a cycle are all sentences rather than a wait that never ends. (`after = "A exits 0"` from the original sketch is not built: `depends_on` covers the case that came up, and a second ordering vocabulary can wait for a second need.)

**`agentdocker top`.** The fleet, live: agents grouped by project with what each is doing, how many leases each holds, and the wait queue underneath. Not a TUI framework — the screen is a few ANSI escapes and the content is the same rendering the one-shot commands use, so there is one renderer to keep correct rather than two. Redraws are driven by the daemon's event stream, so a lease taken or an agent blocked appears at once, with a slow tick underneath to keep relative times honest. Piped rather than shown on a terminal, it draws one frame and exits, so `top | head` is not a hang.

#### Sessions and persistence

`run` uses pipes by default for batch commands and `--tty` supplies a controlling terminal for interactive commands. Attaching to that terminal is separate from discovering or adopting an externally started process.

Row 23 fixes the first on our own terms, and it is done. `run --tty` (or `tty = true` in an `Agentfile.toml` entry) gives the agent a **pty** instead of pipes: `posix_openpt` in the daemon, and in the child, between `fork` and `exec`, `setsid` and `TIOCSCTTY` so the terminal is genuinely its controlling one. That replaces `process_group(0)` rather than joining it — `setsid` makes the child a process-group leader by itself, so signalling `-pid` still reaches its descendants, and doing both would fail. Everything the agent prints goes two ways: whole lines to the log, so `logs` reads exactly as before, and raw bytes to a broadcast for whoever is attached.

`agentdocker attach <agent>` connects your terminal to it. The local terminal goes into raw mode under a guard that restores it however the command ends, `SIGWINCH` is forwarded as `attach_resize` so full-screen agents lay out correctly, and Ctrl-] detaches. Attaching and detaching are only a client coming and going: the terminal belongs to the daemon, so the agent neither notices nor stops. A slow reader is told it missed bytes rather than being allowed to stall the agent.

Attaching late shows the screen rather than an empty one: the daemon keeps the last 64 KB each terminal printed, and hands it over with the live stream under one lock, so no byte falls between the two or arrives twice.

Only the terminal-output reader owns the broadcast sender. Session handles hold
weak references, so an attached client cannot keep its own output channel alive
after EOF. An attach racing the reader's exit receives the retained tail and an
already-closed receiver; the server sends `end` after queued output. The CLI
reads an independently reopened, nonblocking terminal through Tokio readiness
notifications. Dropping that read cancels it without a stranded stdin worker or
changes to inherited descriptor flags. The existing raw-mode guard restores
terminal settings on completion.

Live terminal continuity through daemon replacement remains unfinished. `daemon reload` currently returns `unavailable` without touching the daemon or agents. An unplanned death closes the master with the daemon; the child may exit with it, and a separate process group does not guarantee survival. Snapshot restore creates a new process and terminal.

Native launch uses a stateless host gate around `Command`: the forked child establishes its process group/terminal, reports its PID over an inherited private socket, and waits using only async-signal-safe syscalls. The daemon verifies its birth identity, commits the Running identity and event, then authorizes exec. Until authorization, EOF, cancellation or the 30-second child deadline denies exec. A worker completes Command's exec-error handshake; an owned child wrapper kills/reaps a launch whose asynchronous activation is dropped. No command, shell wrapper or helper application runs before the durable transaction. Normal exit supervision polls the owned child and drains its process group before releasing protection.

The gate closes the unrecorded-command execution window. It does not provide seamless daemon replacement or vendor conversation resumption. OS crash/reboot acceptance and recovery timing when the owner dies after commit but before activation still need the wider trial matrix.

Snapshot relaunch and transfer of live terminal descriptors are separate engineering tasks. The former restarts a stored command; the latter would preserve the running process during a planned upgrade. Neither automatically restores an LLM conversation.

Row 27, **snapshot restore**, has transactional preparation and startup readiness checks, with the original defects and follow-up evidence in the [audit](AUDIT-2026-09-06.md#blocking-findings) and [delivery record](NATIVE-DELIVERY.md). `run --restore` (or `restore = true` in an `Agentfile.toml`) marks a managed agent as one to bring back; opt-in, because starting a daemon should never spawn processes nobody asked it to, and `agentdocker ps` starts the daemon. On startup, before the liveness sweep can retire anything, such an agent is relaunched **under its own id**. That is the whole of it: the read set, the journal cursor, the checkpoints, the ledger attribution and the leases are all keyed by the agent id, so restoring the identity restores the working set with it rather than handing out a fresh shell in the right directory.

A clean shutdown records restore intent and stops its agents, so their records read `exited`; a crash normally leaves them `running`. An interrupted prepared restore can remain `created` with its durable point. Naturally completed commands have no shutdown point and remain completed. Points older than twelve hours are not automatically relaunched. An agent whose process group is somehow still alive is left alone rather than started twice, and an agent stopped on purpose is not restored — `stop` clears the flag on the record, so the reason it will not come back is visible in `inspect` rather than hidden in the daemon.

Leases need one extra step, because releasing them when an agent stops is correct: once nothing is working on a resource, the resource is free. So the daemon writes a **restore point** for each restorable agent immediately before its own shutdown stops them — the resources they hold, with the mode and note of each — and puts them back on restore, as new leases with fresh expiries. After a crash there is no restore point and none is needed: nothing released anything, so the lease table is still the truth. A restore point older than twelve hours is ignored, because a daemon that has been down for half a day is not resuming a session, and re-taking a stale lease would be claiming a resource for work nobody is doing.

What the agent is told arrives as a `restored` message, so it reaches the agent by whatever route its runtime already reads — the inbox, a hook's injected context, `wait_for_messages`. It carries the checkpoint it last saved and the next steps it recorded, how many observations its read set holds and **which of those paths changed while it was down**, the leases it holds and which of them had to be put back, and where its journal reading had got to. That is the difference from restoring a multiplexer's layout: a restored agent resumes with evidence, not with a directory name. Every restore is announced as `agent_restored`, carrying how many paths went stale.

What is still not restored is the terminal. A `--tty` agent comes back with a new one and an empty scrollback; the old master descriptor died with the old daemon. That is row 28.

Row 28, **live daemon replacement**, remains blocked. Actual binary tests at integrated source `63c6fbf66dbd2668acda2da138f64744f66ad864` reproduced `reload` returning success, the old daemon exiting, and both a batch and a PTY agent dying before their next instruction. The earlier in-process test kept the Tokio runtime alive and did not test that boundary. The unsafe exit path has been removed; requests now return `unavailable` without mutation.

A complete replacement must preserve child ownership, batch stdout/stderr, PTY input/output and scrollback, append logging, process identities and protection. It must quiesce writes, select the intended installed binary, validate compatibility, and receive successor readiness before retiring the predecessor, with bounded failure recovery. Descriptor transfer alone proves none of these properties. The host's `SCM_RIGHTS` helper remains a mechanism, not delivery evidence.

#### The app's terminal and command bar

A window that can only look at things is half a product: everything the CLI can do has to be reachable from it. Two different needs, and they want different answers.

**Attaching** uses the managed agent's PTY and the same `attach` protocol as the CLI. The app renders its output through a VT parser and sends keystrokes and resizes back in order. Its input queue admits at most 32 messages and 64 KiB in total; a single input must also fit 64 KiB. Admission accepts the whole input or none, and rejection remains visible until dismissed. Adjacent resizes coalesce without crossing a keystroke. A rejected resize is deferred until a later UI pass; keystrokes are never automatically retried.

The terminal reader limits each newline-delimited response to 256 KiB, including the daemon's encoded 64 KiB replay. OSC control strings are separately limited to 64 KiB across responses: [vte's generic OSC bound](https://docs.rs/vte/0.15.0/vte/struct.Parser.html) does not apply with its default `std` feature. Accepted bytes reach the VT parser unchanged; malformed, incomplete or oversized output ends the attachment with a reason. These are attachment budgets, not a total process RSS ceiling or deletion of daemon logs.

Closing remembers cancellation before a connection arrives, shuts down an installed socket and wakes the input worker without idle polling. Reader completion and writer failure use the same shutdown path; the background reader joins its writer while the UI remains responsive. A pending OS connection/open still inherits the client's transport and startup behavior; this is not a new deadline for a saturated listener or daemon autostart.

**Other commands** run through the console, which invokes `agentdocker` and renders its result. Dedicated controls cover common actions such as stopping or adopting an agent, setup, installation maintenance and answers to questions. Console output and command recall have separate history budgets.

What the app must not become is a multiplexer: panes, layouts, and tiling are herdr's and tmux's ground, and building them here would spend our effort on their strength rather than ours.

#### Terminal multiplexers

We should not write one. `tmux` exists, herdr exists, and a multiplexer is not the working set. What is worth having is an adapter — row 25 — and its first half is done: an agent living in a `tmux` pane, a `screen` window, a `zellij` session or a herdr session is recognised as such, and that is recorded beside its record, shown in `ps` and `discover`, and returned by `inspect`. That makes AgentDocker composable with whatever owns the terminal instead of competing for it: a person reaches the agent with the tool that already has it.

**How it is known, and why that differs by platform.** A multiplexer tells its children who they are through the environment — `TMUX`/`TMUX_PANE`, `STY`/`WINDOW`, `ZELLIJ_SESSION_NAME`/`ZELLIJ_PANE_ID`, herdr's own — which is exact and names the pane. Reading it is where the platforms part. On Linux `/proc/<pid>/environ` is readable for the caller's own user, so the daemon can look for itself. **On macOS it is not**: measured on 26.5.1, `ps -E` returns only the command line for a process other than the caller — even one owned by the same user — so a daemon that only looked would find nothing on the platform we ship first. (What a *privileged* caller sees was not tested and is not relied on.) The answer is to have it reported first-hand instead — a client registering itself is running *inside* the session, so `register` carries what the client read of its own environment, and the daemon prefers what it can read itself, then what was reported, then ancestry. Ancestry — a `tmux` or `zellij` process between the agent and its shell — is the last resort: true, but it cannot name the pane, so it is recorded as `evidence: ancestry` rather than dressed up as the real thing.

Verified against a real `screen` session on macOS: registering from inside one records `{kind: screen, session: "83350.agentdocker-test", pane: "0", evidence: environment}` and `ps` shows `screen:0`.

**`run --in-pane`** is the other half, and it is done. The daemon asks `tmux` for a new detached session named after the agent, hands the child `AGENTDOCKER_SOCKET`, `AGENTDOCKER_AGENT_ID` and `AGENTDOCKER_AGENT_NAME` through `tmux new-session -e`, reads the pane id it printed and the pane's pid, and **registers** what tmux started. A person then reaches it with `tmux attach -t <name>`.

Registered, not supervised — that division is the whole design, and it decides everything else about the path. tmux owns the process, so there is no captured log (the output is on tmux's terminal, and `tmux capture-pane` is where it lives), the agent ends when its command ends and the ordinary liveness sweep notices, and `stop` signals the pid tmux reported. What the agent does get is everything that matters: an identity, the project, leases, a read set, a journal cursor, and the pane on its record so `ps` says where to find it. `--in-pane` needs a `workdir` — tmux has to be told where to start — and tmux 3.2 or newer, since `new-session -e` is how the agent is handed its own id; both are checked before anything is created, so "tmux is too old" is an answer rather than an agent that started and failed. It refuses to combine with `--tty` (two terminals for one agent, and tmux is providing the one), with `--restore` (a daemon that did not start it cannot bring it back), or with `--image-build` (a container is started by the engine, so there is nothing for tmux to own). The CLI refuses all three at the argument level and the daemon refuses them again, because MCP, an `Agentfile.toml` and the protocol all bypass the CLI.

#### Token-lean output

Everything an agent reads from us costs it input tokens, and an agent reads `ps`, `journal`, `stale` and `channels` many times per session. [rtk](https://github.com/rtk-ai/rtk) compresses *shell* output before an agent sees it, which is real but orthogonal: our MCP results never pass through a shell, so nothing outside AgentDocker can shrink them. Measured on one developer machine, a single agent record is 709 bytes pretty-printed, 577 compact, and 418 carrying only the fields an agent uses.

Row 26 therefore makes our own output lean, and it is done: MCP tool results are compact JSON rather than pretty-printed, and `whoami`, `inspect_agent`, `list_agents`, `list_leases` and `list_channels` answer with a projection — for an agent, its id, name, runtime, status, project and branch, not the pid, process group, host and four timestamps the daemon keeps for itself — with `verbose: true` to opt back into the whole record. Absent fields are omitted rather than sent as `null`. The CLI keeps its tables, which are for humans. Where we run somebody else's command and rtk is installed, `logs --compress` and `validation <id> --compress` offer a compressed *view* of a retained log — never a compressed log, because a validation log is evidence and evidence is kept whole. The file on disk is not touched, and a host without rtk, or an rtk that fails, gets the whole log and a line saying why.

#### Derived activity

Recognising a herdr session is row 25, above. Herdr marks every pane working, blocked, or idle. That is the right question and we answer it better, because we know *why*: an agent waiting on a claim is blocked **on a named resource, held by a named agent**; an agent that has not acted through the daemon for two minutes is idle; an agent changing a file under a lease it holds is working. Row 24 derives that from the working set instead of guessing at terminal output, and it is what `ps`, `activity` and the desktop app show beside each agent.

### Phase 6 — Windows and federation

Windows needs named pipes in place of the Unix socket, a Windows service in place of launchd/systemd, and process inspection without `ps`; the watcher (`notify`) already works there. It is scheduled after the desktop app so the app ships on macOS and Linux first.


`agentd` instances discover and authenticate each other (mTLS, or a WireGuard-style keypair exchange). Agent ids become `host/agent`; messages, leases, events, and the journal route across peers; the registry becomes a replicated view. Project fingerprints are what make "the same repository on my laptop and in the cloud" one project, and handoff bundles are what move work between them. The core primitives do not change — which is the reason for keeping them pure and host-agnostic now.

### Delivery order

Each PR changes `protocol.rs`, the wire-protocol table above, the CLI, and tests together, per `CLAUDE.md`. Adding a table is not a `SCHEMA_VERSION` bump (`CREATE TABLE IF NOT EXISTS`); changing what a stored row means is.

| # | PR | phase | depends on |
|---|---|---|---|
| 1 | ✅ `crates/host` with project discovery; `register` defaults `workdir`; `project` on records; `ps` grouping, `--project`, `list {project?, labels?}`; `projects` cache table | 2 | — |
| 2 | ✅ `project:` destination; hooks orient by project | 2 | 1 |
| 3 | ✅ canonical physical `path:` lease keys with validated `file:` input aliases | 2 | 1 |
| 4 | ✅ service/lazy start, release archives and installer, and a maintained Homebrew tap carrying both the formula and the application cask; every tagged release publishes to it, and a tap that will not take the formula warns rather than failing the release | 2 | — |
| 5 | ✅ `discover` / `adopt`; dimmed rows in `ps` | 2 | 1 |
| 6 | ✅ `report` request with `vcs`; `BRANCH`/`HEAD` in `ps` | 2 | 1 |
| 7 | ✅ project watcher over every checkout of a project — the main one and each linked worktree, capped at 32 extra per project with a `watcher_gap` when the cap bites — ledger (`changes` table, `changes`, `blame`), watcher-triggered branch refresh with a five-second polling fallback that also re-reads each checkout's HEAD | 3 | 3, 6 |
| 8 | ✅ durable content read sets (`observe`, `reads`, `stale`), notices, hook denial until reread | 3 | 7 |
| 9a | ✅ change journal: entries, schema with FTS, release barrier and same-transaction write path, join/leave/commit/note entries, ring cache, `journal` CLI, `release --summary`, MCP `summary` and `journal_note` | 3 | 7 |
| 9b | ✅ change journal: cursors seeded by name, digests with budgets, `SessionStart`/`UserPromptSubmit` injection, transcript-tail summaries on `Stop`, MCP `read_journal` | 3 | 9a |
| 10 | ✅ `run --isolate`, `worktree-diff`, `overlap`, and `commit`: an agent commits its checkout through the daemon, so the entry names the agent that asked and carries the message it wrote, instead of the watcher guessing afterwards from a HEAD that moved. The checkout is marked while the commit is in flight so the watcher does not also write its own | 4 | 7 |
| 11 | ✅ `handoff`, lease transfer, `export` / `import` | 4 | 9b, 10 |
| 12 | ✅ scoped tokens, Docker/Podman builds and supervision, authenticated workspaces, engine-volume relay and image-bound validation | 4 | 3 |
| 13 | ✅ FIFO wait queue with RAII places, pure deadlock search over the lease and wait tables, `error(deadlock)` with the cycle, `waiting` | 5 | — |
| 14 | ✅ human agent (`me`), `ask` / `answer` / `questions`, `watch --me`, MCP `ask_human`, desktop notifications, the app's Questions screen | 5 | 2 |
| 15 | ✅ admission policy: host and project files, a project narrowing and never widening, deny-beats-allow with allow lists as whitelists, path patterns canonicalised on load, bounded regular-file reads, errors keeping the last good rules or refusing admission on first load; quotas as `quota:<name>` with `--amount` on the lease primitive | 5 | 12 |
| 16 | ✅ restart policies (`no`/`always`/`on-failure[:n]`, backed off, cleared by `stop`, restarting under the same id), `depends_on` with ordering and a wait in `up`, and `agentdocker top` | 5 | — |
| 17 | federation | 6 | 11, 12, 20 |
| 18 | ✅ runtime inventory (`runtimes`), one-command `setup` per runtime, continuous discovery with `agent_discovered` / `agent_vanished`, `adopt --all` | 5 | 5 |
| 19 | ✅ native desktop app `agentdocker-ui` (Rust, egui, OS IPC), terminal/console/questions; desktop distribution and onboarding remain incomplete | 5 | 18 |
| 20 | Windows: named pipes, a Windows service, process inspection | 6 | 19 |
| 21 | ✅ channels: a room per collision or task, membership-routed messages (`channel:<id>`), `review` verdicts as the tie-break, opened from the ledger, closed when everyone leaves, pruned | 5 | 10 |
| 22 | ✅ contests: passing, provenance-matched `validate` evidence as the entry; a measure fixed before anyone starts, taken by the daemon where it can be; a declared noise floor, inside which the ranking refuses to decide and channel review settles it | 5 | 21, 14 |
| 23 | ✅ PTY-backed sessions: a terminal per managed agent so interactive runtimes work under `run`, `attach` and detach, window size, scrollback on attach | 5 | — |
| 27 | ✅ snapshot restore with transactional preparation, watcher/socket readiness and failed-launch cleanup: `run --restore` brings an agent back under its own id after a daemon restart, with its leases re-taken from a restore point and a `restored` brief naming its checkpoint, what it had read, what changed while it was down, and its journal cursor | 5 | 23 |
| 28 | ⏳ live daemon replacement: requests fail without mutation pending process/I/O transfer, successor readiness and actual upgrade acceptance | 5 | 23 |
| 24 | ✅ derived activity: working, idle, starting, finished, or blocked on a named resource held by named agents — from the working set, never from terminal output; `activity`, `ps` DOING, MCP `activity`, and the app's agent list | 5 | 13 |
| 25 | ✅ multiplexer adapters: `tmux`/`screen`/`zellij`/herdr sessions recognised from the environment (reported first-hand at registration, since macOS does not expose another process's environment) or from ancestry, recorded on the agent and shown in `ps`/`discover`; `run --in-pane` starts an agent in a new tmux session and registers what tmux started, so the human attaches with the tool that owns the terminal | 5 | 18, 23 |
| 29 | ✅ the app's terminal view over `attach` (vt100 screen, keys, colours, resize), plus a console that runs any `agentdocker` command and renders what it said; both draw on a chosen terminal palette, with text sizes and row density, kept in `ui.json` per home | 5 | 19, 23 |
| 26 | ✅ token-lean output: compact MCP results with projections and a `verbose` opt-in; `logs --compress` and `validation <id> --compress` pipe a copy of a retained log through rtk where it is installed, and fall back to the whole log with a reason where it is not. The retained log is never rewritten | 5 | — |

Priority is [PRODUCT-DIRECTION.md](PRODUCT-DIRECTION.md#delivery-order): verify restore/privacy through the staged trial, complete native packaging and onboarding, then deliver Linux desktop and native Windows parity. Policy/quotas (15), restart policy (16), `commit` (10) and the rtk view (26) have implementations requiring the integrated delivery audit. Live daemon replacement (28) remains blocked on process/I/O ownership and successor readiness. Windows (20) requires full native process, terminal, IPC, service and installer acceptance. Federation (17) follows a dependable single-host product.

### Planned protocol and event additions

Listed here so the wire-protocol table above stays a description of what exists. Handoff and scoped authentication are already implemented with the request shapes above; the older design of a token field on every host request was superseded by the separate authenticated endpoint.

| Request | Response | Phase |
|---|---|---|
| `report {…, reads?, writes?}` | `ok` (adds read and write sets to the existing request) | 3 |
| `diff {agent, stat?}` | `diff` | 4 |
| `commit {agent, message?, push?, pr?}` | `commit` | 4 |
| Additional execution adapters | capability-specific | 4 |

Shipped events include `policy_updated` (effective rules or load diagnostic changed), `policy_denied` (what was asked and which rule refused it), `agent_restarted` (a managed agent started again by its policy, with the attempt number), `contest_opened`, `contest_entered`, `contest_submitted`, `contest_closed`, `lease_waiting`, `lease_wait_ended`, `lease_deadlock`, `agent_restored` (a managed agent brought back after a daemon restart, with how many of its reads went stale), `container_updated` (durable container transitions), `image_built`, `file_changed` (ledger observations), `agent_stale` (stale-reader events), `journal_appended` and `journal_read`. The `file_changed` and `agent_stale` notifications are live-only (`seq:0`) and cannot be recovered through event replay. The inbox notification uses the separate message kind `stale`.

`lease_waiting`, `lease_wait_ended` and `lease_deadlock` are shipped with row 13. Error codes `Timeout` (`ask`) and `Deadlock` (`claim --wait`) are both shipped.

## Open questions

- Should topic messages ever queue? Durable subscriptions solve it, but require the daemon to know about an agent's interests when it is offline. The `project:` destination removes the most common reason to want this.
- Priority vs. fairness for contested leases: waiters are FIFO. Whether labels or policy should ever let a claim jump the queue, and whether deadlock victims should be chosen by priority rather than always being the newcomer, is deferred until there is usage to look at.
- Whether `from` should be verified for *unsandboxed* agents too. Per-agent tokens (Phase 4) settle it for sandboxed runtimes, where it matters; requiring them from local shells and hooks would cost ergonomics for little, so they stay optional until there is a reason.
- Read-set capacity and eviction: 5,000 marks per agent is a guess; measure a long Claude Code session before tuning.

What exists is described above; the contracts and hardening decisions behind it — delivery boundaries, content observations, durable recovery, the verification workstream, and the journal's event barrier — are recorded in [IMPLEMENTATION-NOTES.md](IMPLEMENTATION-NOTES.md).

Configuration references: [Codex MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli), [Codex state locations and CODEX_HOME](https://learn.chatgpt.com/docs/config-file/config-advanced), [Claude Desktop local MCP configuration](https://py.sdk.modelcontextprotocol.io/get-started/real-host/). Setup respects an explicit CODEX_HOME for the calling host; inventory uses the daemon's configuration environment. Model and provider details are not inferred from an installed app or process name.


Lease admission and renewal commit the holder's liveness, lease row and ordered replay event atomically. Conflict replies commit their liveness and initial conflict event together. Planning does not mutate the live lease table; memory and publication advance only after commit. Release deletion, journal and replay commit before memory protection is removed. Storage failure freezes coordination and retains its prior protection; a failed liveness write does not make an agent appear more recently active in memory.
