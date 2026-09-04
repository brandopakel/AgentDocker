# AgentDocker architecture

This document describes what exists today (Phase 0) precisely, and the later phases at the level of design intent. When the two disagree, the code wins and this document has a bug.

## Goals

1. **Universal.** Any agent — any model, any vendor, any runtime — can participate with nothing more than the ability to write JSON to a socket. No SDK is required, though one may exist for convenience.
2. **Safe by default.** Nothing an agent can do to the daemon can wedge another agent. Every claim expires; every exit releases.
3. **Familiar.** If you know Docker's mental model and CLI, you know AgentDocker's.
4. **Local first.** One host works with no network, no accounts, no cloud. Federation is layered on top later, not baked into the core.

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
| `paths` | Where the socket and data live |

### `agentd` (`crates/agentd`)

One process per host. It owns:

- **Registry** — in memory, guarded by a mutex.
- **Supervisor** — spawns managed agents with `tokio::process`, captures stdout/stderr to `<home>/logs/<id>.log` with timestamps and stream tags, and records the exit status.
- **Bus** — a `tokio::sync::broadcast` channel. Every message is published to it; each live subscription filters what it wants.
- **Inboxes** — per-agent queues for messages that arrive while the agent has no live subscription. Capped at 1000; oldest dropped first.
- **Lease table** — the core `LeaseTable`, plus a 1-second reaper that expires leases and emits events.
- **Events** — a second broadcast channel carrying `Event`s.
- **Store** — SQLite at `<home>/state.db`; see [Persistence](#persistence).

Locking discipline: no method holds more than one mutex at a time, and no lock is held across an `.await`. There is therefore no lock ordering to get wrong.

## Persistence

Reads are served from memory; every mutation is written through to SQLite (`rusqlite`, bundled, WAL mode) before the response goes out. Rows are JSON blobs of the core types beside the few columns needed for lookups (`agents`, `leases`, `inbox`, `events`), so adding a field to a core type is not a migration. A `meta.schema_version` row guards against opening a database written by an incompatible build.

On startup the daemon reloads agents, leases, and inboxes. Leases keep their original expiry, so a restart never extends anyone's claim. Agents that were live when the previous daemon stopped are *adopted*: the new daemon has no `Child` handle for them, so a once-per-second liveness check sends signal 0 to every unsupervised live agent that reported a pid and records an exit (releasing its leases) when the process is gone. The same check covers externally registered agents, which is what makes a Claude Code session that dies without deregistering harmless. An external agent that registered without a pid can only leave by deregistering.

Events are appended to the store as they are emitted and trimmed to the newest 10,000 once a minute; `agentdocker events --replay N` shows the last N before streaming. A persistence failure is logged and does not fail the request — the in-memory state has already moved, and a daemon that keeps serving beats one that halts on a full disk.

### `agentdocker` (`crates/cli`)

A thin client. Each invocation opens one connection, sends one request, and prints the response(s). It exists so humans and shell hooks can participate; it is not the only way in.

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
| `register {spec, pid?}` | `agent` | for processes the daemon did not start |
| `deregister {agent}` | `agent` | marks an external agent exited |
| `stop {agent, force?}` | `agent` | SIGTERM, or SIGKILL with `force`; status updates when the process actually exits |
| `remove {agent}` | `ok` | forget a finished agent |
| `list {all?}` | `agents` | live only unless `all` |
| `inspect {agent}` | `agent` | |
| `heartbeat {agent}` | `ok` | bumps `last_seen` |
| `send {from, to, kind, payload, reply_to?}` | `sent` | `to` is an agent ref, `topic:<name>`, or `all` |
| `subscribe {agent?, topics?}` | stream of `message` | flushes the inbox first, then live until the client disconnects |
| `inbox {agent, drain?}` | `messages` | |
| `claim {agent, resource, mode?, ttl_secs?, note?}` | `lease` or `error(conflict)` | conflict `details.held_by` lists the blocking leases |
| `renew {agent, lease, ttl_secs?}` | `lease` | |
| `release {agent, lease}` | `lease` | holder only |
| `leases {agent?, resource?}` | `leases` | `resource` filter uses overlap, not equality |
| `events {replay?}` | stream of `event` | replays the last `replay` stored events, then live until the client disconnects |
| `logs {agent, follow?, tail?}` | stream of `log`, then `end` | |

Any agent reference (`agent`, `from`, `to`) accepts a full id, a unique id prefix, or a name. Names resolve to the live agent with that name, or failing that to the most recently created finished one (so `logs` works after exit).

Errors: `{"type":"error","code":"conflict|not_found|ambiguous|name_taken|forbidden|invalid|internal","message":"...","details":{...}?}`.

## Leases

A **resource key** is `kind:value`. The daemon does not interpret kinds except one: `path`. Path keys overlap hierarchically — `path:/repo/src` overlaps `path:/repo/src/lib.rs` and `path:/repo` — so claiming a directory protects everything under it. Every other kind (`branch:`, `task:`, `db:`, or anything you invent) overlaps only on exact match. The CLI canonicalises paths that exist so two agents naming the same file differently still collide.

Two **modes**: `exclusive` conflicts with any lease on an overlapping resource held by someone else; `shared` conflicts only with exclusive leases held by someone else. An agent never conflicts with itself, and re-claiming a resource you hold in the same mode renews it instead of failing.

**TTL.** Every lease expires. The default is 300 s, the cap is 24 h. Long-running work should `renew` periodically rather than ask for a long TTL: a TTL is a liveness bound, not a reservation. A reaper runs every second; `leases` also expires before listing so its output is never stale.

**Exit.** When a managed agent's process exits, or an external agent deregisters or is stopped, every lease it holds is released and a `lease_released` event is emitted for each.

**Conflicts are informative.** A refused claim returns every blocking lease including its holder, mode, expiry, and note, and emits a `lease_conflict` event. Agents are expected to read the note, message the holder, or wait; a `wait` option (block until available, with timeout) is planned.

## Messaging

An **envelope** carries `from` (an agent id, or `user` for CLI-injected messages), `to`, a free-form `kind`, a JSON `payload`, an optional `reply_to`, and a timestamp. The daemon routes; it does not interpret `kind` or `payload`.

Three destinations:

- **Agent** — one recipient, resolved by id/prefix/name before publishing.
- **Topic** — a `/`-separated path like `repo/backend/reviews`. Subscribers give MQTT-style patterns: `+` matches one level, `#` matches the rest.
- **Broadcast** — every live agent except the sender.

**Delivery.** A message is pushed to every live subscription whose filter matches. For agent and broadcast destinations, each recipient *without* a live subscription gets the message queued in its inbox instead. Topic messages are live-only; durable topic subscriptions are a Phase 1 item. When an agent opens a subscription its inbox is flushed into the stream first; a message that lands in the tiny window between "subscribed to the bus" and "inbox drained" is suppressed by id so it is not shown twice.

Guarantees, stated plainly: live delivery is at-most-once (a slow subscriber that falls more than 1024 messages behind is told it lagged and skips); inbox delivery is at-least-once until drained, and inboxes survive a daemon restart.

## Events

`agent_created`, `agent_started`, `agent_exited`, `agent_removed`, `message_sent`, `lease_claimed`, `lease_renewed`, `lease_released`, `lease_expired`, `lease_conflict`. Each carries a timestamp and enough data to be actionable on its own (a lease event carries the whole lease). `agentdocker events` streams them; dashboards and policy engines will consume the same stream.

## Process supervision

`run` spawns the command with stdin closed and stdout/stderr piped into a log writer that prefixes each line with an ISO timestamp and `[out]`/`[err]`. The child inherits the daemon's environment plus `spec.env`. It is deliberately *not* given the CLI caller's environment, so secrets don't silently travel through the registry; pass what the agent needs with `-e`. On daemon shutdown every managed agent receives SIGTERM.

## Security model (Phase 0)

The socket is `0600`, so only the owning user can talk to the daemon, and every request is trusted at the level of that user. The protocol has no authentication because there is no one to authenticate yet. Federation (Phase 3) introduces per-host identities and authenticated channels; per-agent capability tokens are being considered for the same phase so a sandboxed agent can be given, say, messaging without lease control.

## Planned phases

### Phase 1 — adapters & persistence

- **MCP server.** `agentd` exposes an MCP endpoint offering `list_agents`, `send_message`, `read_inbox`, `claim`, `renew`, `release`, `list_leases`. Nearly every current agent runtime speaks MCP, so this single adapter makes them all first-class participants with no bespoke integration. A2A support will be evaluated alongside it for agent-to-agent task delegation.
- **Claude Code hooks pack.** `SessionStart` registers the session; `PreToolUse` on Edit/Write claims the path (and surfaces a conflict as a blocking message the model sees); `Stop`/`SessionEnd` releases and deregisters.
- ~~**Persistence.**~~ Done: see [Persistence](#persistence).
- **Teams.** An `Agentfile` (TOML) describing several agents and their topics, and `agentdocker up` / `down` to manage them together.
- **`claim --wait`.** Block until the resource is free or a timeout passes.

### Phase 2 — shared context

A versioned key/document store agents can read, write, and *watch*. Writes emit change events to watchers, which is the mechanism that tells an agent its context is stale before it acts on it. A `handoff` message kind with a defined payload (task, current state, open questions, relevant resource keys) lets one agent hand work to another without a human relaying.

### Phase 3 — federation

`agentd` instances discover and authenticate each other (mTLS, or a WireGuard-style keypair exchange). Agent ids become `host/agent`; messages, leases, and events route across peers; the registry becomes a replicated view. The core primitives do not change — which is the reason for keeping them pure and host-agnostic now.

### Phase 4 — policy & scheduling

Quotas per agent or label, priorities that decide who wins a contested resource, dependencies (`start B after A exits 0`), restart policies, and a TUI/web dashboard fed by the event stream.

## Open questions

- Should topic messages ever queue? Durable subscriptions solve it, but require the daemon to know about an agent's interests when it is offline.
- Priority vs. fairness for contested leases: strict priority is simple but starves; the plan is to start with FIFO waiting in `claim --wait` and revisit.
- Whether `from` should be verified (the socket owner is trusted today, so an agent can impersonate another). Per-agent tokens would close this; the cost is friction for shell-based agents.
