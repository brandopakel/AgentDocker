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
3. A per-project visual identity derived from the path hash, for dense lists (present: `crates/ui/src/app/style.rs`).
4. An optional "install hooks" action framed as faster and more reliable (present: Tools **Set up**).
5. Per-tool status badges (present: Connected / Configured / Needs setup / Not installed; independent input-capability verification remains open).

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

## Paprika (added 17 September 2026)

Read-only web research from [paprika.ai](https://paprika.ai/) and
[docs.paprika.ai](https://docs.paprika.ai/); nothing was installed or signed up
for. "Kanban for people and agents": a hosted board where humans and agents are
both actors. An agent is an owned bot identity with a `papagt_` bearer token.
By default it follows its creator across workspaces and inherits the creator's
current workspace memberships and project access; explicit restrictions can
only reduce that access. Any future bridge identity must restrict both its
workspaces and projects explicitly. Agents are reached over streamable-HTTP MCP
at `mcp.paprika.ai`
([MCP](https://docs.paprika.ai/mcp/), [agents](https://docs.paprika.ai/agents/));
Claude Code, Codex, Cursor, Copilot, Grok Build and Antigravity are the named
hosts. The *agent workflow* board template has nine columns — Backlog, Approved,
Analyst, In progress, Testing, UAT, Done, Blocked, Cancelled — each with an owner
hint of `user`, `agent` or `any` ([boards](https://docs.paprika.ai/boards/)).
Approved is the pull source: `kanban_pull` moves a card to the pull target and
assigns it to the caller, and fails if another actor already holds it — "two
agents cannot take the same card; that is the whole point"
([cards](https://docs.paprika.ai/cards/)). A card carries acceptance text
("what done means; agents should read this before moving the card"), typed
links (URL, PR, File — a path the agent opens on its own machine, Memory — a
note for the next agent), blockers on other cards, a parent, a plan and files;
history is one event log (create, move, comment, assign, pull, …) that agents
and humans read alike. Plans are spec documents next to the cards that implement
them (`plan_list/get/create/update/archive`); Files are project artifacts
(`artifact_list/get/put/complete/delete/link_card`)
([plans](https://docs.paprika.ai/plans/), [files](https://docs.paprika.ai/artifacts/)).
Automations are webhooks (generic or Slack-shaped, signed) and rules that comment
or move cards on an event or after N idle days; "rules never run host commands"
([automations](https://docs.paprika.ai/automations/)). A `paprika` CLI with
`--json` on every command and documented exit codes talks the same API
([CLI](https://docs.paprika.ai/cli/)). Web dashboard, Android app, iOS "coming
soon"; seats are humans, agents are a plan quota (Free: 1 agent, 5 boards;
Standard $4.99/human/month and Teams $10/human/month when billed annually
(monthly billing: $5.99 and $12 respectively); Standard allows 3 agents and
Teams 5 agents per human; Enterprise has SSO and audit)
([pricing](https://paprika.ai/pricing/), checked September 17, 2026 UTC). Closed source,
hosted only; no self-hosting or data-location statement was found.

### How it differs from AgentDocker

Paprika is the **board**: what the work is, who holds it, what done means, the
spec and the artifacts, in the cloud, for a team. AgentDocker is the **floor**:
the processes on one machine, their terminals and supervision, leases on the
files they touch, typed messages between them and the person, questions that
block, a journal attributed to commits, review and hand-off, and the daemon's
own idea of who is live. Paprika does not know what an agent is doing to a
checkout, cannot stop two agents editing one file, runs nothing on the machine
and reaches an agent only when the agent calls in; AgentDocker had no board
when this was written (it has one now — the first item below, in source), and
still has no spec documents beside the work, no roles, no webhooks and no
mobile client. The overlap is coordination vocabulary: Paprika's
pull is our `claim` on a `task:<name>` lease (both atomic, both refuse a second
taker), its hand-off is a column move where ours is a bundle to a named agent,
its comments are our channel, its Memory link is our `journal_note`.

### Worth taking

1. **A card with acceptance text and an atomic pull**, as a first-class shape
   over the `task:` lease we already have: title, what done means, a column,
   an assignee; `task pull` claims it or refuses. A Board tab per project in
   the app (Backlog · Ready · In progress · Review · Done) would put the work
   beside the sessions doing it, and the person could file work without
   opening a terminal. Local, journaled, no cloud.
2. **Owner hints and roles** (`Analyst`, `Implementer`, `Reviewer`) as agent
   labels a hand-off can name, so "send this to the reviewer" resolves.
3. **Typed links on a message or hand-off**: PR, path, memory-for-the-next-agent.
   The journal note and commit attribution are the data; the link type is the
   affordance.
4. **Webhooks on the event stream** (generic and Slack-shaped, signed) so a
   team channel hears `question_asked`, `agent_exited`, `lease_deadlock`.
   `agentdocker events` already streams; a sink is small.
5. **Rules that only comment or move**, never run commands — the same line we
   draw around policies.
6. **`--json` everywhere with documented exit codes** for agents driving the
   CLI; ours has `--json` on most commands and no exit-code contract yet.

### Combining rather than competing

An agent can sit on both: Paprika tells it what to do next, AgentDocker helps it
coordinate overlapping work with the others. An optional bridge, like the
proposed herdr one, would work as follows: when an agent pulls Paprika card `T-7`, take the
`task:paprika/T-7` lease here with the card's title as the note (so `agentdocker
leases` shows who holds which card); when the card moves to Review, open a
channel with the reviewer; post `agentdocker` journal commits back as card
comments through Paprika's MCP. Nothing in that needs Paprika's cooperation
beyond its public tools. Not started.

A task lease has a TTL. The holder must renew it during long-running work;
without renewal it expires and no longer excludes another local claimant.
Exclusion applies only while a valid exclusive lease is held, and the agents
must honor that coordination contract. It does not prevent arbitrary filesystem
writes or make a remote card update atomic with a local lease.

The proposed bridge needs explicit reconciliation before work or renewal: read
both the card's current assignee/state and the local lease. If they disagree,
stop new work and report the mismatch instead of claiming ownership from either
one alone. A card still assigned to an ended agent after its lease expires is
stale; require confirmed card reassignment (or the person's explicit recovery)
and a fresh successful lease claim before another agent starts. If the card was
reassigned or closed while a local lease survives, release the old claim and
report the transition. A lost reply enters an explicit uncertain state: stop work and automatic retries.
Reading both states is diagnostic, not sufficient authority to replay a write.
Recovery must use an idempotent or conditional operation tied to the original
assignment, or a confirmed compensating action, before obtaining a fresh lease.
Only then may work resume; recovery cannot duplicate assignment or renew an
expired claim. These are acceptance requirements for the optional proposal,
not delivered integration behavior.

### Adoption status of the September 11 notes

From herdr: multiplexer adapters ship (row 25: a herdr, tmux, screen or zellij
session is recognised at registration and shown in `ps`); the focus/prompt
bridge and blocked-state mirror are designed and measured in
[HERDR-BRIDGE.md](HERDR-BRIDGE.md) and deferred; a shipped SKILL.md exists
(the portable coordination skill). From Dax: per-project visual identity,
**Set up** for hooks and per-tool status badges are present; reply from the
notification and trace-file resume are not. From Paprika: the first item —
a card with acceptance text and an atomic pull over the `task:<id>` lease,
with a Board tab in the app — is in source (PR #176), as are roles (a `role`
label an agent is given, and `role:<name>` as the recipient of a message or
a hand-off); typed links, webhooks and the exit-code contract are in review,
comment-only rules are not started, and the card-to-lease bridge to Paprika
itself remains a proposal.

Assessed September 17, against the [product direction](PRODUCT-DIRECTION.md):
what is still worth taking is small and agent-facing — an exit-code contract
for the command line (agents drive the CLI; PR #181), typed links on a card,
message or hand-off (in source: `links` on cards, messages, checkpoints and
hand-off bundles, shown in the app) (a path, a PR, a memory note; the data already exists), webhooks
as a signed sink on the event stream (a team channel hears `question_asked`
and `lease_deadlock`), and roles as agent labels a hand-off can name (in
source: `agentdocker role`, `role:<name>` as a recipient). Reply
from the notification (Dax) is worth it once the routing acceptance above is
closed. Not worth taking: herdr's focus/prompt bridge and blocked-state
mirror (measured and deferred in [HERDR-BRIDGE.md](HERDR-BRIDGE.md); the
pane identity we show is the useful part), trace-file resume (a provider's
own concern), Paprika's spec documents, mobile client and hosted rules (the
floor is local), and the card-to-lease bridge until somebody runs both.
