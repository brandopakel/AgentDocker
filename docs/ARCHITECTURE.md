# AgentDocker architecture

This document describes what exists today precisely, and the later phases at the level of design intent. When the two disagree, the code wins and this document has a bug.

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
- **Store** — SQLite at `<home>/state.db`; see [Persistence](#persistence).

Locking discipline: no method holds more than one mutex at a time, and no lock is held across an `.await`. There is therefore no lock ordering to get wrong.

## Persistence

Reads are served from memory; every mutation is written through to SQLite (`rusqlite`, bundled, WAL mode) before the response goes out. Rows are JSON blobs of the core types beside the few columns needed for lookups (`agents`, `leases`, `inbox`, `events`; `projects` is the one plain table, a cache of fingerprints per repository root), so adding a field to a core type is not a migration. A `meta.schema_version` row guards against opening a database written by an incompatible build.

On startup the daemon reloads agents, leases, and inboxes, tidying as it goes: a managed record still `created` (the old daemon died mid-spawn) is recorded as failed, a second live record with an already-live name is recorded as exited, and a lease whose holder is not live is dropped — each written back so the store and the registry agree. Leases keep their original expiry, so a restart never extends anyone's claim.

Agents that were live when the previous daemon stopped are *adopted*: the new daemon has no `Child` handle for them, so a once-per-second liveness check inspects every unsupervised live agent that reported a pid and records an exit (releasing its leases) when the process is gone. "Gone" means signal 0 fails, *or* the process now behind that pid started at a different time than the one that registered — the daemon records the process start time (macOS `proc_pidinfo`, Linux `/proc/<pid>/stat`) so a recycled pid, typically after a reboot, is not mistaken for the agent. The same check covers externally registered agents, which is what makes a Claude Code session that dies without deregistering harmless. An external agent that registered without a pid can only leave by deregistering; a managed agent still being spawned is skipped.

Every event carries a strictly increasing `seq`, assigned by the daemon and continued across restarts from the stored history. Events are appended to the store as they are emitted and trimmed to the newest 10,000 once a minute; `agentdocker events --replay N` shows the last N before streaming, and the server drops any live event whose `seq` the replay already covered, so an event emitted while the stream was being set up is delivered once. A persistence failure is logged and does not fail the request — the in-memory state has already moved, and a daemon that keeps serving beats one that halts on a full disk.

### `agentdocker` (`crates/cli`)

A thin client. Each invocation opens one connection, sends one request, and prints the response(s). It exists so humans and shell hooks can participate; it is not the only way in.

### Starting the daemon

Nobody has to start `agentd` by hand. A client that cannot connect — no socket file, or nothing listening — starts the daemon itself, the way `ssh-agent` and `buildkitd` are started by their clients, then waits for the socket (3 s for the CLI and the MCP server, 1 s for hooks, which fail open past that). `AGENTDOCKER_NO_AUTOSTART=1` turns this off. The daemon it starts is the `agentd` beside the client's own binary when there is one, so a build in `target/` starts the matching daemon, else `agentd` on `PATH`; it runs in its own process group with stdout and stderr appended to `<home>/agentd.log`.

Exactly one daemon serves a socket, guaranteed by an advisory lock beside it (`agentd.sock` → `agentd.lock`). The daemon takes the lock for its lifetime before touching the socket, and exits at once, successfully, if it cannot. A client decides whether to spawn by taking the same lock for an instant: getting it means no daemon exists; not getting it means one is up or starting, so the client only waits. Two clients racing may both spawn a daemon, and the loser exits on the lock. The daemon's stale-socket check (remove the file if nothing answers on it) stays as a second line of defence.

**As a service.** On-demand start is enough for a laptop; `agentdocker daemon install` additionally runs `agentd` as a login service so it survives reboots and crashes and belongs to no terminal — a launchd agent (`~/Library/LaunchAgents/dev.agentdocker.agentd.plist`) on macOS, a systemd user unit (`~/.config/systemd/user/agentd.service`) on Linux. Both restart the daemon after a *failure* only, because a clean exit is what a service daemon does when an on-demand one already holds the lock; `install` therefore first asks any running daemon to exit (the `shutdown` request, which SIGTERMs managed agents exactly as Ctrl-C does) and then hands the socket to the service. `daemon uninstall`, `start`, `stop`, `restart`, and `status` do what they say, with `start` and `stop` falling back to the on-demand daemon when no service is installed; `--dry-run` on `install` and `uninstall` prints the files and commands instead. The service definition bakes in `--home` (and `--socket` when overridden) so it serves the same paths the CLI that installed it used. Files and command sequences are pure and unit-tested; only the final execution touches the system.

**Installing.** `cargo install agentdocker` builds both binaries; `install.sh` at the repository root downloads the release archive for the host (`agentdocker-<target>.tar.gz`, four targets: macOS and Linux musl on x86_64 and aarch64, named without the version so `releases/latest/download/…` works) and drops them into `~/.local/bin`; `packaging/homebrew/agentdocker.rb` is a formula for a tap, with a `brew services` block that runs the daemon. The release workflow builds and uploads the archives with SHA-256 checksums on every `v*` tag.

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
| `SessionStart` | register (or reuse) `claude-<session id prefix>`; list agents; drain inbox | `additionalContext`: who else is live, how to talk to them, queued messages |
| `UserPromptSubmit`, `PostToolUse` | drain inbox | `additionalContext` with the messages, or nothing |
| `PreToolUse` (Edit/Write/MultiEdit/NotebookEdit) | claim `path:<absolute file>` exclusive, 600 s, note "editing in Claude Code session …" | on conflict `permissionDecision: deny` with the holder and their note; otherwise nothing, so the user's own permission rules still apply |
| `Stop` | release all; unless `stop_hook_active` or `--no-wake`, drain inbox | `decision: block` with the messages when any are waiting, so the model handles them before finishing |
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

**Branch and head.** Every agent with a working directory carries `vcs`: the branch (or none, detached), the commit HEAD points at (or none, unborn), and when it was observed. It is read from `.git` directly — `HEAD` and one ref file, packed refs as the fallback, the worktree's own git directory for a linked worktree — in microseconds and with no `git` process (`agentdocker_host::vcs`), so it costs nothing to do often: the daemon reads it when an agent is created and again for every live agent every five seconds, which covers adopted agents and anything started with `run`; the Claude Code hooks additionally send it with `report` on `SessionStart`, `UserPromptSubmit`, and `PostToolUse`, so a `git checkout` run through the Bash tool shows up at once. Only a real change is persisted and announced (`agent_vcs_changed`); a fresh timestamp alone is neither. `ps` shows `BRANCH` and `HEAD` so "are we even looking at the same code" is answered at a glance. Dirtiness stays unknown until something cheap can tell.

**What it gives you.** `ps` shows a `PROJECT` column (`repo`, or `repo@wt` inside a linked worktree) and sorts by project; `ps --project .` (any path inside the project) or `--project <id prefix>` filters, as does `-l key=value`; `BRANCH` and `HEAD` say what each agent's checkout is on; `list {project?, labels?}` is the request behind both. `send --to project` reaches everyone else working in the same project, with inbox fallback like broadcast, and a session's `SessionStart` orientation names the agents in its project before any others. `inspect` shows the full reference. Leases on files inside a project are stored as project-relative `file:` keys (see [Leases](#leases)), so `leases --resource <root>` lists everything held in a project wherever it is checked out.

## Wire protocol

Transport: newline-delimited JSON over a Unix domain socket at `$AGENTDOCKER_SOCKET` (default `~/.agentdocker/agentd.sock`, mode `0600`). One request object per line, tagged by `"op"`; responses tagged by `"type"`.

```json
{"op":"claim","agent":"writer","resource":"path:/repo/src","mode":"exclusive","ttl_secs":300,"note":"refactoring"}
{"type":"lease","lease":{"id":"3f1c...","resource":"path:/repo/src","holder":"9a2b...","mode":"exclusive","acquired_at":"...","expires_at":"...","note":"refactoring"}}
```

| Request | Response | Notes |
|---|---|---|
| `ping` | `pong` | version, uptime |
| `run {spec}` | `agent` | spawns `spec.command`; child gets `AGENTDOCKER_SOCKET`, `AGENTDOCKER_AGENT_ID`, `AGENTDOCKER_AGENT_NAME` |
| `register {spec, pid?}` | `agent` | for processes the daemon did not start; `spec.workdir` decides the project |
| `deregister {agent}` | `agent` | marks an external agent exited |
| `discover` | `processes` | running processes of known agent runtimes that no live agent claims by pid |
| `adopt {pid, name?, runtime?}` | `agent` | registers such a process; `invalid` if a live agent already has the pid |
| `stop {agent, force?}` | `agent` | SIGTERM, or SIGKILL with `force`; status updates when the process actually exits |
| `remove {agent}` | `ok` | forget a finished agent |
| `list {all?, project?, labels?}` | `agents` | live only unless `all`; `project` is an id prefix or an absolute path inside it; `labels` must all match |
| `inspect {agent}` | `agent` | |
| `heartbeat {agent}` | `ok` | bumps `last_seen` |
| `report {agent, vcs?}` | `ok` | what an adapter observed; a changed `vcs` is stored and announced |
| `shutdown` | `ok` | the daemon exits after replying; managed agents get SIGTERM, as on Ctrl-C |
| `send {from, to, kind, payload, reply_to?}` | `sent` | `to` is an agent ref, `project:<id prefix or absolute path>`, `topic:<name>`, or `all` |
| `subscribe {agent?, topics?}` | stream of `message` | flushes the inbox first, then live until the client disconnects |
| `inbox {agent, drain?}` | `messages` | |
| `claim {agent, resource, mode?, ttl_secs?, note?, wait_secs?}` | `lease` or `error(conflict)` | a `path:` inside a project is stored as `file:<project id>/<relative>`; conflict `details.held_by` lists the blocking leases; `wait_secs` (max 600) retries until the conflict clears |
| `renew {agent, lease, ttl_secs?}` | `lease` | |
| `release {agent, lease}` | `lease` | holder only |
| `release_all {agent}` | `leases` | every lease the agent holds; the reply lists them |
| `leases {agent?, resource?}` | `leases` | `resource` filter uses overlap, not equality; a `path:` filter also matches its `file:` form |
| `events {replay?}` | stream of `event` | replays the last `replay` stored events, then live until the client disconnects |
| `logs {agent, follow?, tail?}` | stream of `log`, then `end` | |

Any agent reference (`agent`, `from`, `to`) accepts a full id, a unique id prefix, or a name. Names resolve to the live agent with that name, or failing that to the most recently created finished one (so `logs` works after exit).

Errors: `{"type":"error","code":"conflict|not_found|ambiguous|name_taken|forbidden|invalid|internal","message":"...","details":{...}?}`.

## Leases

A **resource key** is `kind:value`. The daemon interprets two kinds, `path` and `file`, which overlap hierarchically — `path:/repo/src` overlaps `path:/repo/src/lib.rs` and `path:/repo` — so claiming a directory protects everything under it. Every other kind (`branch:`, `task:`, `db:`, or anything you invent) overlaps only on exact match.

**Files are named by project, not by path.** A `file:<project id>/<relative path>` key names a file by the project it belongs to and its path within the checkout, so it is the same resource from a linked worktree, from inside a container that mounts the checkout somewhere else, or from another clone on another host. Clients keep sending `path:<absolute>` — the hooks, the MCP server, and `agentdocker claim <path>` know nothing about ids — and the daemon rewrites the key on the way in: it canonicalises as much of the path as exists (a file about to be created gets the key it will have once it does), then uses the claimant's own project if the path lies in its checkout, else the repository or `Agentfile.toml` root containing the path. Only paths outside every project stay `path:`, and `leases --resource <path>` matches both forms. The CLI prints `file:` keys with the project id shortened.

Two **modes**: `exclusive` conflicts with any lease on an overlapping resource held by someone else; `shared` conflicts only with exclusive leases held by someone else. An agent never conflicts with itself, and re-claiming a resource you hold in the same mode renews it instead of failing.

**TTL.** Every lease expires. The default is 300 s, the cap is 24 h. Long-running work should `renew` periodically rather than ask for a long TTL: a TTL is a liveness bound, not a reservation. A reaper runs every second; `leases` also expires before listing so its output is never stale.

**Exit.** When a managed agent's process exits, or an external agent deregisters or is stopped, every lease it holds is released and a `lease_released` event is emitted for each.

**Conflicts are informative.** A refused claim returns every blocking lease including its holder, mode, expiry, and note, and emits a `lease_conflict` event. Agents are expected to read the note, message the holder, or wait.

**Waiting.** `claim` with `wait_secs > 0` subscribes to the event stream *before* its first attempt, and on conflict waits for a `lease_released` or `lease_expired` event on an overlapping resource (or the deadline) before trying again. One `lease_conflict` event is emitted per request no matter how long it waits. Waiters are not queued: when a lease clears, every waiter retries and the lease table decides, so two agents waiting on the same resource race. FIFO fairness is an open question below.

## Messaging

An **envelope** carries `from` (an agent id, or `user` for CLI-injected messages), `to`, a free-form `kind`, a JSON `payload`, an optional `reply_to`, and a timestamp. The daemon routes; it does not interpret `kind` or `payload`.

Four destinations:

- **Agent** — one recipient, resolved by id/prefix/name before publishing.
- **Project** — every live agent in a project except the sender. `project:<selector>` takes an id (any unique prefix) or an absolute path inside the project; the CLI and MCP server turn a bare `project` into the caller's current directory, so `send --to project` needs no ids at all.
- **Topic** — a `/`-separated path like `repo/backend/reviews`. Subscribers give MQTT-style patterns: `+` matches one level, `#` matches the rest.
- **Broadcast** — every live agent except the sender.

**Delivery.** A message is pushed to every live subscription whose filter matches (a project delivery matches subscribers whose agent was in that project when it subscribed). For agent, project, and broadcast destinations, each recipient *without* a live subscription gets the message queued in its inbox instead. Topic messages are live-only; whether they should ever queue is an [open question](#open-questions). When an agent opens a subscription its inbox is flushed into the stream first; a message that lands in the tiny window between "subscribed to the bus" and "inbox drained" is suppressed by id so it is not shown twice.

Guarantees, stated plainly: live delivery is at-most-once (a slow subscriber that falls more than 1024 messages behind is told it lagged and skips); inbox delivery is at-least-once until drained, and inboxes survive a daemon restart.

## Events

`agent_created` (with the project id), `agent_started`, `agent_exited`, `agent_removed`, `message_sent`, `lease_claimed`, `lease_renewed`, `lease_released`, `lease_expired`, `lease_conflict`, `project_discovered`, `agent_vcs_changed`, `daemon_stopping`. Each carries a timestamp and enough data to be actionable on its own (a lease event carries the whole lease). `agentdocker events` streams them; dashboards and policy engines will consume the same stream.

## Process supervision

`run` spawns the command with stdin closed and stdout/stderr piped into a log writer that prefixes each line with an ISO timestamp and `[out]`/`[err]`. The child inherits the daemon's environment plus `spec.env`. It is deliberately *not* given the CLI caller's environment, so secrets don't silently travel through the registry; pass what the agent needs with `-e`. On daemon shutdown every managed agent receives SIGTERM.

## Security model (Phase 0)

The socket is `0600`, so only the owning user can talk to the daemon, and every request is trusted at the level of that user. The protocol has no authentication because there is no one to authenticate yet. Federation (Phase 6) introduces per-host identities and authenticated channels; per-agent capability tokens are being considered for the same phase so a sandboxed agent can be given, say, messaging without lease control.

## Roadmap

Phase 0 and the adapters in Phase 1 exist. Everything below is design intent, written at the level of detail needed to build it — data model, protocol, storage, CLI, events, and what "done" means — so that each item can become a PR without a second design pass. Phases are ordered by dependency, not importance; [Delivery order](#delivery-order) lists the PR sequence.

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

See [Projects](#projects). `report` will grow `reads` and `writes` in Phase 3.

### Phase 3 — the working set

This phase replaces the earlier plan for a separate key/document context store. Grounding staleness in the filesystem — what agents actually read and change — covers the real case with no new concept for agents to learn; a document store can be added later if a need survives this.

#### Read-set tracking and staleness

- **Reporting.** Hooks report reads on `PostToolUse` for `Read` (the file), `Grep` and `Glob` (the searched directory, recorded as a directory mark that covers everything beneath it), and writes for the edit tools; both go through `report`. MCP hosts expose no tool observation, so MCP agents have no read set (see the fallback below).
- **Core.** `WorkingSet { reads: BTreeMap<RelPath, ReadMark { at, head: Option<String> }>, writes: … }` per agent, paths stored *project-relative* with the worktree noted, so marks compare across worktrees and prefix queries are short. Capacity 5,000 marks per agent, oldest evicted; a directory mark absorbs file marks beneath it. Pure `WorkingSet::stale_against(&[Change]) -> Vec<RelPath>` decides what to notify. Every rule here is unit-tested in core.
- **Watching.** The daemon watches each project root that has at least one live agent (`notify`: FSEvents on macOS, inotify on Linux), ref-counted by live agents and dropped when the last one leaves. Raw events are debounced (100 ms), filtered through the project's `.gitignore` (the `ignore` crate's matcher) so `target/` and `node_modules/` never reach the pipeline, and `.git/` is ignored except `HEAD` and `refs/heads/`, which feed head observation (Phase 3 journal). Events become `Change { seq, project, worktree, path, kind: Created | Modified | Removed | Renamed, at, by: Attribution }`.
- **Attribution.** In order: an agent that reported a write to the path within the last 5 s; else the holder of an exclusive lease overlapping the path; else `External` (the user's editor, `git checkout`, a build). Best-effort by construction, and labelled as such in every output.
- **Notices.** For each live agent in the project other than the author, if the changed path is in its read set and the mark predates the change, the daemon queues a message `kind: stale` from `agentd` with `{paths: [{path, by, at}]}`. Notices are coalesced per agent over a 2 s window and merged into an undelivered `stale` message already in the inbox rather than enqueued beside it, so a noisy build produces one notice, not a thousand. Hooks surface it as `additionalContext`; a fresh `Read` of the path clears its mark. On `PreToolUse` for an edit of a stale path the hook denies once with the reason and the author's note, so the model re-reads before it writes; a second attempt after the read passes. `agent_stale {agent, paths}` is emitted; `file_changed {change}` is emitted to the live stream but persisted in the ledger, not the events table, because change volume would otherwise crowd out everything else within the 10,000-event window.
- **Fallback for agents without a read set.** Nothing is pushed. `changes --project . --since <seq|duration>` and the digest in `SessionStart`/`UserPromptSubmit` are pull-based, so an MCP or adopted agent still learns what moved without being flooded.
- **Persistence.** `reads (agent, project, path, at, head, PRIMARY KEY (agent, path))`, written through per `report` call in one transaction and deleted on agent exit, so a daemon restart under running agents does not silently forget what they read.

*Done when* agent A reads a file, agent B edits it under a lease, and A's next hook fire hands the model a notice naming the file, B, and B's note — and an edit by A before re-reading is refused once.

#### Attribution ledger

The `changes` table is the ledger: `(seq INTEGER PRIMARY KEY, project, worktree, path, kind, at, by_agent, lease, head, json)` with indexes on `(project, seq)` and `(project, path, seq)`. Paths are project-relative, so "everything under `src/`" is a string prefix range (`path >= 'src/' AND path < 'src0'`), which is the query the journal needs at every lease release. `agentdocker blame <path>` lists the changes to a path with agent, lease note, and time; `agentdocker changes [--project .] [--agent X] [--since …]` lists a range; `inspect <agent>` shows the agent's changed paths. Retention: newest 100,000 rows per project, pruned once a minute like events; journal entries carry their own path lists, so pruning the ledger loses fine grain only. Attribution is best-effort and every rendering says so.

#### Change journal

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

**Write path.** In the release handler, after the lease table has dropped the leases: for each released `path:` resource, one indexed prefix-range query on the ledger bounded by `[acquired_at, now]`; union, sort, dedupe, cap; build the entry; assign `seq` from the per-project counter (loaded from `MAX(seq)` at startup, like event `seq`); write `journal`, `journal_paths`, and `journal_fts` in the *same transaction* as the lease deletions so a crash cannot leave a released lease with no entry or an entry for a lease still held; push to the ring; emit `journal_appended {entry}`. Sub-millisecond on the indexes above, and the response goes out after the write like every other mutation.

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

**Adapters.** Hooks: `SessionStart` and `UserPromptSubmit` inject the digest as `additionalContext`; `Stop` sends `release_all` with the transcript-tail summary. MCP: `read_journal {since?}` returns the digest with `advance`, `journal_note {summary}` appends a note, and the `claim`/`release` tools accept `summary`.

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

#### Worktrees as the writable layer

A container writes to its own layer over a shared image; an agent should write to its own worktree over a shared repository. `run --isolate [--base <ref>]` (and `isolate = true` in an `Agentfile.toml` entry) has the daemon create `git worktree add <home>/worktrees/<project id>/<agent name> -b agent/<name> <base>` (default `HEAD`), set the spec's `workdir` to it, and record `isolation: Some(Worktree { path, branch, base })` on the agent. The worktree resolves to the same project through the common git directory. It outlives the agent like a container's layer outlives a stopped container: `remove` keeps it, `remove --purge` deletes the worktree and branch.

- **`agentdocker diff <agent> [--stat]`** — the worktree's changes against `base` (committed and uncommitted), rendered as a unified diff.
- **`agentdocker commit <agent> [-m …] [--push] [--pr]`** — commits the worktree's changes on the agent's branch; `--push` pushes; `--pr` opens a pull request via `gh`. Under the hood the daemon claims `branch:<base>` exclusive for the duration so two commits onto one base cannot interleave.
- **`agentdocker overlap [--project .]`** — pairs of agents whose ledgers touch the same project-relative path, i.e. merge conflicts before they happen. This is why the ledger stores relative paths.
- **Leases in isolation.** Two isolated agents editing the same relative path is the *intended* parallelism, so the hooks do not claim per edit in an isolated worktree (path keys are absolute and would never collide anyway); contention moves to `commit`, which is guarded by the branch lease. Absolute `path:` keys keep their physical meaning for non-isolated agents.

*Done when* two isolated agents edit the same file, `overlap` names them, each `commit` lands on its own branch, and neither was ever blocked from editing.

#### Sandboxes

Settled on 2026-09-04. **The daemon is never sandboxed; the sandbox is a property of the agent's runtime**, the way Docker's `--runtime` picks runc, gVisor, or Kata. It supervises processes that use the user's repositories and credentials, signals pids, and canonicalises paths, so it lives in the host's namespace, and AgentDocker builds no sandbox engine of its own. Three layers compose:

1. **Runtime-native sandboxes.** Claude Code and Codex ship their own (seatbelt on macOS, bubblewrap or landlock on Linux). The hooks and the MCP server run as children of the host process, *outside* that sandbox, so they always reach the daemon; nothing to build.
2. **Worktree isolation** (above) sandboxes the filesystem layer with no process isolation.
3. **Container runtimes.** `--runtime docker --image <img>` (likewise `podman`, and Apple's `container`) has the supervisor run the agent's command inside a container; the docker client is the supervised child, so pid, logs, and exit status work unchanged. Defaults are closed: `--network none`, the checkout bind-mounted read-write at `/work`, the daemon socket mounted at `/run/agentd.sock` with `AGENTDOCKER_SOCKET`, `AGENTDOCKER_AGENT_ID`, and `AGENTDOCKER_PROJECT_ROOT=/work` set, and no host environment passed through; the spec opts into network, extra mounts, and `-e` values. Combined with `--isolate` an agent gets its own worktree *and* its own filesystem view.

Two consequences were pulled forward because sandboxes depend on them:

- **Project-relative resources** (done; [Leases](#leases)). Inside a container the same file has a different absolute path, so leases — and, in Phase 3, read sets and the ledger — name files by project id and relative path. The daemon translates; sandboxed clients still send whatever path they see.
- **Per-agent tokens.** The socket is the one deliberate hole in a sandbox, so a sandboxed agent must not be able to impersonate another sender or stop other agents. `run` and `register` mint a token, returned in the response and passed to managed agents as `AGENTDOCKER_TOKEN`; a request carrying `token` is bound to that agent id (its `agent`/`from` must match) and may only act on itself. Sandboxed runtimes always get one and the daemon requires it from them; local shells and hooks stay token-free, so the CLI's ergonomics do not change. Tokens are stored hashed beside the record and revoked when the agent exits. This is the identity admission policy (Phase 5) binds rules to, and it closes the `from` open question for the case that matters.

#### Handoff bundles

An agent handing work to another should not have to write its state down; the daemon already knows it. `handoff {from, to, task?, note?, transfer_leases?}` assembles a `HandoffBundle { task, note, from, vcs, leases, read_set (paths and the heads they were read at), changes (the sender's ledger rows since it joined), diff (for isolated senders: the patch, truncated at 64 KB with a pointer to the worktree), unread_inbox, journal (the sender's entries) }` and sends it as a message `kind: handoff` to the recipient with inbox fallback. With `transfer_leases` the sender's leases move to the recipient atomically (`LeaseTable::transfer(from, to, now)`, new event `lease_transferred {lease, from, to}`; only the holder may transfer); otherwise they are released. A `handoff` journal entry is appended, the recipient's read set is seeded from the bundle so staleness carries over, and its cursor is set to the sender's. `agentdocker handoff <from> <to> --task "…"`; `agentdocker export <agent> > bundle.json` / `import` write and read the same structure so a bundle can cross hosts by hand until federation. The bundle is a core type with a schema version of its own.

### Phase 5 — arbitration, humans & policy

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

### Phase 6 — federation

`agentd` instances discover and authenticate each other (mTLS, or a WireGuard-style keypair exchange). Agent ids become `host/agent`; messages, leases, events, and the journal route across peers; the registry becomes a replicated view. Project fingerprints are what make "the same repository on my laptop and in the cloud" one project, and handoff bundles are what move work between them. The core primitives do not change — which is the reason for keeping them pure and host-agnostic now.

### Delivery order

Each PR changes `protocol.rs`, the wire-protocol table above, the CLI, and tests together, per `CLAUDE.md`. Adding a table is not a `SCHEMA_VERSION` bump (`CREATE TABLE IF NOT EXISTS`); changing what a stored row means is.

| # | PR | phase | depends on |
|---|---|---|---|
| 1 | ✅ `crates/host` with project discovery; `register` defaults `workdir`; `project` on records; `ps` grouping, `--project`, `list {project?, labels?}`; `projects` cache table | 2 | — |
| 2 | ✅ `project:` destination; hooks orient by project | 2 | 1 |
| 3 | ✅ project-relative `file:` lease keys, translated by the daemon | 2 | 1 |
| 4 | ✅ `daemon install/uninstall/status`; lazy start; release workflow, tap, installer | 2 | — |
| 5 | ✅ `discover` / `adopt`; dimmed rows in `ps` | 2 | 1 |
| 6 | ✅ `report` request with `vcs`; `BRANCH`/`HEAD` in `ps` | 2 | 1 |
| 7 | read sets, project watcher, ledger (`changes` table, `blame`, `changes`) | 3 | 3, 6 |
| 8 | staleness notices; hook deny-once on stale edits | 3 | 7 |
| 9 | change journal: schema, release summaries and transcript tail, cursors, ring cache, `journal` CLI and MCP tools, hook digests | 3 | 7 |
| 10 | `run --isolate`, `diff`, `commit`, `overlap` | 4 | 7 |
| 11 | `handoff`, lease transfer, `export` / `import` | 4 | 9, 10 |
| 12 | per-agent tokens; `docker` / `podman` / `container` runtimes with closed defaults | 4 | 3 |
| 13 | FIFO wait queue, wait graph, deadlock detection | 5 | — |
| 14 | human agent, `ask` / `answer`, notifications | 5 | 2 |
| 15 | admission policy and quotas | 5 | 12 |
| 16 | restart policies, `depends_on`, `top` | 5 | — |
| 17 | federation | 6 | 11, 12 |

### Planned protocol and event additions

Listed here so the wire-protocol table above stays a description of what exists.

| Request | Response | Phase |
|---|---|---|
| `claim {…}` | adds `error(deadlock)` | 5 |
| `report {…, reads?, writes?}` | `ok` (adds read and write sets to the existing request) | 3 |
| `release {…, summary?}`, `release_all {…, summary?}` | `lease` / `leases` | 3 |
| `changes {project, since?, path?, agent?, limit?}` | `changes` | 3 |
| `journal {project, since_seq?, until_seq?, agent?, branch?, kind?, path?, grep?, limit?, digest?}` | `journal` or `digest` | 3 |
| `journal_add {agent, summary}` | `journal_entry` | 3 |
| `diff {agent, stat?}` | `diff` | 4 |
| `commit {agent, message?, push?, pr?}` | `commit` | 4 |
| `handoff {from, to, task?, note?, transfer_leases?}` | `handoff` | 4 |
| `run` / `register` responses gain `token`; every request accepts `token?` | — | 4 |
| `ask {from, to, question, timeout_secs}` | `message` (the answer) or `error(timeout)` | 5 |

New events: `file_changed`, `agent_stale`, `journal_appended`, `lease_transferred`, `lease_waiting`, `lease_wait_timeout`, `lease_deadlock`, `policy_denied`. New error codes: `Deadlock` (Phase 5) and `Timeout` (for `ask`).

## Open questions

- Should topic messages ever queue? Durable subscriptions solve it, but require the daemon to know about an agent's interests when it is offline. The `project:` destination removes the most common reason to want this.
- Priority vs. fairness for contested leases: waiters race today, and Phase 5 makes them FIFO. Whether labels or policy should ever let a claim jump the queue, and whether deadlock victims should be chosen by priority, is deferred until there is usage to look at.
- Whether `from` should be verified for *unsandboxed* agents too. Per-agent tokens (Phase 4) settle it for sandboxed runtimes, where it matters; requiring them from local shells and hooks would cost ergonomics for little, so they stay optional until there is a reason.
- Read-set capacity and eviction: 5,000 marks per agent is a guess; measure a long Claude Code session before tuning.
