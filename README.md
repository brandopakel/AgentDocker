# AgentDocker

**Docker for AI agents.** One daemon that creates, supervises, organises, and — above all — connects every agent running on your machines, whatever model or vendor is behind it.

Coding agents are cheap to start and easy to lose track of. Run three of them against one repository and you get the same failure modes distributed systems solved decades ago: two agents editing the same file, an agent reasoning about context another agent just invalidated, and no shared channel to say "I've got this one" or "here's what I found". AgentDocker gives agents the primitives to coordinate, using the shape everyone already knows from containers.

> Status: **early**. The daemon, CLI, messaging, and lease system work end-to-end on a single host. Runtime adapters, persistence, and multi-host federation are on the [roadmap](#roadmap).

## The Docker analogy

| Docker | AgentDocker | What it is |
|---|---|---|
| image | **agent spec** | How to launch an agent: runtime, model, command, workdir, env, labels |
| container | **agent** | One running instance with an id, name, status, pid, and logs |
| `dockerd` | **`agentd`** | Per-host daemon: registry, supervisor, message bus, lease arbiter, event log |
| `docker` CLI | **`agentdocker`** | `ps`, `run`, `stop`, `logs`, `inspect`, `send`, `watch`, `claim`... |
| network | **messages & topics** | Direct, topic (pub/sub), and broadcast messaging with offline inboxes |
| volume lock | **lease** | Time-limited exclusive/shared claim on a file, directory, branch, task, or anything |
| compose project | **project** | Derived from the working directory: the repository (every worktree of it), else the directory. Agents group by it automatically |
| `docker events` | **events** | Live stream of everything the daemon does |

Agents don't need an SDK. Anything that can write a line of JSON to a Unix socket — a shell hook, a Python script, an MCP tool call — is a first-class participant. That is what makes it model- and vendor-agnostic: Claude Code, Codex, Gemini CLI, Cursor, and hand-rolled agents all coordinate through the same daemon.

## Quick start

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"

# 1. Start the daemon (listens on ~/.agentdocker/agentd.sock)
agentd &

# 2. Launch two agents. Any command works; here they are shell loops.
agentdocker run --name writer   --runtime custom -- sh -c 'sleep 300'
agentdocker run --name reviewer --runtime custom -- sh -c 'sleep 300'
agentdocker ps                    # grouped by project: the repo each agent works in
agentdocker ps --project .        # only agents in this project

# 3. Coordinate on a resource. The second claim is refused, and says by whom.
agentdocker claim --as writer   src/ --note "refactoring the parser"
agentdocker claim --as reviewer src/parser.rs        # -> conflict: held by writer
agentdocker leases

# 4. Talk. Messages to an offline agent queue in its inbox.
agentdocker send --from reviewer --to writer "ping me when src/ is free"
agentdocker inbox --as writer
agentdocker watch --as writer &                       # live delivery from here on
agentdocker send --from reviewer --to topic:repo/reviews --kind notice "PR #12 approved"
agentdocker send --from reviewer --to project "heads up: I'm touching src/ next"   # everyone in this repo

# 5. Watch it all happen
agentdocker events
agentdocker logs -f writer
agentdocker stop writer
```

Processes started with `agentdocker run` get `AGENTDOCKER_SOCKET`, `AGENTDOCKER_AGENT_ID`, and `AGENTDOCKER_AGENT_NAME` in their environment, so inside an agent the CLI already knows who it is:

```sh
agentdocker claim path:src/lib.rs      # --as defaults to $AGENTDOCKER_AGENT_ID
agentdocker send --to reviewer "done"  # --from too
```

An agent you did not start through the daemon (an interactive Claude Code session, say) joins with `agentdocker register --name claude-main --runtime claude-code --pid $$` and leaves with `agentdocker deregister`.

### Give any MCP-capable agent the tools directly

`agentdocker mcp` is an MCP server over stdio. Point a host at it and its model gets `list_agents`, `send_message`, `read_inbox`, `wait_for_messages`, `claim`, `renew`, `release`, `list_leases`, `inspect_agent`, and `whoami` as tools, plus instructions on when to use them. The server registers the host as an agent when it starts (named `<runtime>-<pid>` unless you pass `--name`) and deregisters when the host closes it; if the host was itself started by `agentdocker run`, the existing identity is reused.

```sh
# Claude Code
claude mcp add agentdocker -- agentdocker mcp --runtime claude-code --name reviewer

# Codex: ~/.codex/config.toml
[mcp_servers.agentdocker]
command = "agentdocker"
args = ["mcp", "--runtime", "codex"]

# Cursor (.cursor/mcp.json) / Gemini CLI (~/.gemini/settings.json) / anything else
{ "mcpServers": { "agentdocker": { "command": "agentdocker", "args": ["mcp", "--runtime", "cursor"] } } }
```

Two Claude Code sessions in the same repo, both with the server configured, will refuse to edit the same file at the same time and can message each other about it — with no changes to either session's prompt.

### Claude Code: hooks make it automatic

The MCP server gives the model tools it *may* call. Hooks make coordination happen whether or not it thinks to:

```sh
agentdocker hook install claude-code          # writes ./.claude/settings.json (or --user for ~/.claude)
```

| Claude Code event | what the hook does |
|---|---|
| `SessionStart` | registers the session as agent `claude-<session>`; tells the model who else is running and hands it any queued messages |
| `PreToolUse` on Edit/Write/MultiEdit/NotebookEdit | claims `path:<file>` first; if another agent holds it, the edit is **denied** with the holder's name and note |
| `UserPromptSubmit`, `PostToolUse` | delivers messages from other agents as context, as they arrive |
| `Stop` | releases every lease; if messages arrived while it was working, blocks the stop so the model reads them first (`--no-wake` disables) |
| `SessionEnd` | releases and deregisters |

The hook fails open: if `agentd` isn't running, Claude Code carries on as if the hook weren't there.

### Teams: `Agentfile.toml`

Describe several agents in one file and manage them together, the way a compose file manages containers:

```toml
name = "backend"                      # every agent gets label team=backend

[agents.writer]
runtime = "claude-code"
command = ["claude", "-p", "Implement the parser in src/parser.rs"]
workdir = "."                         # relative to this file

[agents.reviewer]
runtime = "codex"
command = ["codex", "exec", "Review whatever writer changes and message it"]
env = { RUST_LOG = "info" }
labels = { role = "review" }
```

```sh
agentdocker up                # starts writer, then reviewer; skips any already running
agentdocker up reviewer       # just one
agentdocker down              # stops them
```

### Waiting instead of failing

`agentdocker claim --wait 120 src/parser.rs` blocks until the holder releases (or the lease expires), then takes it — or reports the conflict after two minutes. The MCP `claim` tool has the same `wait_secs`.

## What it solves

**Race conditions.** A lease is an exclusive or shared claim on a *resource key* such as `path:/repo/src`, `branch:feature/x`, or `task:ISSUE-42`. Path keys are hierarchical, so a lease on a directory covers every file beneath it. Every lease has a TTL, so a crashed agent can never wedge the system, and the daemon releases everything an agent holds the moment it exits. A refused claim tells the requester exactly who holds what and the note they left.

**Lost context.** Agents that overwrite each other's work do so because neither knew the other existed. The registry (`ps`, `inspect`) makes every agent visible; leases carry human-readable notes about what the holder is doing; the event stream shows changes as they happen. Next, the daemon tracks what each agent has read and watches the project, so an agent is told when a file it depends on moved, and who moved it.

**No common channel.** Messaging is direct (`--to writer`), project-wide (`--to project` reaches everyone working in the same repository), topic-based (`--to topic:repo/reviews`, subscribed with MQTT-style patterns like `repo/#`), or broadcast (`--to all`). Direct and broadcast messages to an agent without a live subscription queue in its inbox, so polling agents (hooks, cron-style loops) and streaming agents both work. Payloads are JSON with a free-form `kind` (`chat`, `task`, `handoff`, `question`, `answer`, `notice`), so agents on different models can agree on a vocabulary without the daemon caring.

## Architecture

```
┌────────────────────────────── host ──────────────────────────────┐
│                                                                  │
│   claude-code ─┐                                 ┌─ agentdocker  │
│   codex ───────┤   NDJSON over Unix socket       │   (CLI)       │
│   gemini-cli ──┼──────────────►  agentd  ◄───────┤               │
│   custom ──────┘                   │             └─ MCP adapter  │
│                                    │                  (planned)  │
│               ┌────────────────────┼─────────────────────┐       │
│               │  registry   supervisor   bus   leases    │       │
│               │  inboxes    events       logs            │       │
│               └──────────────────────────────────────────┘       │
└──────────────────────────────────────────────────────────────────┘
```

Three crates:

- `crates/core` — `agentdocker-core`: the data model, the wire protocol, and the pure coordination logic (`LeaseTable`, `Registry`, topic matching). No I/O, no clocks: every operation takes `now`, so it is fully unit-tested.
- `crates/agentd` — the daemon: Unix-socket server, process supervisor with log capture, broadcast bus, inbox queues, lease reaper, event stream, SQLite write-through store so state survives restarts.
- `crates/cli` — `agentdocker`: a thin client over the same protocol, plus the adapters: `agentdocker mcp` (stdio MCP server) and `agentdocker hook` (Claude Code hooks).

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) covers the protocol, lease semantics, delivery guarantees, and the design of the phases below.

## Roadmap

The thesis: Docker's moat was a layered filesystem plus namespaces. AgentDocker's is the **working set** — the daemon observes what every agent read, holds, changed, and which branch it is on, and derives from that what no single agent can: grouping, staleness, attribution, deadlock, handoff. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md#roadmap) has the engineering detail for every item.

- **Phase 0 — local control plane** *(done)*: daemon, registry, `run`/`stop`/`logs`, direct/topic/broadcast messaging with inboxes, leases with TTL and hierarchy, event stream, CLI.
- **Phase 1 — adapters & persistence** *(done)*: ✅ SQLite-backed state so agents, leases, inboxes, and event history survive daemon restarts (`events --replay`); ✅ `agentdocker mcp`, an MCP server so any MCP-capable agent gets the registry, messaging, and leases as tools without integration work; ✅ `agentdocker hook`, a Claude Code hooks pack (auto-register, claim before edit, deliver messages, release on stop); ✅ `Agentfile.toml` + `agentdocker up`/`down` for multi-agent teams; ✅ `claim --wait`.
- **Phase 2 — native install & projects**: a native per-user daemon with a launchd/systemd service and lazy start from any client; agents grouped automatically by the repository they work in (worktrees included), a `project:` message destination, discovery and adoption of agent processes that never registered, and each agent's branch in `ps`.
- **Phase 3 — the working set**: read-set tracking and a project watcher, so an agent is told when something it read has changed and by whom; an attribution ledger (`blame`); a per-project change journal that hands newcomers a "since you joined" digest instead of the event stream.
- **Phase 4 — layers, sandboxes & handoff**: a worktree per agent as its writable layer, with `diff`, `commit`, and `overlap` (merge conflicts before they happen); `docker`/`podman` runtimes for sandboxed agents; handoff bundles the daemon assembles from what it already knows.
- **Phase 5 — arbitration, humans & policy**: deadlock detection on contested leases, the human as a first-class agent with `ask`/`answer` and desktop notifications, admission policy and token budgets on the lease primitive, restart policies, `depends_on`, and a `top` dashboard.
- **Phase 6 — federation**: `agentd` peers across laptop, cloud, and phone over authenticated channels with a global `host/agent` namespace; project fingerprints make one repository one project everywhere.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Set `RUST_LOG=debug` for verbose daemon logging and `AGENTDOCKER_HOME` to point the daemon and CLI at an alternate directory (handy for running several isolated daemons). Pull requests are reviewed by CI and [CodeRabbit](.coderabbit.yaml).

## License

MIT — see [LICENSE](LICENSE).
