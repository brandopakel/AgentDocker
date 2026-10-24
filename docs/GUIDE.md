# Using AgentDocker

Everything the product does, in the order you meet it: install it, wire in
the agents you already run, watch them from the app, and reach for the CLI
when you want more than the app shows.

If you want the reasoning instead of the instructions, read
[ARCHITECTURE.md](ARCHITECTURE.md). This page is the manual.

- [Install and first run](#install-and-first-run)
- [The desktop app](#the-desktop-app)
- [The console](#the-console)
- [Command reference](#command-reference)
- [MCP tools](#mcp-tools)
- [Hooks](#hooks)
- [Tutorials](#tutorials)
- [Behaviour by area](#behaviour-by-area)

---

## Install and first run

```sh
curl -fsSL https://raw.githubusercontent.com/brandopakel/AgentDocker/main/install.sh | sh
```

On a Mac that installs the desktop app — with the `agentdocker` command and
the daemon inside it — through the app's own installer, so `agentdocker desktop
update` and rollback work from the first install; on Linux, or with
`AGENTDOCKER_INSTALL=cli`, it puts the two commands under `~/.local/bin`. A
release that has no desktop archive falls back to the commands and says so.
An app not signed with a Developer ID is quarantined by macOS when downloaded:
right-click it and choose **Open** once, or
`xattr -dr com.apple.quarantine ~/Applications/AgentDocker.app`. From a checkout:

```sh
cargo install --path crates/cli --locked   # agentdocker, which is also the daemon
cargo install --path crates/ui  --locked   # the desktop app
```

`agentdocker` and `agentd` are one executable: invoked as `agentd` it is the
daemon, and packages ship `agentd` as a link to `agentdocker`. A `cargo install`
makes only `agentdocker`; the daemon still starts on demand, as
`agentdocker daemon run`. `agentdocker daemon install` names `agentd` in the
service it writes, so add the link first:
`ln -s agentdocker "$(dirname "$(command -v agentdocker)")/agentd"`.

The Linux desktop archive needs a graphical session and its native display
libraries (X11/XCB, keyboard, rendering and fonts); the archive does not install
distribution packages. Before launching an extracted archive, run `ldd` on its
`agentdocker-ui` executable and resolve any `not found` dependencies for your
distribution. The CLI-only installer does not install the desktop or those
libraries.

On Windows x64, the preview ZIP can run portably or install for your account.
From PowerShell in its extracted `AgentDocker` folder:

```powershell
.\agentdocker.exe desktop install --from . --local-preview
```

The installed app and CLI are under
`$env:LOCALAPPDATA\AgentDocker\desktop\bin`; open `agentdocker-ui.exe` there.
In that directory, use `.\agentdocker.exe desktop update --check` to check for a
preview, `desktop update --local-preview` to verify and preview it, and add
`--apply` to activate. `desktop rollback --local-preview` restores the retained
previous version. Updates apply on the next launch; running sessions keep their
current binaries. Windows uses its own preview channel, separate from the stable
and Mac/Linux feeds. It is unsigned and requires explicit preview consent.

`desktop prune --preview` reports removable old builds; omit `--preview` to remove
them. `desktop uninstall --preview` reports launcher removal; omit `--preview` to
apply. Both preserve settings, running versions and unrecognized files.
Uninstall leaves the tools' configuration alone: run `agentdocker setup --remove`
first, or Claude Code, Codex and the rest keep MCP entries and hooks that point
at the removed binary. Loaded
launchers move into a private retirement directory until they close; a later
activation, prune or uninstall removes their verified files. Retirement is capped
at eight directories and 256 MiB. Uninstall
refuses while a service references the installation, even if stopped: run
`daemon uninstall` for that service first. Use `desktop --prefix <directory>` for
an isolated installation. Start menu shortcuts and PATH changes are not installed.
Windows pruning keeps the exact versions referenced by recognized services,
including their working directories and explicitly selected files. An unknown
wrapper, program, option or ambiguous path keeps all builds. Preview shows which
versions remain protected; a stopped registration still prevents uninstall.

You do not start the daemon. The first client that needs it starts it, on
`~/.agentdocker/agentd.sock`. To have it survive a reboot:

```sh
agentdocker daemon install    # launchd, systemd user unit, or Windows login task
agentdocker daemon status     # what is running, and where
```

Linux service definitions preserve spaces, dollar signs and percent characters in selected paths.

On macOS, installation registers the launchd definition and explicitly requests
an immediate start, just as `daemon start` does. A GUI domain in on-demand-only
mode can leave automatic starts or crash recovery pending; explicit startup
does not establish recovery after a crash, login or reboot in that mode.

On macOS, daemon and connector service operations wait up to five seconds for
launchd to confirm removal before replacing a job or removing its definition.
A timeout or an unrecognized manager error stops the operation.

On Linux, `daemon uninstall` stops and disables the user service before
removing its definition, then reloads systemd. If stopping or disabling an
installed service fails, its definition remains available for diagnosis and
retry. If the definition was already missing, those failures are tolerated.
The browser connector follows the same order with `connector uninstall`.

On Windows, installation creates a Task Scheduler task for the current user
and starts it immediately. It runs with limited privileges at login without
storing a password; an interactive sign-in is required. `daemon start`, `stop`,
`restart` and `uninstall` operate on that task. A supervisor retries a failed
daemon up to three times with two seconds between attempts; a clean shutdown
stays stopped. Ten minutes of continuous operation resets that retry budget.
Installed builds register stable CLI and daemon launchers, so activation and rollback
preserve service paths. Portable builds register their extracted paths; after moving
that folder, run `daemon install` from the new build and keep the old folder until
that succeeds. An ownership record in the daemon
home prevents replacing or removing a task whose action or user was changed
outside AgentDocker. `daemon install --dry-run` previews the task without
registering it.
For an on-demand or portable home without an ownership record, `daemon status`
reports no owned service and checks the daemon directly. It does not start
PowerShell to enumerate unrelated scheduled tasks.
Task Scheduler operations allow up to 60 seconds per PowerShell invocation,
including interpreter and module startup. A timeout stops that invocation and
reports failure; it does not automatically retry a possibly completed change.
Daemon readiness and stop deadlines remain separate.

Then bring in the agents already on the machine:

```sh
agentdocker runtimes        # what is installed, and whether we are wired into it
                            # (HOOKS `outdated (CwdChanged)` names the events setup would add)
agentdocker setup           # register the MCP server and install hooks
agentdocker discover        # agent processes nobody registered
agentdocker adopt --all     # register all of them
agentdocker me              # register yourself, so agents can ask you things
agentdocker ui              # open the app
```

`setup` is the step that matters most, and the one that is easy to skip.
An agent that has not been wired up is still *seen* — it appears in `discover`
and in the app — but it cannot tell the daemon what it is holding or
reading, so it reports nothing it is doing. See
[Why an agent reads "idle"](#why-an-agent-reads-idle).

An agent that works **inside the browser** — Claude's or ChatGPT's extension
in its side panel, whichever vendor's — is a different case. `runtimes` finds
the extension in each Chrome, Brave, Edge, Arc, Chromium or Vivaldi profile
(`Claude in Chrome <version>`) and says under the table that its sessions run in
the browser and on the vendor's side: nothing on this machine speaks for them,
so AgentDocker cannot list, message or lease for them, and no setup changes
that. What *is* on this machine is at most a bridge the browser launches for a
command-line tool (`claude --chrome-native-host`, for a terminal Claude Code
that drives the browser). That bridge is the tool's helper, not a session:
`discover` never lists it and `adopt <pid>` refuses it by name, because
registered it would sit in a project called `chrome` looking like your browser
agent, connected — and it is neither.

When the browser agent has something a terminal agent should know, the way in
is the one the vendors give hosted agents: a remote MCP connector.
`agentdocker connector serve --tunnel tailscale` serves one on loopback — one
per machine, for every project on it — exposes it through Tailscale Funnel on
this machine's own stable `*.ts.net` name (`--tunnel cloudflared` for a quick
tunnel instead), prints the URL to add as a custom connector in Claude or
ChatGPT and a pairing code for the consent page, and each consent becomes an
agent in the project chosen on that page (any folder on this machine) with the
messaging tools and nothing that touches a checkout. The connector belongs to
your account, so any client of it — the side panel, the web or desktop app, a
terminal reaching your account's connectors — comes in as that agent, and
nothing tells them apart: it is named `claude-connector-xxxx` or
`chatgpt-connector-xxxx`, never called a browser, and `connector grants` shows
what each client said it was. A call from a Codex session is refused with the
way to AgentDocker's own MCP server, which Codex uses as itself. It stays
registered until revoked but counts as connected only while it is heard from:
an hour after its last tool call, `ps` and the desktop show it `not connected`
until it calls one again. `connector
install` runs the same as a login service; `connector status` and the
desktop's Tools screen show its address and pairing code, which works for one
consent and is then replaced. On macOS and Linux,
open a browser tool's **Details** and choose **Enable with Tailscale** or
**Enable with Cloudflare** to install and start the connection at login. The
first needs Funnel enabled; the second gives a new address after each restart.
Setup preserves different existing service settings. Each browser account must
still consent to its project connection. Desktop setup admits the vendors' own
addresses through `--allow-from anthropic` and the automatically refreshed
`--allow-from openai` feed. [The remote connector](REMOTE-CONNECTOR.md) has the whole contract.

Everything respects `AGENTDOCKER_HOME`, so a throwaway daemon for
experiments costs nothing:

```sh
AGENTDOCKER_HOME=/tmp/ad-scratch agentdocker ps
```

---

## The desktop app

`agentdocker ui` opens the native Iced window. Its four destinations are
**Projects**, **Inbox**, **Tools**, and **Settings**. The app uses the local
daemon directly. See [the desktop guide](DESKTOP-UX.md) for every interaction.

### Projects and sessions

Choose a project in the sidebar or use **Add project…** to pin an existing folder.
The app remembers the selected project and keeps quiet projects available.

- **Current** shows live sessions. **History** keeps completed runs, including
  older runs with the same name. No records are deleted by these filters.
- **Needs input** shows unanswered questions from this project. Select a session
  and use **Answer** to open its question.
- Select a session for **Open terminal**, **Stop session…**, or **Details**.
  Stopping requires **Confirm stop** within five seconds. External agents stay
  in the terminal or application where they started.
- **Launch agent…** starts an installed CLI in the selected project. **Connect**
  adopts a process under **Running here, not connected** for coordination; it does not
  install provider integrations.
- **Activity** shows the recent project journal, newest first. **More** also
  holds **Files in use**, **Command line**, and project pin/forget actions.
  Channels and `#all` are not in the app; `agentdocker channels` and
  `agentdocker history <conversation>` read them.
- **Pause…** on the project header asks for a reason and tells every agent in
  the project to hold; the daemon refuses their new leases until **Resume**.
  What an agent already holds, it keeps; only you can pause or resume.
- **Board** is the project's cards, lane by lane: type a title at the foot of
  a lane to file one there, drag a card to move it, click it to open it in
  place (its fields, comments and history). Agents pull the next ready card;
  a lane says its count and turns red past its limit. The toolbar searches,
  shows only high-priority cards, those waiting on something, those waiting
  for you, those nobody holds, those an agent holds, yours and your agents'
  (cards you hold and those held by agents you launched from the app, your
  own terminal or a card, or by Claude Code and Codex sessions you started
  yourself in a terminal app or IDE) or those with some tags,
  switches between lanes and a table of every card (`v`), between roomy and
  compact cards (`d`), switches boards, and edits a board's lanes, tags and
  rules. `n` adds a card to the first lane, `/` searches, Esc closes what is
  open. An open card keeps its plan, files and links (open, copy, add,
  remove) and starts an agent on itself.
- **To-do**, in the sidebar, is every project's open cards that are yours,
  your agents' or nobody's that you filed; the circle ticks one done, the
  row opens it on its board.
- **More → Plans** is the project's plans (specs in Markdown that cards
  carry out) and its files: write and save a plan, set where it stands, open
  the cards that carry it out; add a file, open one with its app.

On narrow windows the selected session replaces the list, with **Back to sessions**
to return. Wide windows show actions beside the list. The human `user` identity
remains available for coordination and inbox delivery but is omitted from the
session list.

### Why an agent reads "idle"

Without fresh provider observations or recent coordination, activity is unknown.
Recent coordination can establish working; an explicit provider stop produces
provisional idle that expires or is superseded by newer activity. An agent
with no hooks of its own in a herdr pane takes herdr's word instead, for as
long as herdr keeps saying it; a Codex whose conversation runs in Codex's
shared app-server takes that server's word the same way, ahead of herdr's.
Process presence, provider configuration and
observed activity are separate facts.
See [messaging](ARCHITECTURE.md#messaging).

A Claude Code session you start in a terminal sees messages only at its next
prompt, unless it was started with the channel flag
(`--dangerously-load-development-channels server:agentdocker`); during the
channels research preview no setting replaces the flag. `agentdocker setup
--shell` adds a `claude` function to your shell's startup file (zsh, bash or
fish) that passes the flag on every launch — planned, previewed and undoable
like every other setup change — and the Claude Code card in Tools offers it as
**Wake terminal sessions**. `runtimes` says when it is missing. The app's own
launches already carry the flag.

### Inbox and tools

**Inbox** holds questions and direct messages. Answer drafts survive navigation
and failed sends. Successful delivery does not prove the agent consumed an answer.

**Tools** starts with installed tools. **Details** reveals executable paths,
versions and MCP/hooks configuration. **Other supported tools** expands the rest
of the inventory. **Review setup**, **Apply reviewed changes**, and **Undo this
setup** use saved plans. Ordinary `agentdocker setup <runtime>` also saves an
undo receipt and prints its ID; `setup --show ID` reviews it and `setup --undo ID`
reverses only unchanged configuration. `setup --remove [runtime…]` takes
AgentDocker's wiring out of every tool (or the ones named) without a receipt:
MCP entries and hooks that run AgentDocker, its skills and OpenCode plugin while
still as installed, and the shell block; everything else in those files stays,
and it saves a receipt `setup --undo` puts back. `--remove --preview` saves the
plan to review and `--apply` first. **Check connections** provides bounded diagnostics;
actual provider delivery requires a real round trip.

### Usage

**Projects → Usage** and `agentdocker usage` report the tokens providers reported.
Choose the last day, week or month and group reported tokens by agent, model,
provider, project or hour. `~` marks a partial count; `—` means the source did
not report that counter. These are token totals, not a bill. The report shows
the available time range, gaps and whether collection has caught up. The CLI
names each agent or project row (`codex-<pid>`, `<project>`) while AgentDocker
still knows it, and shows its ID once it is gone; `(unattributed)` is usage no
session could be matched to.
Local-output instrumentation records successful hook, stdio MCP and
managed/native Codex input writes and authenticated browser MCP response bodies
when collection is enabled. The separate
overhead line counts UTF-8 protocol bytes, including forwarded content and
wrappers. It is always marked partial: missing reports and older writers are not
measured, and a completed write does not prove a model consumed it. Browser
counts exclude HTTP headers, OAuth/token responses and consent pages. Counts are neither provider tokens nor a bill, and
are never added to the provider totals. No matching records still means
*not measured*, rather than zero; output text is not saved in accounting tables.
Shared native Codex MCP tool replies are assigned only to the conversation
verified for each call. Unverified replies stay unmeasured. If a batch mixes
conversations or unverified replies, only each verified reply's JSON bytes are
counted; shared array framing is excluded. Native hook and queue writes are
counted separately.

The MCP `usage` tool reads the same stored report and advertises that it is
read-only; querying usage does not enable collection or change its settings.

Collection is off by default. To enable it, add this section to your existing
`~/.agentdocker/agentd.toml` (or the file under `AGENTDOCKER_HOME`), preserving its
other settings:

```toml
[usage]
enabled = true
retention_days = 30
```

The running daemon picks it up on its next collection cycle. It reads supported
local Codex and Claude Code logs; it retains accounting metadata, not message
text. Optional `codex_roots` and `claude_roots` are arrays of absolute directories
without parent (`..`) path components; use a direct path rather than one that
walks up to a parent. Invalid roots produce an explanatory query error. Empty
arrays use the provider defaults. Turning collection off retains available
totals. Increasing retention does not restore previously discarded history.
Tracking metadata has a 256 MiB budget (database indexes and other state take
additional space). When a new record cannot fit, the report says that the storage
limit was reached and keeps totals partial. Existing dedupe records are preserved
so copied or replayed logs cannot count twice. Retention may free space, but the
fixed tracking budget cannot be increased through configuration. Do not
delete accounting tables to make space: doing so can invalidate deduplication.
AgentDocker's emitted-byte coverage is partial when collection is enabled;
without matching output records, overhead is **not measured**.

### Accounting storage

Run `agentdocker usage-storage --home /path/to/agentdocker-state` to inspect an
existing private state directory. The command prints JSON and starts no daemon.
It opens a separate read-only connection and does not initialize, migrate,
checkpoint or vacuum the database. SQLite may maintain shared-memory read locks
in its WAL sidecar. It does not change accounting retention or
recover records omitted at capacity.

The logical tracking budget is separate from allocated SQLite table/index pages.
The report measures all `usage_` tables and their indexes in one read snapshot,
including discovery and tracking metadata. Payload and unused space are included
in those pages; do not add them again. Whole-database page/free-page totals also
cover unrelated coordination state. Main-file, WAL and shared-memory lengths are
separate before/after observations; a changing WAL cannot be attributed entirely
to accounting. These measurements do not estimate injected tokens or billing.

Inspection refuses unsupported state and stops without partial totals if it
exceeds 128 accounting btrees, 131,072 pages or ten seconds between page steps.
Operating-system I/O itself has no hard timing guarantee. Use this on demand;
the normal Usage screen and report do not scan database pages automatically.
Missing state and refused scans produce an error with no partial JSON report.

### Standalone transcript scans

`agentdocker usage-scan --codex /path/to/rollouts --claude /path/to/transcript.jsonl`
prints token accounting for explicitly selected local files or directories.
Repeat either provider flag for up to sixteen inputs. Directories discover JSONL
files using the same bounded walker as collection; missing/unreadable inputs or
incomplete enumeration refuse the report. No provider, authentication or daemon
connection is used. Daemon configuration, accepted records and cursors are unchanged.

Records from the selected snapshots are ordered globally before the shared
accounting logic deduplicates copied logs, handles cumulative baselines and marks
conflicting records. Unknown counters stay unknown. `--by model|provider|hour`
selects grouping; `--since` accepts a duration or RFC3339 timestamp and `--until`
accepts RFC3339. Without a lower bound, the scan includes the earliest selected
sample. The upper bound defaults to the captured scan start. Ranges use UTC-hour
rounding, shown explicitly in the JSON report.

`scan_complete` describes the selected file snapshots. Unsupported records,
conflicts and partial tails leave visible gaps/partial coverage. Nonempty inputs
without recognized accounting records add a gap; an empty or entirely unrecognized
selection reports unknown counts. Supported format families are listed. Complete coverage
applies only to those snapshots, not all account activity or the remaining current
hour. The output contains paths and accounting metadata, never prompt/response
text. Injected-byte overhead is still unknown.

Each parser pass reads at most 4 MiB/4,096 records; prefix validation is separate.
Records above the 1 MiB parser limit produce gaps. The whole operation limits captured input to 8 GiB,
100,000 parsed samples, a 64 MiB metadata allowance, 1,024 reader gaps and five
minutes between operations; discovery retains its 10,000-file limit. These are
operation limits, not a hard RSS or OS-I/O deadline. A file that grows during the
scan, such as a session still being written, is read to the length captured when
the scan started. An input replaced by another file, shortened, or changed at the
same length with new change metadata, or an exhausted limit, refuses without a
partial JSON report. A scan cannot tell that a file which also grew had bytes it
already read rewritten in place: the daemon's collector catches that on its next
pass, and a scan keeps no state to check it. Temporary private accounting state
is removed before success; cleanup failure is an error. This command has no saved
progress and does not reconcile gaps in the daemon's retained history.

### Terminal and settings

**Project terminal** in the project header opens a shell in that project’s folder.
Each current agent also has a **Terminal** action: a managed agent opens its live
PTY inside AgentDocker; an external macOS Terminal session brings its existing
tab forward after checking the process identity. Other terminal hosts report
when focusing is unavailable.

**Open terminal** attaches to a managed live PTY. **Detach** leaves the agent
running. Copy uses the selected terminal range, or the visible screen when nothing is selected; paste respects bracketed-paste mode,
F6 leaves terminal focus, and Control+] detaches. Rejected input is reported;
already sent input is never automatically replayed.

**Settings** controls light/dark appearance, text sizes, terminal palette and row
spacing. Project selection and appearance are saved privately in `workspace.json`;
`ui.json` remains the settings compatibility file. Settings also opens installation,
rollback, launcher removal and retained-version cleanup. These operations preview
exact changes and preserve running releases; see [DISTRIBUTION-SETUP.md](DISTRIBUTION-SETUP.md).

---

## The console

Open **Projects → More → AgentDocker commands** to run a bundled `agentdocker` subcommand
in the selected project. Output stays in the window; **Previous** and **Next**
recall commands. This field runs CLI arguments without a shell.

Type the command without the leading `agentdocker` (though typing it
anyway is forgiven):

```
ps --all
journal --new
runtimes
leases
activity
channels
inspect <agent>
```

Streaming commands — `watch`, `events`, `logs -f`, `top` — never finish on
their own, so the console stops them after twenty seconds and shows what
they said. Run those in a real terminal.

---

## Command reference

Every command, by what you are trying to do. `--help` on any of them for
the flags.

### Exit status

A command ends with a status that says what class of thing went wrong, so
a script or an agent driving the command line can branch without parsing
text; the words and any details still go to stderr as `Error: … (Code)`.

| Status | Meaning | Daemon answers |
| --- | --- | --- |
| 0 | done | — |
| 1 | something unexpected: an internal error, or a failure that is not the daemon's answer (no daemon, a broken connection) | `internal` |
| 2 | a usage error, the argument parser's own; also an invalid request | `invalid` |
| 3 | nothing by that name, or too many | `not_found`, `ambiguous` |
| 4 | held or taken by somebody else | `conflict`, `name_taken`, `deadlock` |
| 5 | refused: not the caller's to do, the project is paused, or it acts as an agent, or registers a process, from inside another agent's session or from a sender that cannot be traced | `forbidden`, `paused`, `identity_mismatch` |
| 6 | not now: the daemon, its storage, an engine or a build is unavailable, busy, handing over, timed out or cancelled | `storage_unavailable`, `unavailable`, `engine_unavailable`, `build_failed`, `backpressure`, `timeout`, `cancelled`, `transferring`, `event_history_lost` |

The class holds wherever the answer was read — a `daemon reload` refusal, an
`attach` the daemon refuses or ends with an error — and `adopt --all`, which
tries every process, ends with the class of the first refusal after naming
each one by pid.

### Look at the fleet

| Command | What it does |
|---|---|
| `ps` | Agents grouped by project, with INPUT readiness and, when usage collection is on, each agent's TOKENS over the last 24 hours (input and output, in three figures); `--input-details` adds the one fix for each session that cannot take messages now |
| `top` | The fleet live, redrawing as the daemon reports changes |
| `activity` | What each agent is doing: working, idle, or blocked on a named resource |
| `usage` | Tokens the providers reported, filtered explicitly with `--agent <id>` (`--as` alias) or `--project <id\|path>`, one row per agent (`--by model\|provider\|project\|hour`), each count with its coverage (`~` where some samples did not say, `—` where none did), and under the table what the totals cover: the range answered, retention, gaps, whether collection is on, and AgentDocker's emitted-byte overhead with partial coverage, or *not measured* without matching records; `--since 24h`, `--json`. `AGENTDOCKER_AGENT_ID` does not narrow this query. |
| `inspect <agent>` | Everything known about one agent, as JSON |
| `logs <agent>` | An agent's captured output, at most 16 MiB kept per agent (the newest 8 MiB and the 8 MiB before them); `-f` to follow, `--compress` for an rtk view |
| `validation <id>` | The retained log of one validation; `--compress` for an rtk view |
| `events` | The daemon's event stream; `[[webhooks]]` in `agentd.toml` posts a signed copy of chosen kinds to an address you name (see [webhooks](ARCHITECTURE.md#events)) |
| `webhook deliveries <name>` | A webhook sink's latest attempts to post, oldest first: the event, which attempt, whether it was delivered, will be retried (and when) or was given up, the receiver's status code or a word for why there was none, and how long it took; `--limit` (up to 200), `--json` |
| `ping` | Check the daemon is reachable |

### Start, adopt and stop

| Command | What it does |
|---|---|
| `run <command>` | Launch a command as a supervised agent |
| `codex [--name <name>] [-- <codex options>]` | Start Codex in this terminal so AgentDocker can reach it: the same Codex and conversation, run through AgentDocker's launcher, so a message wakes it even while it is idle, and it can hold a role (`role --as <name> reviewer`, or the app's Roles menu) like an agent the app launched. The binary behind the `codex` on `PATH` is used; experimental |
| `register` | Announce an already-running process as an agent. `--pid` names its process: this command's own shell (`--pid $$`) or another of its parents. From inside an agent's session any other is refused `identity_mismatch` (exit 5), and a process an agent already holds is registered only from inside that agent's own session; from your own terminal, any |
| `discover` | Agent processes nobody registered |
| `adopt <pid>` | Register one of them; `--all` for all |
| `stop <agent>` | Signal an agent to stop. Its record says so from then on (`"stop_requested": true` in `inspect`), even if it exits 0, until it next starts under the same id |
| `restart <agent>` | Replace a managed container after confirming it exited |
| `deregister` / `rm` | Mark an external agent finished / forget a finished one |
| `role <name>…` | Give an agent (`--as`) roles from its project's palette (`roles`) — `role --as codex-1 reviewer tester` — in place of what it held; `--add reviewer` and `--remove tester` change one at a time, `--clear` takes them all away. Roles are yours to give: run from an agent's own shell it is refused. A command that names no sender speaks as you only from a terminal outside every agent: where a provider's own variables are set (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`) it asks for `--as` instead, even when the provider's process cannot be found. An agent's `send --to role:reviewer` reaches every reviewer in the project, in the role's room (one thread, where the sender hears the answers); `handoff role:reviewer` needs the role's one holder |
| `roles` | A project's roles, each with its colour: the five every project offers (lead, implementer, reviewer, explorer, tester) and your own. `roles add "Security reviewer" --color red --description "…"` makes one, `roles set reviewer --name "Code review" --color pink` changes one (its address stays), `roles rm tester` takes one out. `roles set reviewer --tools review_verdict,review_gate,get_task` offers its holders only those MCP tools and the few every agent keeps (`--tools` works on `roles add` too), `roles set reviewer --all-tools` offers every tool again; the list shows a limited role's tools. Only you change them, and nothing holds a role until you give it; run from an agent's shell, including a Codex conversation's shared server, they are refused. `--project` names another project |
| `rename <agent> <name>` | Give a live agent a name of your choosing (up to 64 characters, unique among live agents); its id and everything addressed by id are unchanged |
| `deregister --as <agent>` / `rm <agent>` | Mark an external agent finished, without signalling its process / forget a finished one. `rm` on a live agent says which of the two applies: `stop` for one AgentDocker started, `deregister` for one it did not. A finished record that holds nothing is forgotten by itself thirty days on, or after `[agents] retention` in `~/.agentdocker/agentd.toml` (`"off"` keeps every record) |
| `up` / `down` | Start or stop the agents in an `Agentfile.toml`. An entry with `after = "build exits 0"` starts once `build` has exited 0; `up` stays while it waits (`waiting: build exits 0`), and if `build` exits otherwise, is stopped or fails to start, it and everything waiting on it are not started (`not started: build exited 1`) and `up` exits unsuccessfully. `up test` alone takes an earlier exit 0 of `build`, from another `up`, as success when it was `build`'s own: its record says no stop was asked of that run, and it is still the run of the entry as this file gives it now (the same command, runtime, working directory, environment and the rest of how it runs; not renamed). Edit `build` in any of those ways, or give it `isolate = true`, and bring it up again before `test`. An earlier `build` that was stopped does not count, whatever it exited with (`not started: build was stopped`), and neither does one whose record cannot show this (`not started: build ended before this run; …`); the record is kept, and the answer with it, across a daemon restart and until retention forgets it. For an agent `up` is watching, only an exit 0 whose event it received counts, so an exit 0 seen after `up` has said its event stream ended does not |
| `heartbeat` | Report that an agent is alive |

### Talk

| Command | What it does |
|---|---|
| `send` | Message an agent (or `role:<name>`, the one agent with that role in your project), the project, a topic, or everyone. An agent reaches only its own project and you; its `all` means its project, and you can still send anywhere. A `--link kind:target` (repeatable) travels beside the text: a path, a commit, a pr, a url, a task, a message or a memory for the reader. |
| `delivery <message>` | Where one of your messages stands with each agent it was queued for: answered, delivered, received by the model, shown to its session, or why it is still waiting (next prompt, a browser agent's check-in, paused, blocked, ended). Never "read": nothing says a model read it. `send` prints the id; `--json` |
| `watch` | Stream messages for an agent or matching topics. Topics keep their messages: `watch --as <agent> 'repo/#'` first shows what each matching topic kept since the last topic message that agent acknowledged there (everything kept, the first time), then streams live; `inbox --as <agent> --ack <id>` on a topic message moves that topic on past it. A topic line shows its `seq:`; without an agent, topics are live only unless `--after-seq <n>` replays from that point. One watch replays at most a topic's newest 500, 5,000 in all, and says how many older ones it did not replay |
| `inbox` | Messages queued while an agent was not watching |
| `ask` | Ask an agent — or the human — and wait for the answer |
| `answer` | Answer a question somebody is waiting on |
| `questions` | Questions waiting for an answer |
| `conversations` | What you can read: every conversation with its unread count, newest first |
| `history <conversation>` | What was said in one conversation, oldest first; `--read` marks it read through the last line shown, for you or for `--as <agent>` (the agent's own shell, through `AGENTDOCKER_AGENT_ID`), never the other |
| `thread <message>` | One message and the replies under it |
| `search <query>` | Find archived messages by text (`--project` narrows; `--as <agent>` searches only what that agent could list) |
| `me` | Register yourself as an agent named `user` |

A successful `send` prints the message ID on stdout. Acceptance and recipient
warnings go to stderr: a queued message can still be waiting for another prompt
in a session without an input receiver. Each warning names where the session
stands and the one thing that would get it taking messages — the same fix
`agentdocker ps --input-details`, the INPUT column's words and the desktop
give: a Claude Code session's resume command (`agentdocker reconnect <id>` for
one AgentDocker started, which a plain resume could not bring back under its
record), `agentdocker setup <runtime>`
first for a session nothing of which ever reached AgentDocker, `codex resume`
with its conversation (only for a Codex started in a terminal), a live Codex from Launch agent, waiting out a provider
limit, reviewing a paused delivery, or the agent's own read of its inbox. `agentdocker reconnect` and
**Reconnect here** start an ended session again; no fix resubmits the
session's initial prompt, and once its receiver is back, queued delivery offers
the same unacknowledged message again. Provider limits remain separate from receiver
health. When the daemon does not report readiness, `send` prints `recipient input readiness unavailable from this daemon` rather than imply that every recipient can wake.

While you type a line in a live Codex's terminal (one started with
`--codex-input` or Launch agent), a message from anyone else waits until you
have stopped for three seconds or pressed Enter, so it never starts a turn that
prints over your line; your own messages never wait. Meanwhile its INPUT reads
**Waiting for the person to stop typing** and `events` shows `input held`.
AgentDocker types no message into any other terminal or multiplexer pane:
other sessions take messages through their provider's own input.

For an existing Claude session with no channel, save the current work, exit that
Claude session, and either press **Reconnect here** on its row or in the app's
session Details,
run `agentdocker reconnect <session>` (`--claude <path>` to choose a different
executable; otherwise the recorded command or `claude` on PATH is used), or use
the session-specific resume command that the session's panel, `send`'s warning
and `ps --input-details` show, from its project folder. The first two bring the
session back under its own record with its conversation and the channel — what
was queued for it stays its own — and refuse with the reason while its process
is still running, in another checkout, or with somebody attached; the app opens
its pane, and `reconnect` prints the same id. Accept Claude's prompt there. Recorded launch options and explicit environment values are preserved, including
permission/tool restrictions and the provider profile. The original prompt is
not submitted again. Unsupported or ambiguous options refuse with guidance to
resume explicitly; options never recorded for an external session cannot be
recovered.
Complete Claude's startup channel consent. The AgentDocker MCP entry must include `--claude-channel`; see
[Claude channel setup](CLAUDE-CHANNEL-INPUT.md). Hooks alone cannot start an idle
turn. A copied instruction is not executed by AgentDocker.

### Share a resource

| Command | What it does |
|---|---|
| `claim` | Claim a lease on `path:`, `branch:`, `task:`, or anything |
| `renew` / `release` | Extend or give up a lease you hold |
| `leases` | Every lease held right now |
| `waiting` | Claims waiting for a resource, in the order they will be served: higher priority first, then oldest |
| `task` | The board of work. `task create "Fix login" --acceptance "SSO works" --lane ready --priority high --size m --due YYYY-MM-DD --tag auth --blocked-by AD-7 --link pr:#<n>` files a card and prints its key (`AD-12`); `--parent`, `--description` and `--blocker "waiting on legal"` say more, and `--board` files it on another board. `task create-many cards.json` files up to 50 at once, all or none, each able to name another of the batch by its `ref` as its parent or in `--blocked-by`. `task pull --as <agent>` takes the next ready card (the first in the board's pull source, free and waiting on nothing; it says how many are held or waiting when there is none), `task pull AD-12 --as <agent>` that card; either holds it as a `task:<id>` lease (four hours; `renew` extends it, an exit releases it; a card whose hold lapsed is taken over only with `--take-over-from <holder>`). `task move AD-12 review --comment "ready for eyes"` moves it to a lane by slug, name or category (`--after`/`--before` another card places it; an agent moves only a card it holds; back to Ready or to Done ends the hold). `task show AD-12` is the card in full: fields, comments, history, what it waits on. `task update AD-12 --assignee bob --priority low --tag ui` edits it (`--size ""`, `--due ""`, `--parent ""`, `--no-tags`, `--no-blocked-by`, `--no-links` clear). `task comment AD-12 "…" [--reply-to <comment>]`, `task comment-edit <comment> "…"`, `task comment-delete <comment>`. `task archive AD-12`, `task unarchive AD-12`, `task archive-lane done`. `task boards`, `task board-create Sprint --template agents` (classic, agents or simple), `task board-update sprint --lanes lanes.json --pull-source approved`. `task search "logn page"` finds cards by words, slips forgiven, a key exactly. `task start AD-12 --runtime codex` starts an agent on a card nobody holds that waits on nothing, and hands it the card. `task rules`, `task rule-add "Stale review" --idle review --days 14 --comment "Still waiting?"` (or `--enter LANE`; `--move LANE`, `--archive`, `--start RUNTIME [--max N]`), `task rule-off\|rule-on\|rule-rm <id>`. `task tags`, `task tag ui --color teal [--rename frontend]`, `task tag-rm ui`; `--plan` on `create` and `update` names the plan a card carries out. `task list [--board B] [--lane L] [--assignee me --as <agent>\|mine\|none] [--open] [--query words] [--tag T] [--archived] [--limit 100] [--offset N]` (a page; it says when the board goes on and where the next page starts); `--assignee mine` is your cards together with those held by your agents — launched with `run` or `up` from your own terminal, the app's launch, or `task start` and start rules, or a Claude Code or Codex session you started yourself in a terminal app or IDE (root's install, or one whose signature verifies), or over SSH, with no agent's session anywhere above it (their records carry `launched_by=user`, so `ps --label launched_by=user` lists them; a session inside a multiplexer server such as tmux cannot be told apart from an agent's and is not counted) — and, with `--as <agent>`, that agent's own |
| `plan` | A project's plans, specs in Markdown that cards carry out: `plan list [--all]`, `plan show <id>`, `plan new "Auth rewrite" --body spec.md` (or `-` for stdin), `plan edit <id> [--title …] [--body FILE] [--status draft\|active\|done\|archived]` |
| `file` | A project's files, kept beside the work: `file list [--task AD-12]`, `file put notes.md [--task AD-12] [--name …]` (at most 4 MiB; the same name again is a new version), `file get <id> [--out PATH]`, `file link <id> AD-12 [--unlink]`, `file rm <id>` |
| `pause` | `agentdocker pause "sleeping the laptop"` tells every agent in this directory's project to hold: they get the reason as a `pause` message and their new leases are refused until `agentdocker pause --lift`; `--project` names another project; `pause --list` lists what is paused and why |

### Know what changed

| Command | What it does |
|---|---|
| `journal` | What changed and why, one line per entry; `add` appends a note; `prune --before <seq\|duration>` trims (or set `[journal] retention` in `~/.agentdocker/agentd.toml`) |
| `changes` | The ledger: file changes seen in a project, with who held each file |
| `blame <path>` | Who changed a file, oldest first |
| `overlap` | Paths changed in more than one checkout: what will collide |
| `observe` / `reads` / `stale` | Record what was read, and check it is still true |
| `memory add <text>` / `search <words>` / `profile` / `forget <id>` | The project's memory: facts its agents and you keep for each other, told to every new session; `add --supersedes <id>` replaces one, `add --standing` keeps a convention said first, `add --forget-after <when>` (an RFC 3339 time, or `90m`, `12h`, `7d`, `2w` from now) stops saying it after then |

### Work in isolation, then merge

| Command | What it does |
|---|---|
| `worktree-create --branch <name> [--from <ref>] [--seed-build <dir>]…` | A new linked checkout and branch, at your HEAD or at `--from`, without touching existing files; commits you make there with git are journaled as yours, and a post-commit hook (put in front of any the repository has, which still runs) reports each one so your verdict on that branch is refused later. When it cannot (the repository sets `core.hooksPath`), it says why on stderr. `--seed-build target` (repeatable: `node_modules`, `.venv`) starts the new checkout with your build output so its first build is warm: cloned on APFS (near-instant, no extra space until a file changes), reflinked where Linux's filesystem can, copied otherwise; not on Windows yet. No symlink on the way to it is followed, and nothing already in the new checkout is replaced. It prints the new path, then one line per directory on stderr (`target: cloned`, `node_modules: skipped (…)`); a directory that cannot be seeded never stops the worktree |
| `worktree-diff` | Tracked changes in an agent's checkout |
| `worktree-remove <agent>` | Remove an ended session's own worktree once its branch is merged or no longer wanted; the worktree must be clean, and the branch is deleted only when git considers it merged |
| `review-branch <agent>` | What an agent's branch would bring to the branch in the project's main checkout: commits, files and lines, and what stops a merge now (uncommitted work, uncommitted changes in the main checkout, conflicts, held files); `--patch` prints the diff, `--json` the whole review |
| `merge-branch <agent> --head <sha>` | Merge that branch into the main checkout's branch with a merge commit, if its head is still the one `review-branch` printed and nothing stops it; refused with nothing changed otherwise. It does not push |
| `commit` | Commit the agent's checkout, journaled and attributed to it |
| `validate` | Run a check and retain its command, log and content fingerprints |
| `validations` | Retained validation evidence |
| `integrate` | Preview or prepare an uncommitted merge of validated source |

### Hand work over

| Command | What it does |
|---|---|
| `checkpoint` / `checkpoints` | Persist task context and content identity; `checkpoints prune --older-than <duration>` forgets finished sessions' old ones |
| `handoff` / `handoffs` | Hand an agent's work to another, with everything around it `--link kind:target` names what the recipient should open first; `checkpoint --link` does the same for a replacement session. |
| `resume` | Inspect or accept a verified handoff |
| `export` / `import` | Carry a bundle to another host |

### Agree with each other

| Command | What it does |
|---|---|
| `channels` / `channel` | The rooms agents share when they are on the same work; `channel open --name planning` gives one a `#name` (made from the task otherwise); `--project <id or path>` opens it in a project the opener is not in, which is how a person opens one |
| `review-request` / `review` | Ask for and give verdicts; requested changes block |
| `verdict <branch> --head <commit> --approve\|--changes\|--comment [--from <commit>] [--findings P1,P2,P3] [note]` | Give your verdict on a branch at the commit you read; it counts for the roles you hold, and only for that commit |
| `gate <PR\|branch> [--head <commit>] [--role <role>] [--publish] [--json]` | Whether it may land: approved at its head as it is now (exit 0), else changes asked, stale (commits since the approval) or unreviewed (exit 1). `--publish` sets the `agentdocker/review` commit status on GitHub |
| `contest` / `contests` | Several agents attempt one task, ranked by a measure declared first |

### Containers

| Command | What it does |
|---|---|
| `image-build` / `images` | Build with an explicit engine, retaining input provenance |
| `grant-access` / `revoke-access` | Issue and revoke scoped container credentials |

### The machine

| Command | What it does |
|---|---|
| `runtimes` | Agent tools installed here, and whether we are wired in; UNREGISTERED counts that tool's processes nobody registered (what `discover` lists), not its sessions — `ps` shows those; browser extensions per profile, with the note that their sessions never appear; anything the inventory could not read within its bounds is listed as `inventory incomplete` rather than passed off as absent |
| `setup` | Wire us in: MCP registration, and hooks for Claude Code |
| `ui` | Open the desktop app |
| `attach <agent>` | Connect this terminal to an agent's; Ctrl-] detaches. Only the agent's own output is shown while attached; dropped output or a replaced daemon is said once you detach or it ends |
| `daemon` | Install, start, stop, reload, inspect or `vacuum` `agentd` |
| `desktop` | Install, inspect or roll back the native desktop for this user |
| `identity-repair` | Preview a legacy identity repair; apply it only with daemon and sessions stopped |
| `report-activity` | Report an observed provider turn state (expires after five minutes) |
| `cancel-question` | Close a question you asked; messages and answers are retained |
| `hook` | Handle a hook event, or install the hook configuration |
| `mcp` | Serve our tools to an MCP host over stdio |
| `connector serve` / `status` / `install` / `enable` / `uninstall` / `grants` / `revoke` | Let the clients of your Claude or ChatGPT account join the messaging of any project on this machine (chosen at consent): served on loopback behind a tunnel you run or one it starts (`--tunnel tailscale` for a stable name, `--tunnel cloudflared`), as a login service with `install`, admitting only the vendors' addresses with `--allow-from`; see [the remote connector](REMOTE-CONNECTOR.md) |

On Windows, Ctrl-] also works when the nested console uses Win32 keyboard events; other keys keep their normal meaning.

Connector service installation saves absolute project, tunnel-program and local `@file`
allowlist paths, so login does not depend on the installing shell’s working directory.
Those configured paths must exist when installing; a missing feed or program is
reported before the service definition is written.

---

### Environment

Everything the commands, the daemon and the app read from the environment, in
one place. The first table is what a person might set; the second is set for
you by AgentDocker itself and is listed so it can be recognised, not so it can
be set.

| Variable | Read by | Meaning |
|---|---|---|
| `AGENTDOCKER_HOME` | daemon, commands, app | The state directory (`~/.agentdocker`): socket, logs, `state.db`, `agentd.toml`, `policy.toml`. A different value is a different daemon. |
| `AGENTDOCKER_SOCKET` | daemon, commands, app | The socket to serve or connect to (`<home>/agentd.sock`). |
| `AGENTDOCKER_AGENT_ID`, `AGENTDOCKER_AGENT_NAME` | commands, hooks, MCP | Who is speaking: the default for `--as` and `--from`. `run` sets both in a managed agent's environment. An agent speaks only as itself: a command acting as an agent is answered only from inside that agent's own session (its shell, hooks, MCP server, and what they start) or from yours (the app, your own terminal, outside every agent's session); from inside another agent's session it is refused `identity_mismatch` (exit 5), whatever `--as`, `--from` or this variable says. |
| `AGENTDOCKER_NO_AUTOSTART=1` | commands, app | A client that cannot connect does not start a daemon. |
| `AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1` | Claude Code hooks | On a session launched with the channel: hooks leave inbox delivery to the channel and only observe. The parent `claude`'s own flag wins where it can be read, and the hooks deliver again when the channel's queue has paused on an unconfirmed message. `run --claude-channel` sets it. |
| `AGENTDOCKER_CODEX_INPUT=1` | Codex hooks, bridge | On a Codex process AgentDocker launched with input: delivery goes through the bridge. `run --codex-input` and the app's **Idle messages: On** set it. |
| `AGENTDOCKER_NO_NOTIFICATIONS=1` | daemon | Post no desktop notifications. |
| `AGENTDOCKER_UPDATE_FEED` | `desktop update` | A feed other than the published one, as a URL or a file, for an offline trial. |
| `AGENTDOCKER_STARTUP_TRACE=1` | daemon | Print each startup stage and its elapsed time to stderr before the log exists. |
| `AGENTDOCKER_EXPERIMENTAL_RELOAD=1` | daemon | The same as `[experimental] reload = true` in `agentd.toml`, for one daemon process: permit `daemon reload`, the experimental live replacement. |
| `AGENTDOCKER_EXPERIMENTAL_SECRET_INPUT=1` | Codex bridge | The same as `[experimental] secret_input = true`, for one bridge process: enable the temporary secret-input broker. |
| `AGENTDOCKER_INSTALL`, `AGENTDOCKER_INSTALL_DIR`, `AGENTDOCKER_VERSION` | `install.sh` | Which route (`cli` or `desktop`), where, and which release to pin. |
| `AGENTDOCKER_CAMPAIGN_LEASE`, `AGENTDOCKER_CAMPAIGN_WAIT`, `AGENTDOCKER_CAMPAIGN_RENEW_SECS`, `AGENTDOCKER_FUZZ_ROOT` | `scripts/verify.sh` | The build-campaign lease on a shared machine, and where a fuzz campaign keeps its fixtures. |

| Variable | Set by | For |
|---|---|---|
| `AGENTDOCKER_TOKEN_FILE` | `grant-access` | The file holding a contained agent's scoped credential; a client with one talks to the restricted endpoint and never starts a daemon. |
| `AGENTDOCKER_SETUP_RECEIPT` | guided setup | The receipt a provider's MCP entry was written under, so undo can find it. |
| `AGENTDOCKER_RELOAD_CANDIDATE` | `daemon reload` | The successor daemon's handover. |
| `AGENTDOCKER_NATIVE_CAPABILITY`, `AGENTDOCKER_NATIVE_LIFETIME`, `AGENTDOCKER_TRACE_NATIVE_STARTUP` | the experimental native Codex launcher | Its child's credential and lifetime, and a startup trace. |
| `AGENTDOCKER_TEST_*`, `AGENTDOCKER_OWNER_IN_PROCESS`, `AGENTDOCKER_IMAGE_PATH_CHILD`, `AGENTDOCKER_BOOTSTRAP_FIXTURE_PARENT`, `AGENTDOCKER_MULTIPLEXER_TEST_CHILD`, `AGENTDOCKER_BENCH_DIAGNOSTICS` | the test suites and benchmarks | Fixture children and diagnostics; never read in ordinary use. |

## MCP tools

`agentdocker setup` registers `agentdocker mcp` with every runtime that
takes an MCP server. An agent then has these without knowing anything
about us:

`whoami` · `ping` · `list_agents` · `inspect_agent` · `activity` · `usage`

`send_message` · `read_inbox` · `wait_for_messages` · `acknowledge_messages` ·
`ask_human` · `open_questions` · `answer_question` · `report_activity`

`claim` · `renew` · `release` · `list_leases`

`read_journal` · `journal_note` · `observe_paths` · `check_stale` ·
`read_set` · `overlap`

`create_worktree` · `worktree_diff` · `commit` · `integrate_worktree` ·
`validate` · `validation_results`

`save_checkpoint` · `list_checkpoints` · `resume_checkpoint` · `handoff` ·
`list_handoffs`

`open_channel` · `list_channels` · `close_channel` · `request_review` ·
`review` · `review_verdict` · `review_gate`

`contests` · `enter_contest` · `submit_entry`

`list_tasks` · `my_tasks` · `get_task` · `search_tasks` · `create_task` ·
`create_tasks` · `pull_task` · `move_task` · `update_task` · `comment_task` ·
`archive_task` · `list_tags` · `create_tag` · `list_roles`

`list_plans` · `get_plan` · `save_plan` · `list_files` · `get_file` ·
`put_file` · `link_file`

`add_memory` · `search_memory` · `get_profile` · `forget_memory`

A browser agent on the remote connector has the messaging tools and
`list_tasks`, `get_task`, `search_tasks`, `comment_task`, `list_plans` and
`get_plan`.

A role you limit (`roles set <role> --tools …`) narrows this. An agent
holding only limited roles sees the tools those roles name, together, and
`whoami`, `send_message`, `read_inbox`, `wait_for_messages`,
`acknowledge_messages`, `list_agents` and `list_roles`, which every agent
keeps; a call to any other is refused, naming its roles. One role without a
limit, or no role at all, means every tool. A change reaches a running
session's calls within half a minute; the list it shows follows the next
time it asks for its tools, usually its next start. It narrows what the
session is offered, not what its shell can run.

Results are compact rather than pretty-printed, and most answer with a
projection — the fields an agent uses, not every field the daemon keeps.
Pass `verbose: true` for the whole record.

## Hooks

For Claude Code, `agentdocker setup claude-code` installs handlers for
`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`, `StopFailure`,
`SessionEnd`, `Notification`, `PermissionRequest`, `SubagentStart`, `SubagentStop`
and `CwdChanged`. A permission prompt or question dialog marks the session
*waiting* for you. `PermissionRequest` adds what the prompt is for in one line
and never answers it: the dialog stays yours. The line is the tool and: for a
shell command, the program and, for a few tools (`cargo`, `git`, `go`, `npm`,
`pnpm`, `yarn`), one known subcommand, as in *cargo test* or *git push*, never
another argument, since any of them could be a password; for a file, its path;
for a fetch, its address without user, password or query. A search shows its
tool alone, never its query or pattern, and nothing that reads like a secret is
kept. A subagent starting keeps its session *working*, and is never an agent
of its own. `Stop` marks the session idle unless it leaves background work
running (a shell, a subagent, a monitor or a workflow it started), which keeps
it working, as a subagent finishing with background work still running does. When
the session's shell changes directory (`CwdChanged`), its branch and commit are
read again from there. For Codex, setup installs its activity hooks, which
include `SessionEnd` — a Codex session you started plainly in a terminal leaves
when its conversation ends, giving back its leases — and `SubagentStart` and
`SubagentStop`, which keep the conversation working while its subagents run.
Hooks an earlier version installed are *outdated*: `agentdocker runtimes` shows
`outdated (…)` with the events they lack, the Tools card says *Hooks outdated*,
and `agentdocker setup claude-code` (or `codex`) adds them. They are what let the daemon see an agent's session
begin and end, what it is about to edit, what it changed, and what it
should be told before it starts — the journal since it last looked, the
project's memory, and anything it read that has gone stale. Codex's
`SessionStart` hook and the OpenCode plugin tell a new session the
project's memory too. Before an edit, `PreToolUse` refuses
one to a file changed since the session read it. A folder observation
remembers children that were deleted; reread their absence before writing
one back. A target that was and remains absent, or whose content is unchanged
despite sibling edits, stays fresh while the directory baseline is retained.
An own edit clears only when its current identity matches recorded evidence.
Unknown or expired evidence requires a reread; a watcher flush alone cannot
prove that all operating-system events arrived. In a folder removed from the desktop's project list the
hooks do nothing at all until it is listed again: added back, or one of its
sessions opened in the desktop. `PreToolUse` takes a lease on the
file about to be edited and `Stop` gives those edit leases back when the
turn ends; a lease the session claimed itself — a worktree, a branch, the
build campaign — is not touched until the session releases it, its TTL runs
out, or the session ends.

---

## Tutorials

### Two agents, one repository

The case the product exists for.

```sh
cd ~/your-repo
agentdocker setup                  # once per machine
agentdocker me                     # once per machine
agentdocker adopt --all            # bring in the agents you have running
agentdocker ui                     # watch them
```

Now have each agent claim what it is about to touch:

```sh
agentdocker claim path:src/api --as agent-1 --note "rewriting the router"
```

The second agent to want that path is told who holds it and waits, rather
than editing underneath the first. `agentdocker waiting` shows the queue;
`agentdocker leases` shows who holds what. When they turn out to be
changing the same files anyway, a channel opens by itself and both are in
it — `agentdocker channels`.

`--wait 120` waits up to two minutes for a held resource instead of failing
at once, and `--priority N` (from -9 to 9, 0 by default) says where that wait
stands: higher is served first, the oldest first among equals. A high
priority can keep a low one waiting for as long as higher ones keep coming;
every wait still ends by its `--wait` deadline at the latest. A lease that
expires lets the next waiter in line try again, not every waiter at once.
When waits form a ring, each holding what the next wants, the lowest
priority's wait is ended with a deadlock error naming the ring (with equal
priorities, the claim that closed it); give up something you hold and claim
again. The MCP `claim` tool takes the same `wait_secs` and `priority`.

### Let an agent work in its own checkout

```sh
agentdocker run --isolate --as reviewer -- claude
agentdocker worktree-diff --as reviewer
agentdocker validate --as reviewer -- cargo test
agentdocker integrate --as reviewer --source <worktree> --validation <id>
```

`integrate` refuses without a passing validation from that checkout, run
against the content that is actually there. The merge it prepares is left
uncommitted for you to read.

### Hand work from one agent to the next

```sh
agentdocker checkpoint --as agent-1 --task "router rewrite" \
  --next-steps "finish the error mapping"
agentdocker handoff --as agent-1 --to agent-2
agentdocker resume --as agent-2        # inspect, then --accept
```

The receiving agent gets the checkpoint, the leases, what was read, what
changed since, the diff, unread messages and the journal — not just a
sentence about where things stand.

### Commit an agent's work so the journal knows whose it was

```sh
agentdocker commit --as agent-1 --all -m "rewrite the router"
agentdocker journal --kind commit
```

The daemon commits the checkout and writes the journal entry itself, so
the entry names the agent that asked and carries the message it wrote.
Without this the watcher still notices HEAD moved and writes a `commit`
entry — but it has to guess whose it was, from the only agent in the
checkout or whoever holds the `branch:` lease, and it can only summarise
a sha.

Nothing goes into the commit itself. The git author is whoever git is
configured as, and no trailer is added: it is your repository, and which
agent typed it is our record to keep, not a change to your history. That
record outlives the journal: the agent's verdict on the branch is refused
from then on.

A pending merge can be committed even when its file tree is unchanged; its new ancestry is still journaled. An ordinary empty commit remains refused, and untracked files are not staged automatically.

Add `--push` to push afterwards. A push that fails leaves the commit —
losing the work to tidy up a failed push would be the wrong trade.

### Read a long log without paying for all of it

```sh
agentdocker logs agent-1 --compress
agentdocker validation <id> --as agent-1 --compress
```

Where [rtk](https://github.com/rtk-ai/rtk) is installed, this pipes a
copy of the retained log through it and shows what came back. Where it
is not — or where it fails — you get the whole log and a line saying
why.

The log on disk is never rewritten. A validation log is evidence:
`integrate` refuses source that has not passed, and the log is how
somebody checks that claim later. Evidence is kept whole.

### Experimental switches

Experimental features are switched in one place, `[experimental]` in
`~/.agentdocker/agentd.toml`:

```toml
[experimental]
reload = false        # `daemon reload`, the live replacement of the daemon
secret_input = false  # the managed Codex bridge's secret-input broker
native_codex = true   # the receiver for an existing Codex terminal session
```

`daemon status` says how each stands. The first two are off until the file
turns them on; `AGENTDOCKER_EXPERIMENTAL_RELOAD=1` on the daemon and
`AGENTDOCKER_EXPERIMENTAL_SECRET_INPUT=1` on a bridge also turn one on for
that process. The third is on by default: with `native_codex = false`, an
existing Codex session gets its messages at tool boundaries through its hooks
and no receiver is started for it. The file is read by the daemon; a bridge or
a hook asks the daemon how the switches stand.

### Let a model read a quiet screen (opt-in)

An agent can stop for you without saying so: a tool with no hooks shows an
approval dialog, asks a question in plain text, or sits at its prompt over
words you typed and never sent. The screen judge notices: when an agent's
screen has not changed for twenty seconds, it asks TypeSafe's Jev model about
the screen's last 30 lines, and an agent that clearly waits on you goes into
Needs you, marked as judged from its screen with how sure the model was.

It is **off** unless you turn it on, and while it is off no screen is read for
it and nothing is sent anywhere. To turn it on, keep your TypeSafe API key in
a file only you can read, and name it in `~/.agentdocker/agentd.toml`:

```sh
mkdir -p ~/.config/agentdocker
(umask 077; cat > ~/.config/agentdocker/typesafe.key)   # paste the key, then Ctrl-D
chmod 600 ~/.config/agentdocker/typesafe.key   # umask covers only a new file
```

```toml
[judge]
judgments = ["screens"]
key_file = "/absolute/path/to/typesafe.key"
```

`key_file` must be an absolute path: write out the full path of the file you
created (`echo ~/.config/agentdocker/typesafe.key` prints it), since `~` is not
expanded there.

`agentdocker daemon status` prints a `judge` line: `off`, `on` with the
model and the calls made in the last minute, or why it cannot run (a key file
others can read, a relative path, an address that is not https). The daemon
reads the file every few seconds, so no restart is needed. To turn it **off**,
delete the `[judge]` section or empty `judgments`; every wait it judged is
taken off at once.

**What is sent, and to whom.** For each quiet screen, one request to TypeSafe
(`https://api.typesafe.ai/v1/systemone`, or the `url` you set) carrying the
screen's last 30 lines as text, a fixed sentence saying they are a coding
agent's terminal, and four yes/no questions — is an approval
prompt open, is the agent asking you something, is work still running, is
there a message typed and not sent. Once a screen is answered it is not sent
again until it changes. A call that failed for a reason that may pass (no
connection, a timeout, a busy or unavailable service, a key not accepted) is
tried again after a minute; one the service refused, or answered in a shape
AgentDocker cannot read, is not. Your key goes in the request's
`Authorization` header and nowhere else. Nothing else is sent: no agent name
or id, no project, no path, nothing but what is on those 30 lines — which can
be anything the agent printed, so turn it on only for work you are content to
show TypeSafe. TypeSafe's own terms and costs apply; at most
`calls_per_minute` requests go each minute (6 unless you set it, 1–60).

**Whose screens.** The terminals of agents AgentDocker started (from the app
or with `run --tty`) and the tmux pane an agent registered from — at the tmux
server that pane belongs to, and only while the pane still runs that agent's
own process. A herdr pane, screen, zellij, and your terminal app's windows are
never read. A screen is read again just before it is sent and before its
answer is used, so a prompt you answered in the meantime is not sent or
acted on, and turning the judge off stops any upload still to come.

**What it can change.** Only one thing: an agent that works, is idle or whose
state is unknown can move into Needs you, when the likeliest of an approval
prompt, a question or an unsent draft is at least `threshold_percent` (75
unless you set it, 50–99) and work is not read as running. It never moves an
agent out of anything, and a wait its own hooks report always stands. The
judged wait ends when the screen changes, when the agent's hooks report
anything, or when you turn the judge off.

### Planned daemon replacement

`agentdocker daemon reload` requires `[experimental] reload = true` in the running
daemon's `agentd.toml` (or `AGENTDOCKER_EXPERIMENTAL_RELOAD=1` in its environment);
without that gate it returns `unavailable` and leaves the daemon and agents running.
The gated implementation transfers coordinator ownership to a checked successor
while session owners retain managed processes and I/O. The gate is experimental.
Ordinary daemon stop/start is a separate operation: stopping terminates managed
agents.

### Temporary review administration

The local `agentdocker secret-review list` command lists pending temporary review
metadata for the person. `secret-review cancel <id>` closes a route; it cannot
recall an answer already taken. `secret-review answer <id>
--acknowledge-provider-retention` accepts a complete JSON object of field IDs and
text values through a pipe, never command-line values or an echoing terminal.
It prints no answer. A failed or uncertain submission must not be resent
without inspecting the request state. Codex and its selected model receive the
answers and may retain or repeat them into ordinary output history.

Managed Codex can use this broker once `[experimental] secret_input = true` is in
the daemon's `agentd.toml` (or `AGENTDOCKER_EXPERIMENTAL_SECRET_INPUT=1` is in the
bridge's environment); default and native-input sessions still refuse secret bundles. Interactive managed terminals also require AgentDocker to
control echo, so Windows console secret input is refused.

The experimental managed secret route also has masked desktop entry with a
provider-retention acknowledgement. Submit once removes the local edits before
sending; a missing acknowledgement never restores the answer or resubmits it.

---

## Behaviour by area

Current behaviour that the sections above do not already state, by area.

### Board, plans and files

- **A review counts for the commit it read.** A reviewer gives its verdict
  on a branch at the exact commit it read (`agentdocker verdict`, or the
  `review_verdict` tool), and `agentdocker gate <PR|branch>` says whether
  the branch may land now: approved only at that commit. Any later commit,
  a merge of main included, makes the approval stale until the reviewer
  reads what came after. Whose word counts is a role you give
  (`reviewer` by default); `--publish` also sets it as a GitHub commit
  status that branch protection can require. An agent's verdict is
  refused on the branch its checkout is on, and on any branch it
  committed to through `agentdocker commit` or with git in a worktree
  AgentDocker made for it, wherever its checkout is now and however
  much of the journal is gone. Checking a branch out to read it is not
  committing to it. Commits made with git in a checkout you made by hand
  are not seen this way, so keep authors and reviewers separate agents.

- **Rules, tags, plans and files on the board.** A board's rules comment on,
  move, archive or start an agent on a card when it comes into a lane or
  has sat there some days (`task rule-add`, the board's settings). A
  project's tags have colours, and an agent tags only with the project's
  (`create_tag` makes one). Plans are specs in Markdown that cards carry out
  (`plan`, `save_plan`), and project files sit beside them, linked to cards
  (`file`, `put_file`). `task start` (or **Start an agent** on a card)
  launches Codex or Claude Code with idle message delivery and hands it the card;
  a start rule uses the same input setup. Unsupported delivery, a missing matching
  AgentDocker CLI or incompatible provider arguments refuse before launch. Complete
  provider trust and channel consent in the terminal; tool permissions still apply.
  `task search` and `search_tasks` find cards by words with
  slips forgiven. The desktop adds To-do, a table layout, compact cards,
  tag and agent filters, board shortcuts, and opening, copying and adding
  links. Browser agents can read the board, plans and comment on cards.


- **A full board, for agents.** A board is lanes you name, each with
  a category (Backlog, Ready, In progress, Review, Blocked, Done, Cancelled),
  whose turn it is (anyone, you, an agent, or a role) and an optional limit;
  `classic`, `agents` and `simple` templates start one, and a project can
  have several. Cards have a key (`AD-12`), a description, priority, size, due
  date, tags, a parent, cards they wait on and a blocker off the board, plus
  comments with replies and a history of everything done to them. An agent
  pulls the next ready card that waits on nothing (`task pull`, `pull_task`
  with no card), sees its own (`my_tasks`), files a plan at once
  (`create_tasks`) and comments as it goes; you hear nothing you did not do,
  and an agent hears when you move, hand or comment on its card. The desktop
  board files with a line at a lane's foot, moves by drag, and opens a card
  in place.


### Messaging and delivery

- Conversations: every message is archived in the one conversation its
  destination names (`everyone:<project>`, `all`, `channel:<id>`,
  `dm:<a>:<b>`, `notices:<agent>`), beside the queue it is delivered to and
  never instead of it, bounded by a per-conversation cap and `[messages]
  retention`. `conversations` lists them with unread counts per reader,
  `history` reads one back with reply counts, `thread` a root and its
  replies, `search` finds text; reading moves a cursor forward, never back.
  Channels get a `#name`; a person is not put in a collision room.

- The app keeps to each project's chat and your direct messages. Messages
  lists **Projects** (each project's chat, opening its Chat) and **Direct
  messages**; `#all`, channels and the rooms AgentDocker opens over contested
  paths are not in the app — no More → Channels, no new channel or
  **Add members**, no **Reviews** — and what reaches you there is marked read,
  so it never piles up in your queue or the badge. They are on the
  CLI: `agentdocker conversations`, `agentdocker history all` or
  `agentdocker history channel:<id>`, `agentdocker channels`, `agentdocker
  channel …` (including `agentdocker channel invite --as <member> <channel>
  <agent>`) and `agentdocker send --to channel:<id>`. A notification about
  such a message opens your conversation with its sender and names the
  command; a reply typed into it is refused with the command to use instead.

- Projects keep their own messages. An agent reaches only its own project's
  agents, its channels and you: a send, ask or question to another project's
  agent, project or channel is refused, and an agent's `all` means its own
  project. What an agent says on a topic reaches its own project's
  subscribers and you, never another project's agents. `list_agents` and the
  session-start note do not name agents in other projects. You can still
  message anyone anywhere.

- Topics keep their messages for whoever subscribes later: at most 1,000 per
  topic and seven days, or what `[messages] topic_limit` and
  `topic_retention` say in `~/.agentdocker/agentd.toml`:

  ```toml
  [messages]
  topic_limit = 1000       # messages each topic keeps, 1–100000
  topic_retention = "7d"   # how long it keeps one: 30m, 12h, 7d or seconds
  ```

  An agent that watches a topic gets what it has not acknowledged there and
  picks up after its last acknowledgement on its next watch, across a
  daemon restart too. Topics are not conversations: they are not in
  `conversations`, `history` or the app.

- The Messages workspace: **+** for a new direct message, `@` mentions
  with counts, Enter to send, resizable panes, an Earlier group for ended
  sessions' conversations.

- Needs you holds only questions waiting on you, each naming its project. A
  session that is blocked, not receiving messages or ended with messages
  queued says so on its own row instead. Messages lists only conversations
  with something in them, and an ended session's conversation only while it
  holds something unread.

- Under your own messages in the app, one line per agent says where each
  message stands: answered, delivered, received by the model, shown to its
  session, or why it is still waiting.

- A session that opens within an hour of your message to everyone in a project
  still gets it, as its first input. Only your own messages count, and only
  once.

- A Claude channel session's first message survives the moment before
  Claude's channel handler is registered: when no receipt has yet been seen on
  the channel, an offer without one after thirty seconds is made a second
  time, once, under the same message id and marked `repeat`. A receipt the
  daemon accepted counts whichever message it names, one sent before the first
  offer included. After the first receipt there is no replay, and a missing
  receipt pauses the queue visibly.

- A channel message is confirmed by a later reply in the session even when the
  transcript could not link the reply to the message, so the queue behind it
  (a Pause included) is not held for a receipt that was never recorded.

- A Claude Code session survives Claude's own restart. Restarting in place
  re-executes `claude` without the channel flag; the flag on the running
  `claude` decides whether the channel is used, and the hooks deliver whatever
  a stalled channel holds at the session's next prompt or tool use.

- **A provider outage does not stop an agent for good.** When a provider's
  servers fail (overloaded, unreachable), the agent's queued messages wait a
  minute and then go to it again, twice as long after each failed try and at
  most half an hour; the first turn that goes through ends it. Limits and
  sign-in problems wait for the provider or for **Resume delivery**.


- Reply from the notification: on macOS a message notification has a
  **Reply** field, and what is typed there reaches the conversation — an
  answer to a question closes it — without opening the window.

- Notifications open their message: a click on a notification for an
  archived message opens the conversation on the Messages screen and
  scrolls to the row, paging back a bounded number of pages for an older
  one, even when that conversation is already open.

### Agents, roles and setup

- **A Codex started in a terminal is itself.** A plain `codex` in a terminal
  has its own record, by its thread: its AgentDocker tools work as it, and
  its messages reach it at its next prompt. It cannot be woken while idle; a
  Codex launched from AgentDocker can.

- **Codex's shared app-server says where a terminal Codex stands.** Where
  Codex runs its terminals' conversations in its shared app-server (0.162 and
  later, `$CODEX_HOME/app-server-control/app-server-control.sock`), the daemon
  reads each conversation's status there: one stopped for an approval or a
  question shows as waiting for you, one running a turn as working, a quiet
  one as idle. That is the server's word, not the agent's: while the server
  keeps saying it, the agent's own tools calling in do not make a quiet or
  waiting Codex read as working, and where herdr also follows its pane, the
  server's word wins. `agentdocker events` marks these `(Codex server)`.
  A terminal resumed with `codex resume <thread>` is also fed through that
  server, with the same exact receipts as any Codex receiver: a message that
  arrives while it works goes into the running turn, one that arrives while
  it waits for you waits too, and otherwise it goes at idle through that
  server's own queue. Without the socket nothing changes. `agentdocker
  codex-threads` lists what that server has loaded, read-only
  ([Codex input](CODEX-INPUT.md#terminals-in-codexs-shared-app-server)).


- **Your own terminal's Codex can be the reviewer.** `agentdocker codex`
  starts Codex in the terminal you are in, through AgentDocker's launcher:
  messages wake it even while it is idle, and you can give it a role, such
  as reviewer, like any agent. Codex's own options go after `--`. While
  Codex has the terminal AgentDocker writes nothing into it: the INPUT
  column of `agentdocker ps` and the app say when it can take messages, and
  the launcher's own words go to its launch log. A plain
  `codex` still works, but reads its messages only at its next prompt.


- **Roles in the app.** In a session's panel, **Roles** gives an agent roles
  from its project's palette, several to an agent and one role to several
  agents; they show as coloured labels after its name. **Edit roles…** in the
  same menu recolours, deletes or adds a project's roles. Nothing gets a role
  unless you give it one.


- **Only you act as you.** A command or tool that acts as you — gives a
  role, answers a question or an approval, pauses a project, merges —
  is refused when it comes from inside an agent's session, whatever it
  says about who sent it: the daemon checks which process is asking. Run
  those from your own terminal or the app. A launch does not carry a
  role in: `agentdocker run --label role=…` and an Agentfile's
  `labels = { role = … }` give the roles after the launch, as you. Nor
  does it say who launched it: an agent launched from your own terminal or
  the app, and a session that registers itself whose process's parents
  pass through your terminal app or IDE, `login` or `sshd` with no agent's
  session anywhere among them, is marked yours by the daemon
  (`launched_by=user`). On macOS that app counts when root installed it
  (the system's Terminal, an App Store or administrator's install) or, for
  one you installed yourself in `/Applications` or `~/Applications`, when
  its code signature verifies (`codesign --verify --strict`, signed by a
  developer Apple vouches for, checked afresh each time) and the program
  running is the app's own; a check that times out leaves it unsaid until
  the session registers again.
  One with an agent's session among its parents, even above a terminal
  that agent opened, is marked `agent`, and one whose parents can no
  longer say (an orphan, a multiplexer server) `unknown`, whatever its
  labels say. What is written is never rewritten. It is for showing and
  filtering your cards, never permission for anything: a window an agent
  opens through the system's app launcher (`open`, `osascript`) looks just
  like one you opened.


- Codex input needs an app-server of 0.154.0 or later and refuses an older one
  when it attaches, naming both versions. A turn Codex refuses outright leaves
  the message queued with the provider's reason on the record.

- **Reconnect here** in a Claude Code session's Details, and `agentdocker
  reconnect <session>`: once the session has exited in its terminal, the daemon
  brings it back under its own record with its conversation (`--resume`) and
  the AgentDocker channel, so it takes messages live and what was queued for
  it stays its own; the app opens its pane, where Claude's consent prompt
  appears. Refused with the reason while the process still runs, in another
  checkout or with somebody attached. The copyable command stays as the
  alternative.

- `claude attach` (shown as `claude agents`) is the terminal in front of
  a background Claude Code session, whose `bg-spare` process registers itself:
  neither is discovered or adoptable as a second agent. A command run from
  that session's tools (`agentdocker send`, `ask`, `pause`…) is still that
  session's: the CLI finds its `bg-spare` ancestor and sends as its agent, not
  as you.

- `agentdocker setup --shell` makes every terminal `claude` carry the channel
  flag so AgentDocker can wake it; the Claude Code card in Tools offers it as
  **Wake terminal sessions**, and the MCP server reads the flag from its
  parent `claude`, so `AGENTDOCKER_CLAUDE_CHANNEL_INPUT` is not required
  from a terminal.

- Agents that work inside a browser are inventoried: `runtimes` lists each
  vendor's extension per browser profile (`claude-browser`, `chatgpt-browser`)
  and says that its sessions run in the browser and cannot be listed or
  messaged; a runtime's helper such as `claude --chrome-native-host` is never
  discovered and cannot be adopted; `rm` on a live external record says
  `deregister`.

- `agentdocker runtimes` heads its last column UNREGISTERED: it counts a
  tool's processes nobody registered, which `ps` never shows as sessions.

- A provider session that comes back as a new process (a resumed Claude
  or Codex) is folded into the record it had, keeping its queue and name.

- Setup writes the channel-capable Claude MCP entry (`--claude-channel`)
  and health checks recognise it; a plain entry stays valid MCP.

- Finished agents' records are forgotten by themselves. Thirty days after an
  agent ended, if it holds no lease, has no open question, no queued message
  and no input binding, and is not set to restart or restore, its record goes the way
  `rm` would take it (`agent_removed`, and the batch as `agents_pruned`);
  `[agents] retention = "90d"` in `~/.agentdocker/agentd.toml` changes the
  window and `"off"` keeps every record. The journal and the ledger keep a
  forgotten agent's id.

### Sessions and worktrees

- Sessions start in their own worktree. In a git project, **Launch agent…**
  has **Own worktree: On** by default: the session works on a branch of its
  own in its own folder, its changes are reviewed and merged from Changes, and
  once merged and ended, **Remove its worktree** (or `agentdocker
  worktree-remove`) cleans up. Files git ignores that the project lists in
  `.worktreeinclude` (one pathspec per line, e.g. `.env`) are copied into each
  new worktree on macOS and Linux (not on Windows, where the worktree says so).

- One-click review: a session's details show **Changes** — its branch against
  the branch in the project's main folder, the commits and files, each file's
  diff, and what a merge needs — and **Merge into main** merges it with a
  merge commit, or says why not with nothing changed. `agentdocker
  review-branch` and `merge-branch` do the same from the CLI. Nothing is
  pushed.

- **Changes hands repairs to the agent.** **Ask <agent> to commit**, **Ask
  <agent> to resolve** (naming the conflicting files) and **Ask <agent> to fix
  checks** send the words to the agent in one click; the merge command is
  printed under **Merge into main**, and after a merge the panel says what
  happens to the worktree next. A command a session asks to run is allowed or
  denied from the Needs-you row itself. Ended sessions are grouped by the day
  they ended. A board card shows its holder's standing and pull request and
  moves on to Review when the pull request opens and to Done when it merges.
  No notification is posted about what the focused window already shows, and
  an agent's newer notification replaces its last.


- **Launch agent…** waits up to 15 s for a new worktree to be watched, so a
  launch on a busy machine does not fail with *watcher did not confirm checkout
  coverage within 500 ms*. A launched agent opens in its terminal, where
  Claude's folder-trust and channel-consent questions wait to be answered
  before it can start.

- A turn's end gives back only the per-file edit leases the Claude Code `Stop`
  hook took itself (`automatic`), so a worktree, branch or build-campaign lease
  claimed through `claim` or the MCP tools holds until released or expired.
  `SessionEnd` gives everything back.

- Commits in a private checkout are attributed: a worktree made with
  `agentdocker worktree-create` (with `--from <ref>`) is remembered as its
  maker's, and a checkout held under an exclusive `path:` lease is its
  holder's, so `git commit` there is journaled as that agent's rather than
  `external`. `claim`, `renew` and `release` act as this session without
  `--as`. `scripts/verify.sh` takes the machine's `task:local-cargo-campaign`
  lease for its run, keeps one the caller already held, renews either while
  the run lasts, and stops instead of starting on top of another campaign —
  before the first step, or at whichever step a renewal fails, ending only
  the processes that step started; when the daemon cannot tell which
  session is calling, the run registers itself as an agent (`verify-<pid>`,
  ended with the run) and holds the lease as that, and where a daemon
  answers but will not let the run hold the lease, the run does not start
  (`AGENTDOCKER_CAMPAIGN_LEASE=off` is the explicit override); the scripts
  it runs in turn inherit that decision (`AGENTDOCKER_CAMPAIGN_LEASE=off`)
  rather than negotiating the slot against their own parent, and the
  suites run without a managed session's identity, socket or home in the
  environment.

- The watcher covers every checkout of a project, not only the one an
  agent registered in. Commits in a worktree nobody registered reach
  the journal and the ledger, and `overlap` compares real checkouts.

### Desktop

- **Choose the model and effort.** Launching Claude or Codex from the app
  offers **Model** and **Effort**, starting at the provider's own defaults,
  and a session's panel says which it runs (`<model> · <effort>`).


- **Stopping a terminal agent leaves its terminal clean.** Stopping a Codex
  or Claude you started in your own terminal turns mouse-reporting modes off
  once the program has exited. In a tab left reporting every mouse move as
  typed text, type `reset`.


- **A wait read from the screen.** With the opt-in screen judge on (see
  [Let a model read a quiet screen](#let-a-model-read-a-quiet-screen-opt-in)), an
  agent whose quiet screen shows an approval prompt, a question or an unsent
  draft is in Needs you with its errand chip and a second chip, **Judged · 87%**,
  saying it was read from the screen and how sure the model was.


- **Allow for session.** A Codex approval that Codex offers to remember has a
  third answer beside **Allow once** and **Deny**: **Allow for session** lets the
  same command, network destination, files or access through without asking
  until that Codex session ends (`agentdocker answer <id> "Allow for session"`
  from the command line). Offered only where Codex offers it.


- **Every state in a word.** The desktop says each state with a labelled
  chip — an icon and its word (*Needs you*, *Working*, *Idle*, *Delivered*,
  *Receiving messages*, *4m left*), healthy states too — with the fix, when
  there is one, as a button beside it; a tooltip only adds detail. It has no
  sentences under headings, empty-state captions, key footers or section
  intros. It is set in IBM Plex Sans, with an ink primary action.


- **A quieter, neutral desktop.** The sidebar is neutral, one step off the
  page, in both appearances; colour is kept for state and the one primary
  action. Every agent row is one line with a chip for where the agent stands
  — the same function decides it in the chat, the session list and the
  rail. The project
  header keeps **Launch agent…** as its one labelled button; Pause, the
  terminal and Open in are icons named in their tooltips. A project's rail row
  shows its loudest agent's chip, counting the agents that need you when more
  than one does.
  **Launch agent…** starts from what the agent should work on, sent as its
  first message; its name and arguments are under **More options**.
  Command/Ctrl+Shift+U goes to the next agent that needs you. The
  background-service notice is one quiet line until you act on it.


- **Told when an agent finishes.** While the window is not in front, an agent
  that finishes its turn gets one quiet notification, replacing its last;
  **When an agent finishes or waits** in Settings turns it off.


- **Told when an agent waits on you.** An agent that stops at a permission
  prompt or question in its own terminal while the window is not in front
  gets one too, saying which. A notice is taken back once it is no longer
  true, and a finish is not told when your next message is already queued.


- An agent's row, in a project's chat and in the session list, shows its
  branch's pull request as one chip — **#<n>** and its checks in a word
  (**Passed**, **Running**, **Failed**, **Merged**) — that opens it in the
  browser. It is
  read with your signed-in `gh` every two minutes and when the window regains
  focus; without `gh`, on a default branch or off GitHub there is no chip.
  Every agent row says where the agent stands in a chip (**Needs you**,
  **Failed**, **Working**, **Finished**, **Running**, **Idle** — only when its
  tool reported it quiet), the same in the chat, the
  session list and the rail. An agent that finished while you were not
  looking keeps its **Finished** chip until you open it or its project.

- **Open in…** (the code icon beside the terminal icon in a project's header) opens the project
  in an editor installed on this computer — Visual Studio Code, Cursor,
  Windsurf, Zed, Sublime Text, Nova, BBEdit, Xcode, Fleet or a JetBrains IDE —
  from a small menu of the ones found, the last one chosen first. A session in
  its own worktree offers it in Details for that worktree, and the command
  palette has one **Open in** row per editor.

- A Claude Code session stopped at a permission prompt or question shows as
  *waiting for you*: it is in Needs you with **Go to Terminal**, counted on
  the dock icon, and `agentdocker activity` shows it `waiting`. A permission
  prompt's row also says what it is for (*Bash: cargo test*, *Edit:
  src/lib.rs*). It needs the `Notification` and `PermissionRequest` hooks, so
  run `agentdocker setup claude-code` to add them to an existing install.

- An agent with no AgentDocker hooks of its own — Codex without them, Gemini,
  another CLI — that runs in a herdr pane shows what herdr says of that pane:
  working, *waiting for you* when herdr marks it blocked, and idle when it is
  idle or done. The pane is the one holding the agent's own session, so it is
  followed when the pane moves, and herdr must know that session (its
  integration for the agent, `herdr integration install <agent>`); herdr's
  default and named sessions are read, and nothing is ever sent to herdr.
  `agentdocker events` marks these `(herdr)`.

- Clicking an agent in a project's chat goes to its terminal: the tab in
  Terminal or iTerm it runs in, its tmux or herdr pane, or the editor or app
  its terminal is in. Its details are on the chevron beside it, and they say
  where it runs. macOS asks once to let AgentDocker control Terminal or iTerm.

- The app forgets the note it keeps about a failed answer once the question
  it belonged to is gone.

- The dock icon counts the questions waiting on you, across projects, and
  Command/Ctrl+N opens Launch agent.

- **Back and Forward** are the two arrows beside the mark at the top of the
  sidebar, and Command/Ctrl+[ and Command/Ctrl+] (or the mouse's side
  buttons): they go through the places you went in this window — a
  project's screens, the session, card, thread or conversation open on them —
  as a browser does, passing over a project, session or card that is gone.
  Pointing at one says where it goes.

- The desktop app ships as `AgentDocker.app` on macOS, with its own icon,
  so the Dock and the app switcher name it properly. `agentdocker ui`
  first launches a matching sibling `agentdocker-ui`, then falls back to the
  bundle when no sibling exists. Local preview bundles use ad-hoc signing. Public macOS
  distribution requires Developer ID signing and notarization.

### Usage

- Token usage counts the Claude Code or Codex release you are on: a release of
  the same major version as one that has been checked is counted as it
  reports, and the usage report's coverage says unchecked releases may be in
  it.

- Tokens where you look: `agentdocker ps` has a TOKENS 24H column, and the
  desktop's session rows show each session's tokens over the last day, with
  one line above the list that sums them and opens Usage. Both appear only
  when usage collection is on and there is something to show.

### Project memory

- **Project memory.** Agents and you can keep short facts for a project —
  a convention, a decision, where something lives — with `add_memory` or
  `agentdocker memory add`. Every new Claude Code, Codex and OpenCode
  session in the project is told them as it starts, the standing ones
  first and then the newest, as context for the model, never typed into
  its terminal (Codex runs its start hook at a conversation's first turn,
  and again after `/clear` or a compaction). Search finds whole words, the
  rarer ones counting for more and the newer memory first on a tie; an
  identifier such as `task_id` is one word, so `task` does not find it,
  while `-` and `:` separate words. A memory that is no longer true is
  replaced (`--supersedes`) or forgotten. It stays on this machine; anything that
  looks like a secret is refused. To keep a copy in Supermemory as well,
  name the projects and a key file under `[memory.supermemory]` in
  `agentd.toml` (off unless you turn it on; the hosted service is
  Supermemory's, on its terms). `base_url` names a server of your own
  instead; its certificate is checked against the authorities your system
  trusts, so one from your organisation's own authority is accepted once
  your system trusts that authority.
- **How Supermemory forms memories from the copy.** Two optional lines
  under `[memory.supermemory]`, neither sent unless you write it:
  `dreaming = "dynamic"` (related memories are grouped before Supermemory
  learns from them) or `dreaming = "instant"` (each is learned from at
  once, which Supermemory bills as one more operation per memory), and
  `supporting_context = "…"`, up to 1500 characters of background that
  guides what it learns. With `dreaming` set, a memory counts as copied
  only once Supermemory says its memories are formed. A longer context or
  another `dreaming` word stops the mirror with a notice naming the limit
  or the two words. Leave both out for a server of your own that does not
  take them.


### Remote connector

- **A pairing code works once.** The connector's consent page asks for the
  pairing code first and shows the projects to choose from only after it.
  Each code opens one consent, so connecting a second browser means reading
  the new code from `agentdocker connector status` or the Tools screen.
  A source that types five wrong codes waits a quarter of an hour, and a code
  guessed at twenty times is replaced. A service's log does not record the
  code. A connector that crashes leaves no Tailscale Funnel open: the next
  start clears it.


- The remote connector: `agentdocker connector serve --public-url <https://…>`
  serves an OAuth-protected MCP endpoint on loopback for a tunnel you run, so
  the clients of a Claude or ChatGPT account (the browser side panel, the
  apps, a terminal reaching the account's connectors) can join a project as
  one agent per consent — registered when its code is redeemed, after the
  pairing code from the terminal is typed on the consent page — with the
  messaging tools only. `connector grants` lists consents and what each
  client said about its software, `connector revoke <agent>` ends one.
  `--tunnel tailscale` exposes it through Tailscale Funnel on this machine's
  own stable name and `--tunnel cloudflared` starts a quick tunnel;
  `connector install` runs it as a login service, `connector status` shows
  its address and pairing code, and `--allow-from` admits only the vendors'
  published addresses. One connector serves every project on the machine:
  the consent page chooses the project the agent joins, and its
  `project` broadcast names that project, wherever the connector runs. A
  vendor may identify itself by its Client ID Metadata Document instead of
  registering (fetched only from the vendors' hosts); the desktop's Tools
  screen shows whether the connector is serving, its URL and pairing code.

- Every MCP tool carries annotations (read-only, destructive, idempotent,
  open-world), so a host that asks before risky calls lets the reads through.

### Daemon

- **An idle daemon rests.** With nothing watching — no app open, no
  `agentdocker events`, no webhook — the daemon does not scan the process
  table periodically; `agentdocker ps` and the app scan when they look. The
  file watcher's drain runs only while changes wait to be recorded, and the
  usage worker waits between collections in one-second steps.


- `agentdocker --help` lists the commands a person types. The ones only an
  adapter or an administrator runs (`heartbeat`, `report-activity`,
  `report-adapter`, `report-input`, `identity-repair`, `secret-review`,
  `usage-storage`, `usage-scan`, the `codex-*` controllers) are hidden from
  the list and still work by name; the guide's reference still names them.

- The ledger entries for one batch of file changes are written in one
  transaction, so a branch switch does not stall claims and releases.

- The daemon keeps serving through a momentary accept failure on its socket
  (descriptors exhausted under a burst of clients, a handshake the client
  dropped): it retries with a short backoff instead of stopping.

- A state database held for a moment by another connection (an offline
  repair, a successor opening it, a `sqlite3` shell) does not put the
  daemon into read-only `storage_unavailable`: a write waits up to a second
  for the lock first, and every write transaction takes the lock as it
  begins, so one that reads before it writes cannot meet a busy database
  after the wait.

- An event kind a client has never heard of is ignored instead of taking
  the event stream down with it.
