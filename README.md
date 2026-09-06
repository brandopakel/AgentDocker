# AgentDocker

**Local orchestration for AI agents.** A native daemon that creates, supervises, organises, and connects agents on your computer, whatever model or vendor is behind them.

Coding agents are cheap to start and easy to lose track of. Run three of them against one repository and you get the same failure modes distributed systems solved decades ago: two agents editing the same file, an agent reasoning about context another agent just invalidated, and no shared channel to say "I've got this one" or "here's what I found". AgentDocker gives agents the primitives to coordinate, using the shape everyone already knows from containers.

It is bare metal: a native per-user daemon and a native CLI talking over a Unix socket. Nothing is served over HTTP, nothing needs a browser, and nothing needs Docker — a container is one optional way to sandbox an agent, not the product.

> Status: **alpha, single host.** The daemon, CLI, MCP server, Claude Code hooks, persistence, projects, leases, messaging, the working set (ledger, staleness, journal), worktrees and handoff all work end to end on macOS and Linux. Still to come: a native desktop app, discovery of the agent tools installed on a machine and one-command setup for each, Windows, and federation across machines. See the [product direction](docs/PRODUCT-DIRECTION.md) and [roadmap](#roadmap).

## The Docker analogy

| Docker | AgentDocker | What it is |
|---|---|---|
| image | **agent spec** | How to launch an agent: runtime, model, command, workdir, env, labels |
| container | **agent** | One running instance with an id, name, status, pid, and logs |
| `dockerd` | **`agentd`** | Per-host daemon: registry, supervisor, message bus, lease arbiter, watcher, event log |
| `docker` CLI | **`agentdocker`** | `ps`, `run`, `stop`, `logs`, `inspect`, `send`, `watch`, `claim`, `journal`... |
| network | **messages & topics** | Direct, project-wide, topic (pub/sub), and broadcast messaging with offline inboxes |
| volume lock | **lease** | Time-limited exclusive/shared claim on a file, directory, branch, task, or anything |
| compose project | **project** | Derived from the working directory: the repository (every worktree of it), else the directory. Agents group by it automatically |
| layer | **worktree** | An agent's own writable checkout (`run --isolate`), integrated when validated |
| `docker events` / `logs` | **events** / **journal** | A live stream of everything the daemon does, and a per-project narrative of what changed and why |
| `docker export` | **handoff bundle** | Everything the daemon knows about an agent's work, handed to another agent or another machine |

Agents don't need an SDK. Anything that can write a line of JSON to a Unix socket — a shell hook, a Python script, an MCP tool call — is a first-class participant. That is what makes it model- and vendor-agnostic: Claude Code, Codex, Gemini CLI, Cursor, and hand-rolled agents all coordinate through the same daemon.

## Install

```sh
cargo install --git https://github.com/brandopakel/AgentDocker --rev <commit> agentdocker --locked   # from source on any machine with Rust, pinned to a commit you have looked at (--tag vX.Y.Z once one exists)
cargo install --path crates/cli --locked                                                             # from a checkout: both binaries
curl -fsSL https://raw.githubusercontent.com/brandopakel/AgentDocker/main/install.sh | sh          # release binaries into ~/.local/bin, once a release is published
agentdocker daemon install    # optional: run agentd as a login service (launchd / systemd)
```

Pin the source: the default branch moves, and `--locked` pins dependencies, not the application. Release binaries require a published GitHub release; the installer checks the archive against the SHA-256 published with that release (it does not verify a signature). `cargo install agentdocker` becomes available after the crates are published. Homebrew formulae are generated from release checksums, not placeholder hashes.

The daemon starts on demand the first time a client needs it, so the last step is only for surviving reboots. `agentdocker daemon status` shows what is running and where.

Then wire in the agents you already have:

```sh
agentdocker runtimes          # what is installed: Claude Code, Codex, Gemini CLI, Cursor, ... — CLI, version, app, and whether AgentDocker is wired in
agentdocker setup             # register the MCP server with each, install the Claude Code hooks (--dry-run shows what would change)
agentdocker discover          # agent processes running right now that nobody registered; `adopt --all` brings them in
```

The daemon keeps scanning for agent processes on its own and announces them as `agent_discovered` and `agent_vanished` events, so nothing has to be typed for a running Claude Code or Codex session to show up.

## Quick start

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"

# 1. There is no step 1: the first command that needs the daemon starts it
#    (listening on ~/.agentdocker/agentd.sock, logging to ~/.agentdocker/agentd.log).

# 2. Launch two agents. Any command works; here they are shell loops.
agentdocker run --name writer   --runtime custom -- sh -c 'sleep 300'
agentdocker run --name reviewer --runtime custom -- sh -c 'sleep 300'
agentdocker ps                    # grouped by project, with each agent's branch and head
agentdocker ps --project .        # only agents in this project
agentdocker discover              # agent processes running outside AgentDocker; `adopt <pid>` registers one

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

# 5. See what happened.
agentdocker changes               # the ledger: files that changed in this project, and who held each
agentdocker blame src/parser.rs   # the same for one file
agentdocker journal               # what happened and why, one line per release, note, or commit
agentdocker release --as writer --all --summary "rewrote the tokenizer"   # your line in the journal
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

## Adapters

### Give any MCP-capable agent the tools directly

`agentdocker mcp` is an MCP server over stdio. Point a host at it and its model gets `list_agents`, `send_message`, `read_inbox`, `wait_for_messages`, `claim`, `renew`, `release`, `list_leases`, `inspect_agent`, `whoami`, the working-set tools (`observe_paths`, `check_stale`, `read_set`, `read_journal`, `journal_note`, `overlap`), and the recovery tools (`save_checkpoint`, `resume_checkpoint`, `handoff`, `validate`), plus instructions on when to use them. The server registers the host as an agent when it starts (named `<runtime>-<pid>` unless you pass `--name`) and deregisters when the host closes it; if the host was itself started by `agentdocker run`, the existing identity is reused.

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

MCP exposes voluntary coordination tools. Automatic denial requires the Claude Code hooks below and applies to their covered edit tools; shell/script writes are not guarded by that matcher. Hooks fail open if coordination is unavailable.

### Claude Code: hooks make it automatic

The MCP server gives the model tools it *may* call. Hooks make coordination happen whether or not it thinks to:

```sh
agentdocker hook install claude-code          # writes ./.claude/settings.json (or --user for ~/.claude)
```

| Claude Code event | what the hook does |
|---|---|
| `SessionStart` | registers the session as agent `claude-<session>`; tells the model who else is running, hands it queued messages and the project journal since it last looked |
| `PreToolUse` on Read/Grep/Glob | records what is about to be read, so a later change to it is noticed |
| `PreToolUse` on Edit/Write/MultiEdit/NotebookEdit | refuses the edit if the file changed since it was read; otherwise claims `path:<file>` first, and if another agent holds it, the edit is **denied** with the holder's name and note |
| `UserPromptSubmit`, `PostToolUse` | delivers messages from other agents as context, as they arrive; a prompt also carries new journal entries |
| `Stop` | releases every lease, quoting the model's last message as the journal summary; if messages arrived while it was working, blocks the stop so the model reads them first (`--no-wake` disables) |
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
isolate = true                        # its own worktree and branch

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

## The working set

The daemon observes what every agent read, holds, changed, and which branch it is on, and derives from that what no single agent can.

### Stale context

Before reading files, record what you are about to inspect:

```sh
agentdocker observe --as reader src/lib.rs
# Read src/lib.rs with your normal tool.
agentdocker stale --as reader src/lib.rs
agentdocker reads --as reader
```

`stale` exits unsuccessfully when recorded content changed, including uncommitted edits that leave HEAD unchanged. Observe and reread the reported paths before editing. The Claude Code hooks do this for supported tools; MCP clients use `observe_paths` and `check_stale` explicitly. Read sets persist across daemon restarts and stay specific to the agent's physical checkout.

### The journal

Where the ledger records every file change, the journal records what happened and why, one line per release, note, commit, arrival, or handoff, per project. Every reader has a cursor, so `agentdocker journal --new` shows what you have not looked at yet, a starting Claude Code session is handed what it missed, and a resumed session is not told everything twice.

### Worktrees and overlap

`agentdocker run --isolate --name writer -- codex …` launches an agent in a linked worktree of its own (branch `agent/writer`, under the daemon home), or `agentdocker worktree-create --as writer ../agent-work --branch agent/work` creates an independent checkout by hand. `agentdocker overlap` lists the paths that more than one checkout has changed — merge conflicts before they happen — and `--as writer` narrows it to one agent's checkout. Commit the source's changes and run `validate`; `integrate --as writer ../agent-work --validation <id>` previews integration and `--apply` prepares an uncommitted merge to review with Git.

### Resume with verified context

```sh
agentdocker validate --as worker -- cargo test --workspace
agentdocker checkpoint --as worker parser-step --task "Fix the parser" \
  --assumption "Inputs use UTF-8" --next "Review boundary cases" --release-leases
agentdocker checkpoints
agentdocker resume --as replacement <checkpoint-id>
agentdocker resume --as replacement <checkpoint-id> --acknowledge
```

Review the returned assumptions, stale paths, and matching validation evidence before accepting. Changed content blocks acceptance; re-establish the affected context and save a new checkpoint. Acceptance persists across restart and binds the handoff to one replacement session. Validation records identify the code before and after execution and retain the command's log; changed code, failed checks, timeouts, and surviving subprocesses do not count as passing evidence.

### Hand work to another agent

```sh
agentdocker handoff reviewer --as worker --task "Finish the parser" --note "tests are in src/parser.rs" --transfer-leases
agentdocker handoffs --as reviewer
agentdocker resume --as reviewer <handoff-id> --acknowledge
agentdocker export --as worker > bundle.json      # carry it to another host by hand
agentdocker import --as replacement < bundle.json
```

A handoff is a checkpoint addressed to someone, with everything the daemon already knows about the sender bundled around it: the leases it holds, what it read and at which versions, the changes it made, its uncommitted diff when it worked in a worktree, the messages it never read, and its journal entries. The recipient is told by message and accepts with `resume --acknowledge`; that is when ownership moves — leases transfer if the sender asked, the read set is seeded so staleness carries over, and the recipient continues reading the project journal where the sender stopped. An exported bundle imported on another host is accepted the same way, once the content matches; leases never cross hosts.

## Sandboxes (optional)

An agent can optionally run in an image with no networking or host mounts by default. `--mount-checkout` adds its checkout and a scoped, authenticated coordination endpoint. Docker Desktop uses a private engine-volume relay; Podman VMs use the selected machine transport. `agentdocker grant-access --as writer --container-root /workspace --token-file /private/path/token` issues the credential; containers receive only the separate `container.sock`, the token file, and their mapped checkout, and set `AGENTDOCKER_SOCKET`, `AGENTDOCKER_TOKEN_FILE`, and `AGENTDOCKER_AGENT_ID` inside. The host control socket is never mounted. Docker and Podman are supported as engines for this, including image builds with recorded provenance and container supervision across daemon restarts; none of it is required to use AgentDocker. See [`docs/CONTAINER-ENGINES.md`](docs/CONTAINER-ENGINES.md).

## What it solves

**Race conditions.** A lease is an exclusive or shared claim on a *resource key* such as `path:/repo/src`, `branch:feature/x`, or `task:ISSUE-42`. Path keys are hierarchical, so a lease on a directory covers every file beneath it, and file protection uses canonical physical paths, so aliases and agents from different projects cannot obtain separate exclusive claims on one checkout. Separate worktrees can edit independently; logical project-relative paths support cross-worktree overlap analysis. Every lease has a TTL, so a crashed agent can never wedge the system, and the daemon releases held leases when exit is observed. A stop request reports `stopping` and retains protection until then. A refused claim tells the requester exactly who holds what and the note they left.

**Lost context.** The registry makes participating agents visible; leases carry notes about their work. The daemon records best-effort file-change attribution through unexpired exclusive physical leases, otherwise marks a change external. Durable read sets let supported hooks and explicit MCP calls detect changed content, including uncommitted edits, and require rereading before an edit. The journal hands a newcomer what happened while it was away. Generic adopted processes are not automatically observed.

**No common channel.** Messaging is direct (`--to writer`), project-wide (`--to project` reaches everyone working in the same repository), topic-based (`--to topic:repo/reviews`, subscribed with MQTT-style patterns like `repo/#`), or broadcast (`--to all`). Direct and broadcast messages to an agent without a live subscription queue in its inbox, so polling agents (hooks, cron-style loops) and streaming agents both work. Payloads are JSON with a free-form `kind` (`chat`, `task`, `handoff`, `question`, `answer`, `notice`), so agents on different models can agree on a vocabulary without the daemon caring.

## Architecture

```
┌────────────────────────────── host ──────────────────────────────┐
│                                                                  │
│   claude-code ─┐                                 ┌─ agentdocker  │
│   codex ───────┤   NDJSON over Unix socket       │   (CLI)       │
│   gemini-cli ──┼──────────────►  agentd  ◄───────┤               │
│   custom ──────┘                   │             ├─ MCP adapter  │
│                                    │             └─ hooks        │
│               ┌────────────────────┼─────────────────────┐       │
│               │  registry   supervisor   bus   leases    │       │
│               │  inboxes    events       logs  watcher   │       │
│               │  ledger     journal      store           │       │
│               └──────────────────────────────────────────┘       │
└──────────────────────────────────────────────────────────────────┘
```

Four crates:

- `crates/core` — `agentdocker-core`: the data model, the wire protocol, and the pure coordination logic (`LeaseTable`, `Registry`, topic matching, the journal, handoff bundles). No I/O, no clocks: every operation takes `now`, so it is fully unit-tested.
- `crates/host` — host filesystem, process, Git, and container-engine inspection shared by both binaries.
- `crates/agentd` — the daemon: Unix-socket server, process supervisor with log capture, broadcast bus, inbox queues, lease reaper, project watcher, event stream, SQLite write-through store so state survives restarts.
- `crates/cli` — `agentdocker`: a thin client over the same protocol, plus the adapters: `agentdocker mcp` (stdio MCP server) and `agentdocker hook` (Claude Code hooks).

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) covers the protocol, lease semantics, delivery guarantees, and the design of the phases below; [`docs/IMPLEMENTATION-NOTES.md`](docs/IMPLEMENTATION-NOTES.md) records the contracts and hardening decisions behind what exists.

## Roadmap

The thesis: Docker's moat was a layered filesystem plus namespaces. AgentDocker's is the **working set** — the daemon observes what every agent read, holds, changed, and which branch it is on, and derives from that what no single agent can: grouping, staleness, attribution, deadlock, handoff. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md#roadmap) has the engineering detail for every item.

- **Phase 0 — local control plane** *(done)*: daemon, registry, `run`/`stop`/`logs`, direct/topic/broadcast messaging with inboxes, leases with TTL and hierarchy, event stream, CLI.
- **Phase 1 — adapters & persistence** *(done)*: SQLite-backed state so agents, leases, inboxes, and event history survive daemon restarts; `agentdocker mcp`; `agentdocker hook` for Claude Code; `Agentfile.toml` with `up`/`down`; `claim --wait`.
- **Phase 2 — native install & projects** *(done)*: a native per-user daemon with a launchd/systemd service and lazy start; agents grouped by the repository they work in (worktrees included); `project:` messaging; discovery and adoption of running agent processes; each agent's branch in `ps`.
- **Phase 3 — the working set** *(done)*: read sets and the project watcher, so an agent is told when something it read has changed and by whom; the attribution ledger (`blame`); the per-project journal with cursors and digests.
- **Phase 4 — layers, sandboxes & handoff** *(implemented)*: a worktree per agent (`run --isolate`), `overlap`, validated integration; handoff bundles with lease transfer and `export`/`import`; container sandboxes with scoped credentials, and Docker/Podman as optional engines.
- **Phase 5 — the machine and the human** *(next)*: an inventory of the agent tools installed on the machine and one-command `setup` that wires each into the daemon; the daemon watching for running agents on its own; a native desktop app (`agentdocker-ui`, pure Rust, over the same socket) showing agents, leases, the journal and events, with notifications; the human as a first-class agent with `ask`/`answer`; deadlock detection; policy and quotas; restart policies and `depends_on`.
- **Phase 6 — Windows and federation**: named pipes and a Windows service so the same daemon runs there; then `agentd` peers across laptop, cloud, and phone over authenticated channels with a global `host/agent` namespace, with project fingerprints making one repository one project everywhere.

## Development

```sh
bash scripts/verify.sh check     # the PR gate: fmt, clippy, nextest, doctests, installer tests, packaging, release build
cargo test --workspace
```

Set `RUST_LOG=debug` for verbose daemon logging and `AGENTDOCKER_HOME` to point the daemon and CLI at an alternate directory (handy for running several isolated daemons; a home whose path is too long for a Unix socket name gets its sockets in a short private directory automatically — `agentdocker daemon status` shows where). Clients start `agentd` on demand; `AGENTDOCKER_NO_AUTOSTART=1` makes them fail instead, and `agentd` run by hand in the foreground still works. Pull requests are reviewed by CI and [CodeRabbit](.coderabbit.yaml); see the [testing and benchmarking standard](docs/TESTING-AND-BENCHMARKS.md).

## License

MIT — see [LICENSE](LICENSE).
