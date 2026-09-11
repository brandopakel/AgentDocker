# herdr bridge: measurements and design (11 September 2026)

Hands-on follow-up to the [landscape note](LANDSCAPE-2026-09-11.md). herdr 0.9.0
was installed with Homebrew and driven from a fixture: a private named session
(`herdr --session ad-probe`, never the user's default session) in a pty, one
real Claude Code 2.1.268 agent started in plan mode inside it, and every call
made on the raw Unix socket
(`~/.config/herdr/sessions/<name>/herdr.sock`, newline-delimited JSON,
`{"id","method","params"}` in and `{"id","result"|"error"}` out). The default
session's socket is `~/.config/herdr/herdr.sock`.

## What was measured

| Step | Result |
| --- | --- |
| Round trip for `pane.list`, `agent.list`, `workspace.list` | 0.1 to 0.7 ms |
| `agent.start --kind claude` until herdr reports `idle` | 3.5 s wall; `pane_agent_detected` to `idle` 3.3 s |
| `agent.prompt` with `wait` until `idle`/`done`/`blocked`, trivial reply | 2.2 s; `working` seen 0.6 s after submission, `idle` 1.6 s later |
| Same, unfocused pane and focused other workspace | 1.6 s and 2.3 s |
| Plan-mode approval dialog ("Would you like to proceed?") | reported `blocked`; `agent.prompt` while blocked refused with `agent_blocked`, no keys sent |
| `agent.send_keys` `esc` then `agent.wait` until idle | 0.11 s |
| `agent.wait` on a state that already holds | returns at once (0.0 s) |
| Event delivery (`events.subscribe`) versus the request's return | same ~40 ms tick |
| herdr server footprint hosting one agent, one client | 26 MB RSS, 0.1 % CPU |
| `/exit` sent through `agent.prompt` | agent released within 1 s; pane back to `unknown` |

Detection for Claude Code and Codex is **screen manifest only** even with the
hook integration installed: herdr's own table marks their hooks as `session`
(identity for restore), not lifecycle authority. The manifests are TOML rule
sets fetched at server start into
`~/.local/state/herdr/agent-detection/remote/*.toml` (Claude's was version
`2026.09.11.1`, refreshed the same day); a bundled copy is the fallback.
`agent.explain` returns every rule with its region preview, which made the
classification auditable: idle came from the `❯` prompt box, working from the
OSC title spinner and the `esc to interrupt` line, blocked from the
"Would you like to proceed?" form.

`done` versus `idle` is a **server-side seen flag**, not a different agent
state. A turn that finished while its pane was visible in the attached client
reported `idle` immediately, whether or not the pane had keyboard focus. A turn
that finished while another workspace was focused reported `done`;
`agent.read` left it `done`, `agent.focus` turned it into `idle`. Each TUI client
keeps its own badge; the socket reports the server's view.

`notification.show` returned `shown:false, reason:"disabled"` because
`ui.toast.delivery` defaults to `off`; the docs state that herdr suppresses
popups for the active tab when delivery is on. We did not exercise that path.

Environment injected into every pane process: `HERDR_ENV=1`,
`HERDR_SOCKET_PATH`, `HERDR_BIN_PATH`, `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`,
`HERDR_PANE_ID`, and `HERDR_SESSION` for a named session. During the probe
`agentdocker ps` already showed the herdr-launched Claude session as
`LIVES IN herdr:w1:p1`, registered through our Claude Code hooks like any other
session. That column comes from `agentdocker_core::multiplexer::from_environment`,
which keyed only on `HERDR_SESSION`; in the default (unnamed) session that
variable is absent and only the weaker ancestry evidence would remain, so the
detector now also accepts `HERDR_ENV=1` with `HERDR_PANE_ID`.

## Where a bridge is worth it

AgentDocker stays "agents tell us, we don't guess": our state comes from hooks
and MCP, not from screens. herdr is the terminal layer. The bridge is therefore
read-mostly and optional, and never a dependency.

1. **Pane identity (done).** When an agent lives in a herdr pane we already
   record `herdr:<pane>`. Keep it; it is the join key for everything below.
2. **Focus in herdr.** A "Show terminal" action on an agent that lives in a
   herdr pane calls `agent.focus`/`pane.focus` on the socket named by that
   agent's `HERDR_SOCKET_PATH` (or the default socket). This is the only write
   we need, it marks the completion seen on herdr's side too, and it is the
   herdr equivalent of Dax's reply-from-notification landing in the right
   terminal. Reply itself stays typed messaging; when our reply cannot reach
   the agent through MCP or a channel, `agent.prompt` on the pane is the
   fallback transport and it refuses safely while the agent is blocked.
3. **Blocked mirror.** Subscribe to `pane.agent_status_changed` for the panes
   our agents live in and surface `blocked` as attention when we have no
   `ask_human` question open for that agent. It closes the gap our hooks
   cannot see (permission dialogs, plan approval) without adopting screen
   scraping ourselves. Show it as "herdr reports a prompt", not as our own
   state.
4. **Detection manifests as fallback, not authority.** Our `check_stale`
   and idle markers stay hook-driven. herdr's manifest result is displayed as
   a second opinion only when the agent is in a herdr pane and our own data
   is older than the herdr observation.
5. **Nothing else.** `layout.apply`, worktree management, `pane.split`, and
   `agent.start` are herdr's product; our supervisor and worktree commands
   already exist and must not grow a second code path behind herdr.

## Protocol facts a client must respect

- `events.subscribe` needs an explicit `subscriptions` list; per-pane events
  (`pane.agent_status_changed`, `pane.scroll_changed`) require `pane_id`.
  Lifecycle subscriptions do not replay history: open the subscription, then
  call `session.snapshot` on another connection and apply buffered events.
- `agent.read` requires `source` (`recent`, `visible`, `detection`).
- `agent.prompt` with `wait` returns `agent_prompt_stalled` when no
  `working`/`blocked` is observed within 5 s of submission; it does not track
  turns, so an already working agent may satisfy the wait early.
- `agent.wait` pins the pane occupant; a replacement process cannot satisfy it.
- `state_change_seq` on agent records is monotonic and is the right cursor
  for "have I already surfaced this completion".
- The server can be stopped from under a client (`herdr session stop <name>`);
  treat a vanished socket as "no herdr", never as an error the user sees.

## Not adopted, and why

- Embedding herdr (bundling the binary or its server) adds a 26 MB process and
  a second update stream for a terminal we do not draw. Bridging over the
  socket gives the same information at zero install cost when herdr is present.
- Writing our own screen manifests. The Claude manifest changed the day it was
  probed; that stream of detection fixes is herdr's job.
- Live server hand-off (`server.live_handoff`) is about herdr's own upgrades,
  not agent hand-off, and is experimental upstream.

## Fixture

The probe used three small scripts kept out of the repository: a pty runner for
`herdr --session <name>`, an `events.subscribe` logger with wall-clock
timestamps, and a one-shot request helper. Reproduce with `herdr --skill` for
the CLI shape and `herdr api schema --json` for the self-describing request
schema; the manifests under `~/.local/state/herdr/agent-detection/remote/`
show exactly which screen text herdr keys on.
