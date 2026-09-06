# Get started on your computer

The current daemon and CLI run natively on macOS and Linux. Docker and Podman are optional. The installed desktop GUI, automatic installed-tool inventory and Windows host support are planned; see [product direction](PRODUCT-DIRECTION.md).

## Install from source

Install Rust and Git, then run:

```sh
git clone https://github.com/brandopakel/AgentDocker.git
cd AgentDocker
cargo install --path crates/cli --locked
agentdocker daemon status
```

The install ships both `agentdocker` and `agentd`; ensure Cargo's binary directory is on your PATH. The daemon starts on demand when a client needs it. To start it at login, optionally run `agentdocker daemon install` (launchd on macOS, systemd user service on Linux). Use `agentdocker daemon status` to inspect the service and socket paths.

The [release installer](../install.sh) requires a published release and matching SHA-256 checksum. Building from source is the current trial path. Two computers have independent daemons and registries; connecting them requires future federation support.

## Try native agents

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
agentdocker changes               # the ledger: files that changed in this project, and who held each
agentdocker blame src/parser.rs   # the same for one file
agentdocker journal               # what happened and why, one line per release, note, or commit
agentdocker release --as writer --all --summary "rewrote the tokenizer"   # your line in the journal

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

Host processes started with `agentdocker run` get `AGENTDOCKER_SOCKET`, `AGENTDOCKER_AGENT_ID`, and `AGENTDOCKER_AGENT_NAME` in their environment, so inside an agent the CLI already knows who it is:

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

MCP exposes voluntary coordination tools. Automatic denial requires the Claude Code hooks below and applies to their covered edit tools; shell/script writes are not guarded by that matcher. Hooks fail open if coordination is unavailable.

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


## Next steps

- [Coordination and recovery](COORDINATION.md): leases, stale context, checkpoints, handoffs and isolated worktrees.
- [Optional container engines](CONTAINER-ENGINES.md): Docker, Podman and Docker Desktop transports.
- [Documentation index](README.md): protocol, product direction and verification.
