# AgentDocker

**Local orchestration for AI agents.** A native daemon that creates, supervises, organizes and connects agents on your computer, across models and vendors.

The product is moving toward an installed desktop app for macOS, Linux and Windows: open the app to see active agents, installed CLIs and desktop applications, their projects and supported coordination actions. The GUI will use operating-system local IPC; users will not need a localhost URL. Docker and Podman inspire lifecycle and organization, and remain optional execution adapters.

**Available today:** the native daemon and CLI on macOS and Linux, process discovery on demand, hooks/MCP adapters, project grouping, messaging, leases, a change journal, stale-context checks and verified handoffs. **Still to build:** the desktop GUI, installed-tool inventory, guided setup, automatic background discovery and Windows host support. See [product direction](docs/PRODUCT-DIRECTION.md) for the delivery order and capability boundaries.

## Install

From a checkout, install both binaries with Rust:

```sh
git clone https://github.com/brandopakel/AgentDocker.git
cd AgentDocker
cargo install --path crates/cli --locked
```

Ensure Cargo's binary directory is on your PATH. The daemon starts when a client needs it; `agentdocker daemon install` optionally starts it at login through launchd or systemd. The release installer requires a published release with checksums; source installation is the current trial path.

## Try native agents

```sh
agentdocker run --name writer --runtime custom -- sh -c 'sleep 300'
agentdocker run --name reviewer --runtime custom -- sh -c 'sleep 300'
agentdocker ps
agentdocker discover                    # inspect running agent processes
agentdocker claim --as writer src/ --note 'Updating the parser'
agentdocker send --from reviewer --to writer 'Let me know when src/ is free'
agentdocker inbox --as writer
agentdocker leases
agentdocker journal
agentdocker stop writer
agentdocker stop reviewer
```

These commands launch host processes directly. Discovery recognizes known runtime command lines; detecting a process does not reveal its model or grant access to its context. Configured hooks and MCP adapters provide richer coordination. Follow [getting started](docs/GETTING-STARTED.md) for installation, adapter setup and multi-agent teams.

## Coordinate work

Agents register with the same local daemon and group by repository, including linked worktrees. Time-limited leases coordinate file, directory, branch and task ownership; messages connect agents directly or by project and topic. Supported hooks and explicit MCP calls record reads so changed content can be detected before editing.

The journal records work, and checkpoints carry assumptions and validation evidence. A recipient reviews and acknowledges a handoff before offered leases move. Changed source or execution environment prevents acceptance of stale evidence. See [coordination and recovery](docs/COORDINATION.md).

## Documentation

- [Documentation index](docs/README.md)
- [Product direction and delivery order](docs/PRODUCT-DIRECTION.md)
- [Architecture and wire protocol](docs/ARCHITECTURE.md)
- [Optional Docker, Podman and Docker Desktop support](docs/CONTAINER-ENGINES.md)
- [Testing and benchmarks](docs/TESTING-AND-BENCHMARKS.md)

Two computers run independent local registries today. Federation is future work; the first second-Mac trial is a native single-machine installation.

## Development

The workspace separates pure coordination in `crates/core`, stateless host I/O in `crates/host`, daemon state and supervision in `crates/agentd`, and the CLI/adapters in `crates/cli`.

```sh
bash scripts/verify.sh check
```

Use a separate Cargo target directory for each worktree. `AGENTDOCKER_HOME` selects an isolated daemon state directory; `AGENTDOCKER_NO_AUTOSTART=1` disables client autostart. Long state paths use a short private socket directory, shown by `agentdocker daemon status`. See [coding conventions](CLAUDE.md) and the [verification standard](docs/TESTING-AND-BENCHMARKS.md). CI and CodeRabbit review changes before integration.

## License

MIT — see [LICENSE](LICENSE).
