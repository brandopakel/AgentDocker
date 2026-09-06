# AgentDocker architecture

This document describes what exists today precisely, and the later phases at the level of design intent. When the two disagree, the code wins and this document has a bug.

The [product direction](PRODUCT-DIRECTION.md) sets the next delivery priorities: native local orchestration, automatic discovery and setup, an installed desktop GUI, and macOS/Linux/Windows support. Container engines are optional execution adapters. The historical phase order below does not make container expansion or a browser dashboard prerequisites for that desktop product.

## Goals

1. **Universal.** Any agent — any model, any vendor, any runtime — can participate with nothing more than the ability to write JSON to a socket. No SDK is required, though one may exist for convenience.
2. **Safe by default.** Nothing an agent can do to the daemon can wedge another agent. Every claim expires; every exit releases.
3. **Familiar.** If you know Docker's mental model and CLI, you know AgentDocker's.
4. **Local first.** One host works with no network, no accounts, no cloud. Federation is layered on top later, not baked into the core.
5. **Observant.** The daemon derives an agent's state — what it read, holds, changed, and which branch it is on — from what hooks and adapters report, and never asks an agent to describe itself. Everything past Phase 1 is a derivation of that working set; see [The thesis](#the-thesis).

## Components

### `agentdocker-core` (`crates/core`)

Pure types and pure logic. It has no I/O, no async, and no access to a clock: every operation that depends on time takes `now: DateTime<Utc>` as an argument. This is what makes the coordination logic deterministic and cheaply testable.

| Module | Contents |
|---|---|
| `agent` | `AgentId`, `AgentSpec` (the "image"), `AgentStatus`, `AgentRecord` (the "container") |
| `message` | `Envelope`, `Destination` (agent / topic / broadcast), `MessageId`, `topic_matches` |
| `lease` | `ResourceKey`, `LeaseMode`, `Lease`, `LeaseTable` — the claim/renew/release/expire state machine |
| `registry` | `Registry` — the agent table, name uniqueness, id/name/prefix resolution |
| `event` | `Event`, `EventKind` — everything the daemon announces |
| `protocol` | `Request`, `Response`, `ErrorCode` — the wire format |
| `project` | `ProjectRef`, `ProjectId`, `ProjectSource` — the project an agent works in and how it becomes an id |
| `paths` | Where the socket and data live |

### `agentdocker-host` (`crates/host`)

The I/O that both binaries need but that does not belong in the daemon's state: project discovery from a working directory (`project::discover`, `project::fingerprint`) and process inspection (`procinfo::start_time`). Stateless: every function answers a question about the host as it is right now.

### `agentd` (`crates/agentd`)

One process per host. It is a library crate whose `main` the `agentdocker` package wraps as the `agentd` binary, so one `cargo install agentdocker` ships both. It owns:

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

A failed SQLite write latches `storage_unavailable`: the triggering request receives an error, subsequent coordination requests are refused, and events/messages are not published from the failed projection. `shutdown` remains available. Restart after repairing storage reloads the last durable state. This deliberately keeps the failed in-memory projection unavailable instead of trying to undo already-performed host effects. A multi-write operation can have committed a prefix before failing; clients must inspect/reconcile after restart rather than assume the whole request rolled back. A claim is never acknowledged after a detected write failure, and failed releases cannot admit a conflicting writer. Recovery IDs provide stronger idempotency where supported.

Reads are served from memory; every mutation is written through to SQLite (`rusqlite`, bundled, WAL mode) before the response goes out. Rows are JSON blobs of the core types beside the few columns needed for lookups (`agents`, `leases`, `inbox`, `events`, `changes`, `journal` with its `journal_paths` and `journal_fts` indexes and `journal_cursors`; `projects` is the one plain table, a cache of fingerprints per repository root), so adding a field to a core type is not a migration. A `meta.schema_version` row guards against opening a database written by an incompatible build. Schema 7 upgrades schemas 1 through 6 on open and idempotently translates old `file:` leases using each holder's recorded checkout. Older daemons refuse schema 7, which preserves image-bound validation, runner deadlines, and container lifetime independently of host PIDs; process-group tracking and `stopping` status remain supported.

On startup the daemon reloads agents, leases, and inboxes, tidying as it goes: a managed record still `created` (the old daemon died mid-spawn) is recorded as failed, a second live record with an already-live name is recorded as exited, and a lease whose holder is not live is dropped — each written back so the store and the registry agree. Leases keep their original expiry, so a restart never extends anyone's claim.

Agents that were live when the previous daemon stopped are *adopted*: the new daemon has no `Child` handle for them, so a once-per-second liveness check inspects every unsupervised live agent that reported a pid and records an exit (releasing its leases) when the process is gone. "Gone" means signal 0 fails, *or* the process now behind that pid started at a different time than the one that registered — the daemon records the process start time (macOS `proc_pidinfo`, Linux `/proc/<pid>/stat`) so a recycled pid, typically after a reboot, is not mistaken for the agent. The same check covers externally registered agents, which is what makes a Claude Code session that dies without deregistering harmless. An external agent that registered without a pid can only leave by deregistering; a managed agent still being spawned is skipped.

Lease deletion and its `lease_released` or `lease_expired` event commit in one transaction, including startup cleanup, explicit release, agent exit and expiration. A failed event write retains the durable lease; restart never repeats an already committed cleanup event.

Every event carries a strictly increasing `seq`, assigned by the daemon and continued across restarts from the stored history. Events are appended to the store as they are emitted and trimmed to the newest 10,000 once a minute; `agentdocker events --replay N` shows the last N before streaming, and the server drops any live event whose `seq` the replay already covered, so an event emitted while the stream was being set up is delivered once. A persistence failure disables coordination and fails the request as described above; no failed event is published live. Inbox acknowledgment deletes and its event commit in one transaction.

### `agentdocker` (`crates/cli`)

A thin client. Each invocation opens one connection, sends one request, and prints the response(s). It exists so humans and shell hooks can participate; it is not the only way in.

### Starting the daemon

Nobody has to start `agentd` by hand. A client that cannot connect — no socket file, or nothing listening — starts the daemon itself, the way `ssh-agent` and `buildkitd` are started by their clients, then waits for the socket (3 s for the CLI and the MCP server, 1 s for the entire hook operation, which fails open past that). `AGENTDOCKER_NO_AUTOSTART=1` turns this off. The daemon it starts is the `agentd` beside the client's own binary when there is one, so a build in `target/` starts the matching daemon, else `agentd` on `PATH`; it runs in its own process group with stdout and stderr appended to `<home>/agentd.log`.

Exactly one daemon serves a socket, guaranteed by an advisory lock beside it (`agentd.sock` → `agentd.lock`). The daemon takes the lock for its lifetime before touching the socket, and exits at once, successfully, if it cannot. A client decides whether to spawn by taking the same lock for an instant: getting it means no daemon exists; not getting it means one is up or starting, so the client only waits. Two clients racing may both spawn a daemon, and the loser exits on the lock. The daemon's stale-socket check (remove the file if nothing answers on it) stays as a second line of defence.

**As a service.** On-demand start is enough for a laptop; `agentdocker daemon install` additionally runs `agentd` as a login service so it survives reboots and crashes and belongs to no terminal — a launchd agent (`~/Library/LaunchAgents/dev.agentdocker.agentd.plist`) on macOS, a systemd user unit (`~/.config/systemd/user/agentd.service`) on Linux. Both restart the daemon after a *failure* only, because a clean exit is what a service daemon does when an on-demand one already holds the lock; `install` therefore first asks any running daemon to exit (the `shutdown` request, which SIGTERMs managed agents exactly as Ctrl-C does) and then hands the socket to the service. `daemon uninstall`, `start`, `stop`, `restart`, and `status` do what they say, with `start` and `stop` falling back to the on-demand daemon when no service is installed; `--dry-run` on `install` and `uninstall` prints the files and commands instead. The service definition bakes in `--home` (and `--socket` when overridden) so it serves the same paths the CLI that installed it used. Files and command sequences are pure and unit-tested; only the final execution touches the system.

**Installing.** `cargo install agentdocker` builds both binaries; `install.sh` at the repository root downloads the release archive for the host (`agentdocker-<target>.tar.gz`, four targets: macOS and Linux musl on x86_64 and aarch64, named without the version so `releases/latest/download/…` works) and drops them into `~/.local/bin`; `packaging/homebrew/agentdocker.rb.in` is the template for a tap formula, with a `brew services` block that runs the daemon. The release workflow builds and uploads archives with SHA-256 checksums on every protected `v*` tag, then generates `agentdocker.rb` from all four verified checksum inputs. The installer requires a valid matching checksum before extracting or replacing anything. Workspace dependencies include versions so `cargo package --workspace` packages all four crates; actual crates.io publication and tap publication remain release operations.

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

**Watching.** One `notify` watcher (FSEvents on macOS, inotify on Linux) covers each distinct checkout — the main root or a linked worktree — of every live agent whose project is a repository or an `Agentfile.toml` root. Plain directories are not watched: a recursive watch on a home directory is exactly what inotify cannot afford. Watches are reconciled against the registry once a second, and at once when an agent registers — `register` and `run` wait, bounded at 500 ms, for the watcher to cover the new checkout before replying — including while the watcher is still starting — so a session's first edit is never made in the gap before the next tick — and an agent leaving needs no hook. Raw events are debounced for 100 ms and duplicates within a batch collapse; each path is filtered through the checkout's `.gitignore` so `target/` and `node_modules/` never reach the ledger; directories are skipped; and `.git/` is ignored except the files that say where HEAD is (`HEAD`, `refs/heads/**`, `packed-refs`, a worktree's `HEAD`), which trigger a branch re-read for the agents in that checkout instead of an entry. A linked worktree's own git directory, which lives under the main root, is watched too so its `HEAD` is seen.

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
| `integrate {agent, source, validation, apply?}` | `integration {source_head, applied, clean, text}` | validated source; apply leaves merge uncommitted and target lease held |
| `grant_access {agent, container_root, ttl_secs?}` | `access {grant, token, socket, expires_at}` | host-only; TTL 1–86400 seconds, default 3600; CLI writes token privately and prints grant ID |
| `revoke_access {grant}` | `ok` | host-only; deny new requests, preserve leases |
| `authenticate {token}` | `ok` | restricted endpoint only; precedes one scoped request |
| `ping` | `pong` | version, uptime, restricted endpoint while serving |
| `build_image {spec: {engine, connection?, context, recipe, timeout_secs?}}` | `image_build {build}` | host-only Docker/Podman build from captured inputs; timeout defaults to 600 seconds, valid range 1–3600; immutable image ID and atomic provenance/event |
| `images` | `image_builds {builds}` | retained build evidence, including after restart |
| `run {spec}` | `agent` | spawns `spec.command`; child gets `AGENTDOCKER_SOCKET`, `AGENTDOCKER_AGENT_ID`, `AGENTDOCKER_AGENT_NAME` |
| `run_container {spec, build, options?}` | `agent` | host-only; retained image with durable identity/intent; opt-in checkout/scoped endpoint mounts, Podman VM transport, and bridge networking |
| `restart_container {agent}` | `agent` | host-only; new identity from same build after confirmed exit; `conflict` while exit is uncertain |
| `register {spec, pid?}` | `agent` | external process; PID must be positive and fit i32; `spec.workdir` decides the project |
| `deregister {agent}` | `agent` | marks an external agent exited |
| `discover` | `processes` | running processes of known agent runtimes that no live agent claims by pid |
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
| `overlap {project, since_seq?, agent?}` | `overlap {overlaps: Overlap[]}` | paths changed in more than one physical checkout of the project, from the newest 50,000 ledger rows: per path, each checkout with the agents attributed there, the count, the last change and its HEAD; with `agent`, only overlaps involving its checkout, and an empty `project` means its own |
| `changes {project, since_seq?, path?, agent?, limit?}` | `changes {changes: Change[]}` | the ledger, newest `limit` entries oldest first; `since_seq` is exclusive (`seq > since_seq`); `limit` defaults to 50 and is clamped to 1–10,000; empty, `.` and absolute checkout-root paths select all paths |
| `shutdown` | `ok` | the daemon exits after replying; managed agents get SIGTERM, as on Ctrl-C |
| `send {from, to, kind, payload, reply_to?}` | `sent` | `to` is an agent ref, `project:<id prefix or absolute path>`, `topic:<name>`, or `all` |
| `subscribe {agent?, topics?}` | stream of `message` or `lagged {skipped: u64}` | flushes the inbox first, then live until the client disconnects |
| `inbox {agent, drain?}` | `messages` | |
| `ack_inbox {agent, messages: MessageId[]}` | `ok` | idempotently acknowledge specific delivered messages; emits `inbox_acknowledged` |
| `claim {agent, resource, mode?, ttl_secs?, note?, wait_secs?}` | `lease` or `error(conflict)` | `path:` uses canonical physical absolute keys; `file:` is a validated checkout alias; conflict `details.held_by` lists the blocking leases; `wait_secs` (max 600) retries until the conflict clears |
| `renew {agent, lease, ttl_secs?}` | `lease` | responses may include `change_seq`, the durable acquisition boundary; absent on legacy leases |
| `release {agent, lease, summary?, summary_source?}` | `lease` | holder only; `summary` becomes the journal entry's text; `summary_source` is `explicit` (default) or `transcript` |
| `release_all {agent, summary?, summary_source?}` | `leases` | every lease the agent holds; the reply lists them |
| `journal_add {agent, summary}` | `journal_entry` | a note in the agent's project journal |
| `journal {project, since_seq?, until_seq?, agent?, branch?, kind?, path?, grep?, limit?, digest?}` | `journal` or `digest` | newest `limit` entries oldest first, with the project id; with `digest {reader, max_entries, max_chars, all_branches?, advance?}` the reader's digest since its cursor instead (empty `project` = the reader's own) |
| `journal_prune {project, before_seq}` | `pruned` | drops entries below `before_seq` |
| `leases {agent?, resource?}` | `leases` | `resource` filter uses overlap, not equality; `file:` inputs resolve through the same physical checkout alias |
| `events {replay?,ready?}` | optional `events_ready`, then stream of `event` or `lagged {skipped: u64}` | replays the last `replay` stored events, then live until the client disconnects |
| `logs {agent, follow?, tail?}` | stream of `log`, then `end` | containers: verified engine snapshot, max 10,000 lines/4 MiB, no follow |

Any agent reference (`agent`, `from`, `to`) accepts a full id, a unique id prefix, or a name. Names resolve to the live agent with that name, or failing that to the most recently created finished one (so `logs` works after exit).

Errors: `{"type":"error","code":"conflict|not_found|ambiguous|name_taken|forbidden|invalid|storage_unavailable|engine_unavailable|build_failed|unavailable|internal","message":"...","details":{...}?}`.

## Leases

A **resource key** is `kind:value`. The daemon interprets two kinds, `path` and `file`, which overlap hierarchically — `path:/repo/src` overlaps `path:/repo/src/lib.rs` and `path:/repo` — so claiming a directory protects everything under it. Every other kind (`branch:`, `task:`, `db:`, or anything you invent) overlaps only on exact match.

**Physical protection and logical overlap.** Write leases use canonical absolute `path:` keys independently of the holder's project. A directory claim covers its physical descendants, including files claimed by agents outside the project. Canonicalization resolves existing symlinks and normalizes missing suffixes, including `..`. An explicit `file:<project id>/<relative path>` input is an alias only when `agent` identifies a matching checkout; unsafe relative paths are rejected. Queries use the same normalization. Linked worktrees and clones can edit independently because they are different physical checkouts. Project id plus relative path describes logical overlap for the Phase 3 ledger and Phase 4 integration; it is not a second write-lock namespace. Containers use an authenticated mount mapping before their paths participate.

Two **modes**: `exclusive` conflicts with any lease on an overlapping resource held by someone else; `shared` conflicts only with exclusive leases held by someone else. An agent never conflicts with itself, and re-claiming a resource you hold in the same mode renews it instead of failing.

**TTL.** Every lease expires. The default is 300 s, the cap is 24 h. Long-running work should `renew` periodically rather than ask for a long TTL: a TTL is a liveness bound, not a reservation. A reaper runs every second; `leases` also expires before listing so its output is never stale.

**Exit.** Process-backed `stop` validates the PID and recorded process identity, sends the signal and records `stopping`. It retains leases until the supervisor or liveness check observes exit; force stop follows the same observation rule. New claims/renewals require `running`. An external agent may explicitly deregister; managed agents finish through supervision and cannot deregister themselves. PID zero and values beyond the positive signed PID range are rejected. Exit releases held leases once.

**Conflicts are informative.** A refused claim returns every blocking lease including its holder, mode, expiry, and note, and emits a `lease_conflict` event. Agents are expected to read the note, message the holder, or wait.

**Waiting.** `claim` with `wait_secs > 0` subscribes to the event stream *before* its first attempt, and on conflict waits for a `lease_released` or `lease_expired` event on an overlapping resource (or the deadline) before trying again. One `lease_conflict` event is emitted per request no matter how long it waits. Closing a waiting connection cancels its request; liveness is checked under the state lock before every acquisition. Claim and renew expiration effects are persisted and announced using the same timestamp as the core operation. Waiters are not queued: when a lease clears, every waiter retries and the lease table decides, so two agents waiting on the same resource race. FIFO fairness is an open question below.

## Messaging

An **envelope** carries `from` (an agent id, or `user` for CLI-injected messages), `to`, a free-form `kind`, a JSON `payload`, an optional `reply_to`, and a timestamp. The daemon routes; it does not interpret `kind` or `payload`.

Four destinations:

- **Agent** — one recipient, resolved by id/prefix/name before publishing.
- **Project** — every live agent in a project except the sender. `project:<selector>` takes an id (any unique prefix) or an absolute path inside the project; the CLI and MCP server turn a bare `project` into the caller's current directory, so `send --to project` needs no ids at all.
- **Topic** — a `/`-separated path like `repo/backend/reviews`. Subscribers give MQTT-style patterns: `+` matches one level, `#` matches the rest.
- **Broadcast** — every live agent except the sender.

**Delivery.** A message is pushed to every live subscription whose filter matches (a project delivery matches subscribers whose agent was in that project when it subscribed). For agent, project, and broadcast destinations, each recipient *without* a live subscription gets the message queued in its inbox instead. Topic messages are live-only; whether they should ever queue is an [open question](#open-questions). When an agent opens a subscription its inbox is flushed into the stream first; a message that lands in the tiny window between "subscribed to the bus" and "inbox drained" is suppressed by id so it is not shown twice.

Guarantees, stated plainly: live delivery is at-most-once (a slow subscriber that falls more than 1024 messages behind is told it lagged and skips); inboxes survive daemon restart, but drain and subscription handover remove queued messages before transport acknowledgement, so a broken connection can lose that delivery. Reliable handoffs and questions require the acknowledgement protocol planned below. A `lagged {skipped}` response explicitly reports skipped live items. The CLI warns and continues for messages; event streams exit with an error directing the caller to recover retained history.

## Events

`agent_id`, `project_ref`, `container_updated`, `image_built`, `worktree_created`, `worktree_cleanup`, `integration_prepared`, `access_granted`, `access_revoked`, `checkpoint_saved`, `handoff_accepted`, `handoff_sent`, `handoff_imported`, `lease_transferred`, `validation_started`, `validation_finished`, `watcher_gap`, `watcher_starting`, `watcher_started`, `watcher_unavailable`, `restricted_endpoint_listening`, `restricted_endpoint_unavailable`, `reads_observed`, `inbox_acknowledged`, `agent_created`, `agent_started`, `agent_stopping`, `agent_exited`, `agent_removed`, `message_sent`, `lease_claimed`, `lease_renewed`, `lease_released`, `lease_expired`, `lease_conflict`, `project_discovered`, `journal_appended`, `journal_read`, `agent_vcs_changed`, `daemon_stopping`. Each carries a timestamp and enough data to be actionable on its own (a lease event carries the whole lease). `agentdocker events` streams them; dashboards and policy engines will consume the same stream.

`file_changed` and `agent_stale` are also emitted on the live event stream with `seq:0`; they are not persisted in ordered event history. `changes` reads retained ledger observations, and `stale` checks current content directly after a missed live notification.

## Process supervision

`run` spawns the command with stdin closed and stdout/stderr piped into a log writer that prefixes each line with an ISO timestamp and `[out]`/`[err]`. The child inherits the daemon's environment plus `spec.env`. It is deliberately *not* given the CLI caller's environment, so secrets don't silently travel through the registry; pass what the agent needs with `-e`. On daemon shutdown every managed agent receives SIGTERM.

## Security model

The host control socket is mode `0600` and trusts the owning user. The separate `container.sock` also has mode `0600` but requires a scoped token: first `authenticate`, then exactly one operation, then close. Frames are bounded to 1 MiB and connections time out after 30 seconds. Tokens are stored hashed, scoped to one running agent and physical checkout, and checked for revocation/expiry on every operation. Only mapped path claims/reads, own inbox/lease operations, inspection and direct project-peer messaging are allowed. Host process control, validation execution and credential administration are unavailable. Missing tokens never fall back to host authority. Existing leases survive revocation until normal release/expiry/observed exit. Engine sockets and the host control socket are never container mounts.

## Roadmap

The [product direction](PRODUCT-DIRECTION.md) defines current delivery priorities. The phases below retain the detailed engineering design; numbered delivery rows are not GitHub PR numbers.

Phases 0–2, read tracking, durable recovery, explicit worktree integration and scoped container transport are implemented in the feature stack; merge and public release status are tracked in GitHub. Engine-managed build/launch, authenticated workspace mounts, managed Podman VM transport and image-bound validation provenance are implemented in the container stack. Docker Desktop uses the engine-volume socket relay; actual Desktop verification is tracked separately from Linux engine tests. Unimplemented items in Phases 4–6 remain design intent, written at the level of detail needed to build it — data model, protocol, storage, CLI, events, and what "done" means — so that each item can become a PR without a second design pass. Phases are ordered by dependency, not importance; [Delivery order](#delivery-order) lists the PR sequence.

### The thesis

Docker's moat was a layered filesystem plus namespaces: the daemon knew exactly what a container could see and change. AgentDocker's equivalent is the **working set**. For every agent the daemon observes the paths it read, the resources it holds, the paths it changed, the branch it is on, and the messages it exchanged. Nothing asks an agent to describe its own state: hooks and adapters report what happened, and the daemon derives the rest. From the working set the daemon can do what no single agent can — group agents by project, tell an agent that something it read has since moved, attribute every change to whoever made it, detect two agents waiting on each other, and package a handoff. That is the proprietary layer; each item in Phases 2–5 is one of those derivations. The rule for new features: prefer deriving from what the daemon already sees over asking agents to declare it.

### Phase 1 — adapters & persistence *(done)*

[Persistence](#persistence), [`agentdocker mcp`](#agentdocker-mcp-cratesclisrcmcprs), [`agentdocker hook`](#agentdocker-hook-cratesclisrchooksrs), [`Agentfile.toml`](#agentfiletoml-and-agentdocker-up--down-cratesclisrcagentfilers-teamsrs), and `claim --wait` all exist. Two things the original design called for are deliberately deferred: a FIFO wait queue (today's waiters race when a lease clears; see [Wait queue and deadlock detection](#wait-queue-and-deadlock-detection), which needs the queue anyway) and a daemon-side notion of a team (the Agentfile is a client convenience; `list {labels?}` arrives with project filtering in Phase 2 so `ps --team` can be sugar over labels).

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

What exists is described under [The journal](#the-journal); the rest of this section is the settled design it follows.

A per-project, append-only narrative of what changed and why: coarse where the ledger is fine-grained, readable by models and humans, cheap to read incrementally, and the thing a newcomer is handed instead of the event stream. The design below was settled decision by decision on 2026-09-04 (the list is at the end) and is ready to build.

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

The shared engine interface will cover availability/capability discovery, image build and inspection, container create/start/inspect/stop/remove, logs, and wait. Implementations invoke the selected engine with structured arguments. Engine selection is explicit and persisted; a failed engine must never silently switch to another engine or the host. Record the engine, container ID, resolved image ID/digest and platform with the agent. A client process exiting does not prove the container stopped: engine inspection must establish termination before releasing its protection. An unavailable engine leaves status uncertain and protection governed by existing lease TTLs.

Build support uses a common Dockerfile/Containerfile and explicit context, with per-engine handling for unsupported features. Podman accepts both formats, but its `buildx` compatibility does not cover all Docker Buildx features ([Podman build reference](https://docs.podman.io/en/latest/markdown/podman-build.1.html)). Build provenance must record the source content identity, build recipe, engine/version, target platform, and resulting immutable image identity. Build success is distinct from test success; validation evidence also needs the image identity and command before it can be reused across container sessions.

The host control socket is never mounted in a container. `grant-access` creates an expiring credential scoped to one running agent and one physical checkout, writes the secret to a private token file, and supplies only `container.sock`. The CLI uses `AGENTDOCKER_TOKEN_FILE`; the endpoint requires authentication and rechecks identity, expiry and revocation for each operation. Container `/work` paths translate to canonical host paths before lease and read-set lookup. Token revocation denies new requests but does not free a running writer's leases. Runtime-native sandboxes may need their own transport configuration; hooks and MCP are not assumed to run outside every sandbox.

Engine adapters must handle rootless ownership and VM mount reachability explicitly. A host Unix socket is not presumed usable through a macOS VM bind mount. The VM bridge may expose only the authenticated endpoint, with a tested path mapping; no privileged engine socket is made available to an agent. Default container policy is no network, no inherited host environment, no engine socket, and only the selected checkout plus the scoped endpoint and token mounts. Network access and additional mounts are explicit configuration. Worktrees isolate edits; container engines provide the process/filesystem boundary.

#### Handoff bundles *(done)*

An agent handing work to another should not have to write its state down; the daemon already knows it. A handoff is a checkpoint addressed to someone. `handoff {agent, to?, task?, note?, transfer_leases?, key?}` makes the checkpoint through the checkpoint path — same release barrier, same idempotency by key, leases released with it unless they are to move — then assembles a `HandoffBundle { schema, id, from, from_name, to, project, task, note, assumptions, next_steps, checkout, version (the checkpoint's content identity), environment (image and execution provenance), vcs, leases (what the sender held when the bundle was made), transfer_leases, read_set, changes (the sender's ledger rows since it joined, at most 1,000), diff (for a sender in a linked worktree: the tracked patch, cut at 64 KB on a line boundary with the worktree named), unread_inbox, journal (the sender's own entries), journal_cursor, created_at, imported_at }`, stores it as a document under the checkpoint's id with `handoff_sent`, sends `to` a message `kind: handoff` whose payload names the id and the task, and appends a `handoff` journal entry ("codex-1 handed off to gemini-2: finish the parser"). Retrying with the same key returns the same bundle. Acceptance is `resume {…, acknowledge: true}` by the addressee — anyone else is refused — after the usual content verification, and it commits ownership in one transaction: with `transfer_leases` the still-live leases listed in the bundle that the sender still holds move to the recipient (`LeaseTable::transfer_selected`, one `lease_transferred {lease, from, to}` each), the read set is seeded from the bundle so staleness carries over, and the recipient's journal cursor is set to the sender's so it continues reading where the sender stopped. A bundle nobody was addressed to (`agentdocker export --as <agent> > bundle.json`) is the same structure; `agentdocker import --as <agent> < bundle.json` on another host re-homes it — the checkout becomes the importer's, read marks move with it, leases and the cursor stay behind — stores it with `handoff_imported`, and tells the importer; `resume` then accepts it only if the content identity matches exactly, as ever. The bundle carries schema 2; older importers must refuse it rather than drop image evidence. `agentdocker handoff <to> --as <from> --task "…" [--note …] [--transfer-leases]`, `agentdocker handoffs`, and the MCP `handoff` and `list_handoffs` tools.

### Phase 5 — the machine and the human

#### Runtime inventory, setup, and continuous discovery

`agentdocker runtimes` lists the agent tools on this machine: for each known runtime (`claude-code`, `codex`, `gemini-cli`, `cursor`, `aider`, `goose`, `copilot`, `amp`, `opencode`) whether its CLI is on `PATH` and which version, its desktop app where one exists (Claude, Cursor, VS Code, Windsurf — by bundle on macOS, by binary on Linux), its config directory, and whether AgentDocker is wired in: hooks installed for Claude Code, the MCP server registered in Claude Code's `~/.claude.json`, Codex's `~/.codex/config.toml`, Gemini's `~/.gemini/settings.json`, or Cursor's `~/.cursor/mcp.json`. `agentdocker setup [<runtime> | --all] [--dry-run]` writes the missing registrations idempotently, keeping a backup of every file it touches, and prints what it changed. The inventory is host I/O in `agentdocker-host::runtimes`; the daemon serves it as `runtimes {}` so the desktop app and the CLI share one answer. The request, response and events are listed under [planned protocol additions](#planned-protocol-and-event-additions) until row 18 lands.

**Planned for roadmap row 18:** the daemon will run the process scan every five seconds, cache its last result for `discover`, and emit replayable `agent_discovered {pid, runtime, project?, cwd?}` and `agent_vanished {pid, runtime, adopted}` events when processes enter or leave discovery. The planned `adopt --all` command will register each discovered process. These are design intent until row 18 lands; automatic adoption remains a later policy decision.

#### Native desktop app

`agentdocker-ui` is a native window, not a web page: a Rust binary (`crates/ui`, egui/eframe) that talks to `agentd` over the same Unix socket as the CLI — a background thread for requests, one for the event stream — with nothing listening on HTTP. Screens: agents by project with status, branch, held leases and last activity; runtimes (installed, wired, running; adopt and set up from the app); the journal (per-project digest, follow); leases; events. It raises desktop notifications for messages addressed to the human and for stale-context warnings, which is the channel the human-as-agent item below delivers through. `agentdocker ui` launches it; it ships beside the CLI. Windows follows once the daemon runs there.

#### Wait queue and deadlock detection

`claim --wait` today is a retry loop inside the claim handler: on conflict the request waits for a release or expiry event on an overlapping resource and tries again, and when several requests wait on one resource they race. This item makes waiting a daemon-level fact and derives two things from it.

- **FIFO queue.** Core `WaitQueue` (pure) records waiting requests per resource in arrival order; when a lease clears, only the oldest waiter on an overlapping resource may take it, so a newcomer cannot starve someone already waiting. `lease_waiting {resource, requester, position}` is emitted when a request starts waiting and `lease_wait_timeout {resource, requester}` when it gives up (the response stays `error(conflict)` with the blocking leases, as now). Waiters remain connection-scoped and are never persisted: a daemon restart drops every waiting client, which reconnects and re-queues.
- **Deadlock detection.** Waiters form a graph: an edge from each waiter to every holder of a blocking lease. Core `WaitGraph` (pure, tested) maintains it and, on every new wait, searches for a cycle through the requester. If one exists the claim is refused immediately with `ErrorCode::Deadlock` and `details.cycle` listing the agents and resources, and `lease_deadlock {cycle}` is emitted; the newcomer is always the victim, which is deterministic and needs no priorities. TTLs already bound how long a deadlock can last; detection makes it instant and explains it. Priority-based victim selection stays an open question.

#### The human as an agent

Orchestration needs an escalation path, and it should live inside the same model rather than beside it. `agentdocker me` registers the human as a persistent agent named `user` with runtime `human` (the `from: user` convention already exists), never expired by liveness. `ask {from, to, question, timeout_secs}` sends `kind: question` and blocks the caller until an `answer` with a matching `reply_to` arrives or `ErrorCode::Timeout`; MCP exposes `ask_human`, hooks expose nothing (a model asks in prose). Delivery to the human: `agentdocker watch --me` streams questions, `agentdocker answer <message id> "…"` replies, and the daemon raises a desktop notification (`osascript` / `terminal-notifier` on macOS, `notify-send` on Linux) for anything addressed to `user`, throttled to one per sender per minute.

#### Admission policy and budgets

Like Docker's authorization plugins: a policy file the daemon consults before acting. `<home>/policy.toml` and, per project, `<root>/.agentdocker/policy.toml` (project rules cannot widen host rules). Rules match agents by runtime, labels, name glob, and project, and allow or deny actions expressed as patterns: `claim:path:/repo/migrations/**`, `send:project:*`, `run:*`. Evaluation is pure (`Policy::check(agent, action) -> Allow | Deny { rule, reason }` in core) and refusals use the existing `ErrorCode::Forbidden` with the rule in `details`; a `policy_denied {agent, action, rule}` event fires. Files are reloaded on change (the same watcher) and on `SIGHUP`.

Budgets ride the lease primitive as a quantitative resource kind: `quota:<name>` with a capacity set in policy, claimed in shared mode with `amount`, so `claim quota:tokens/<project> --amount 50000` fails once the sum of live amounts would exceed capacity. This folds quotas into a mechanism that already has TTLs, release-on-exit, and events; whether amounts belong on `Lease` or in a sibling `Quota` table is decided when the policy PR lands.

#### Supervision policy and dashboard

`--restart no | on-failure[:max] | always` on `run` and as `restart` in `Agentfile.toml`, `depends_on` (start after the dependency is running) and `after = "A exits 0"` (start after it succeeds), and `agentdocker top`, a TUI fed by the event stream showing agents by project, their branches, held leases, waiting claims, and the latest journal entries.

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
| 4 | ✅ `daemon install/uninstall/status`; lazy start; release workflow, tap, installer | 2 | — |
| 5 | ✅ `discover` / `adopt`; dimmed rows in `ps` | 2 | 1 |
| 6 | ✅ `report` request with `vcs`; `BRANCH`/`HEAD` in `ps` | 2 | 1 |
| 7 | ✅ project watcher, ledger (`changes` table, `changes`, `blame`), watcher-triggered branch refresh with a five-second polling fallback | 3 | 3, 6 |
| 8 | ✅ durable content read sets (`observe`, `reads`, `stale`), notices, hook denial until reread | 3 | 7 |
| 9a | ✅ change journal: entries, schema with FTS, release barrier and same-transaction write path, join/leave/commit/note entries, ring cache, `journal` CLI, `release --summary`, MCP `summary` and `journal_note` | 3 | 7 |
| 9b | ✅ change journal: cursors seeded by name, digests with budgets, `SessionStart`/`UserPromptSubmit` injection, transcript-tail summaries on `Stop`, MCP `read_journal` | 3 | 9a |
| 10 | ✅ `run --isolate`, `diff`, `commit`, `overlap` | 4 | 7 |
| 11 | ✅ `handoff`, lease transfer, `export` / `import` | 4 | 9b, 10 |
| 12 | 🔄 per-agent tokens ✅, Docker/Podman image builds ✅, container supervision ✅, authenticated workspaces ✅; engine-volume relay and image workspaces in review | 4 | 3 |
| 13 | FIFO wait queue, wait graph, deadlock detection | 5 | — |
| 14 | human agent, `ask` / `answer`, notifications | 5 | 2 |
| 15 | admission policy and quotas | 5 | 12 |
| 16 | restart policies, `depends_on`, `top` | 5 | — |
| 17 | federation | 6 | 11, 12, 20 |
| 18 | runtime inventory (`runtimes`), one-command `setup` per runtime, continuous discovery with `agent_discovered` / `agent_vanished`, `adopt --all` | 5 | 5 |
| 19 | native desktop app `agentdocker-ui` (Rust, egui, over the socket): agents, runtimes, journal, leases, events, notifications | 5 | 18 |
| 20 | Windows: named pipes, a Windows service, process inspection | 6 | 19 |

Order from here: 18, 19, a first tagged release so a second machine installs with the curl installer, then 13–16, 20, and 17.

### Planned protocol and event additions

Listed here so the wire-protocol table above stays a description of what exists.

| Request | Response | Phase |
|---|---|---|
| `claim {…}` | adds `error(deadlock)` | 5 |
| `report {…, reads?, writes?}` | `ok` (adds read and write sets to the existing request) | 3 |
| `diff {agent, stat?}` | `diff` | 4 |
| `commit {agent, message?, push?, pr?}` | `commit` | 4 |
| `handoff {from, to, task?, note?, transfer_leases?}` | `handoff` | 4 |
| `run` / `register` responses gain `token`; every request accepts `token?` | — | 4 |
| `ask {from, to, question, timeout_secs}` | `message` (the answer) or `error(timeout)` | 5 |
| `runtimes {}` | `runtimes {runtimes: RuntimeInfo[]}` — the agent tools on this machine, per runtime: CLI and version, desktop apps, config directory, MCP and hooks wiring, unregistered running processes | 5 |

Shipped events include `container_updated` (durable container transitions), `image_built`, `file_changed` (ledger observations), `agent_stale` (stale-reader events), `journal_appended` and `journal_read`. The `file_changed` and `agent_stale` notifications are live-only (`seq:0`) and cannot be recovered through event replay. The inbox notification uses the separate message kind `stale`.

Planned events: `agent_discovered {pid, runtime, project?, cwd?}` and `agent_vanished {pid, runtime, adopted}` (announced by the daemon's own process scan; stored and replayable like every other event; design intent until row 18 lands), `lease_waiting`, `lease_wait_timeout`, `lease_deadlock`, `policy_denied`. New error codes: `Deadlock` (Phase 5) and `Timeout` (for `ask`).

## Open questions

- Should topic messages ever queue? Durable subscriptions solve it, but require the daemon to know about an agent's interests when it is offline. The `project:` destination removes the most common reason to want this.
- Priority vs. fairness for contested leases: waiters race today, and Phase 5 makes them FIFO. Whether labels or policy should ever let a claim jump the queue, and whether deadlock victims should be chosen by priority, is deferred until there is usage to look at.
- Whether `from` should be verified for *unsandboxed* agents too. Per-agent tokens (Phase 4) settle it for sandboxed runtimes, where it matters; requiring them from local shells and hooks would cost ergonomics for little, so they stay optional until there is a reason.
- Read-set capacity and eviction: 5,000 marks per agent is a guess; measure a long Claude Code session before tuning.

What exists is described above; the contracts and hardening decisions behind it — delivery boundaries, content observations, durable recovery, the verification workstream, and the journal's event barrier — are recorded in [IMPLEMENTATION-NOTES.md](IMPLEMENTATION-NOTES.md).
