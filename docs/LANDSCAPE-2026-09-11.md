# Landscape note: Dax and herdr (11 September 2026)

Read-only web research; neither product was installed. Sources are cited inline.
For the earlier herdr assessment see [Product direction](PRODUCT-DIRECTION.md).

## What they are

**Dax** ([getdax.app](https://getdax.app/)) is a closed-source macOS 14+ menu-bar
"productivity companion" with 21 global-shortcut tools, most unrelated to agents.
Three matter here. *Agent Watch* reads Claude Code trace files under `~/.claude`
and classifies sessions as Working / Needs You / Finished, optionally installing
hooks into `~/.claude/settings.json` for authoritative state. *Shepherd* is a
native window showing agents side by side; its documentation says it is powered
by herdr, which handles the terminals while Dax handles the window, sidebar and
installs ([docs](https://docs.getdax.app/tools/herdr.md)). *Open Project* and
*Vibe History* mine Claude and Codex traces to offer `--resume` commands. Free
tier and a $10/month Pro tier gate only the cloud AI tools; agent features are
local ([pricing](https://getdax.app/pricing/)). No public repository; the download
is a `.pkg`; the maker is unnamed on the site.

**herdr** ([herdr.dev](https://herdr.dev/), [GitHub](https://github.com/herdrdev/herdr),
Apache-2.0, Rust, ~37.7k stars, 331 open issues, created March 2026, $6M seed)
is a tmux-style server/client terminal multiplexer whose server owns PTYs and
survives client disconnect. Its distinction is agent awareness: per-agent TOML
*screen manifests* (`src/detect/manifests/`, 21 agents) regex-match the pane's
bottom buffer, terminal title and OSC progress sequences into
`idle / working / blocked / done / unknown`, with optional hooks and plugins
that report state over the socket. A newline-delimited JSON socket API
(`agent.prompt --wait`, `agent.wait --until blocked`, `pane.read`,
`events.subscribe`) plus a bundled `skills/herdr/SKILL.md` let one agent spawn a
sibling pane, start another agent, prompt it and block until it is idle. TUI
only; multi-machine over SSH; git worktrees; plugin marketplace keyed on GitHub
tags; stable releases roughly monthly with weekly previews.

## Feature matrix

| Capability | Dax | herdr | AgentDocker |
| --- | --- | --- | --- |
| Launch and supervise agents | Shepherd, via herdr | Yes, server-owned PTYs | Yes, supervisor and PTYs |
| Persistent terminals, attach | Via embedded herdr | Core; process hand-off across its own upgrades | Yes; no live hand-off yet |
| State detection | Trace files, optional Claude hooks (Claude only) | Screen manifests and lifecycle hooks, 21 agents | Hooks and MCP self-report; no screen scraping |
| Agent-to-agent messaging | No | Text typed into a PTY (`agent.prompt`) | Typed inbox, channels, `wait_for_messages` |
| Human inbox and notifications | Toasts with reply-from-toast, summaries, sounds | Toasts, per-agent sounds, suppressed for the focused tab | Questions, inbox, desktop notifications |
| File and resource leases | No | No | Yes |
| Review and hand-off between agents | No | No | Yes |
| Journal and audit | Read-only trace history | `session.json`, optional replay | Yes, commit-attributed |
| Multi-project | Yes, trace-derived | Workspaces, worktree groups | Yes |
| Desktop UI | Native macOS | None | Iced |
| CLI | No | Full | Yes |
| MCP and hooks | Claude hooks only | Hooks and plugins, no MCP | MCP and hooks |
| Distribution and updates | `.pkg` | curl, brew, binaries, self-update | Homebrew, installer, `desktop update` |
| License and price | Proprietary, $0 to $10/month | Apache-2.0 | Apache-2.0 |
| Platforms | macOS 14+ | macOS, Linux, Windows | macOS, Linux; Windows open |

## Worth taking

From herdr:

1. A blocking primitive: `agent.prompt` with `wait{until, timeout_ms}` and
   `agent.wait --until blocked|idle|done`
   ([socket API](https://herdr.dev/docs/socket-api)). Our `send_message` plus
   `wait_for_messages` could gain an `until: state` form.
2. Five states with `done` (finished, unseen) distinct from `idle` (finished,
   seen); focus turns one into the other ([concepts](https://herdr.dev/docs/concepts)).
   That is the "has the human looked yet" question our inbox has.
3. Data-driven detection manifests as a fallback for tools that have no hooks,
   hot-updated without restart ([agents](https://herdr.dev/docs/agents)).
4. A shipped `SKILL.md` teaching agents the safe pattern: verify the environment
   marker, take IDs from JSON, never assume focus, never close what you did not
   create.
5. Configurable sidebar rows with conditional styling ([configuration](https://herdr.dev/docs/configuration)).
6. A self-describing protocol (`api schema --json`) bundled in the binary.
7. Toast suppression for the focused tab, and terminal-side notifications over SSH.

From Dax:

1. Reply from the notification: type the answer and it reaches the agent
   ([Agent Watch](https://docs.getdax.app/tools/agent-watch.md)). Ours already has
   `answer_question`; the notification could carry the reply field.
2. Trace-file mining for zero-configuration project discovery and resume commands
   ([Vibe History](https://docs.getdax.app/tools/vibe-history.md)).
3. A per-project visual identity derived from the path hash, for dense lists.
4. An optional "install hooks" action framed as faster and more reliable.
5. An honest online/offline badge per tool.

## Where AgentDocker is different, and should say so

1. **Leases on shared files.** Neither product prevents two agents editing the
   same file. Lead with it.
2. **Typed messaging** rather than text typed into a PTY; herdr's issue tracker
   carries stalled-prompt and pending-launch reports from that path.
3. **Journal with commit attribution, review and hand-off.** Unmatched; Dax only
   reads traces.
4. **Vendor-neutral MCP surface.** herdr has no MCP; Dax is Claude-only.
5. **Reported state, not guessed state.** Hook-reported state is authoritative;
   screen scraping generates a steady stream of detection bugs upstream. Position
   as "agents tell us, we don't guess", and keep treating herdr as the terminal
   layer to embed or bridge rather than a competitor.

## Only learnable by installing

Whether herdr's process hand-off keeps PTYs alive across upgrades on macOS and
the latency of `agent.prompt --wait`; the real false-positive rate of its Claude
manifest in our sessions; how Dax's Shepherd integrates herdr (bundled version,
socket versus TUI), which permissions Dax requests (accessibility and screen
recording are undocumented), and how it updates; reply-from-toast reliability
across terminals; herdr's idle CPU with several clients; and whether a herdr
plugin can be driven from an AgentDocker hook (plugins receive the socket path).
