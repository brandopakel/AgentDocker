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
- [What changed](#what-changed)

---

## Install and first run

```sh
curl -fsSL https://raw.githubusercontent.com/brandopakel/AgentDocker/main/install.sh | sh
```

On a Mac that installs the desktop app — with the `agentdocker` command and
the daemon inside it — through the app's own installer, so `agentdocker desktop
update` and rollback work from the first install; on Linux, or with
`AGENTDOCKER_INSTALL=cli`, it puts the two commands under `~/.local/bin`. A
release that has no desktop archive yet (v0.1.0) falls back to the commands and
says so. Until the app is signed with a Developer ID, macOS quarantines a
downloaded copy: right-click it and choose **Open** once, or
`xattr -dr com.apple.quarantine ~/Applications/AgentDocker.app`. From a checkout:

```sh
cargo install --path crates/cli --locked   # agentdocker + agentd
cargo install --path crates/ui  --locked   # the desktop app
```

The Linux desktop archive needs a graphical session and its native display
libraries; the archive does not install distribution packages. On the tested
minimal Ubuntu 24.04.4 VM, the app initially failed because
`libX11-xcb.so.1` was absent. The isolated acceptance run supplied extracted
X11/XCB, keyboard, rendering and font libraries plus Xvfb. That test does not
establish the prerequisites on a standard Ubuntu Desktop install. Before
launching an extracted archive, run `ldd` on its `agentdocker-ui` executable
and resolve any `not found` dependencies for your distribution. The CLI-only
installer does not install the desktop or those libraries.

You do not start the daemon. The first client that needs it starts it, on
`~/.agentdocker/agentd.sock`. To have it survive a reboot:

```sh
agentdocker daemon install    # launchd, systemd user unit, or Windows login task
agentdocker daemon status     # what is running, and where
```

On Windows, installation creates a Task Scheduler task for the current user
and starts it immediately. It runs with limited privileges at login without
storing a password; an interactive sign-in is required. `daemon start`, `stop`,
`restart` and `uninstall` operate on that task. A supervisor retries a failed
daemon up to three times with two seconds between attempts; a clean shutdown
stays stopped. Ten minutes of continuous operation resets that retry budget.
The task records the current CLI and daemon paths. After moving the portable
folder or selecting another build, run `daemon install` from the new build.
Keep the old folder until that succeeds. An ownership record in the daemon
home prevents replacing or removing a task whose action or user was changed
outside AgentDocker. `daemon install --dry-run` previews the task without
registering it.

Then bring in the agents already on the machine:

```sh
agentdocker runtimes        # what is installed, and whether we are wired into it
                            # (HOOKS `no (StopFailure)` names the events setup would add)
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
(`Claude in Chrome 1.0.93`) and says under the table that its sessions run in
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
ChatGPT and a pairing code for the consent page, and each consent becomes a
browser agent in the project chosen on that page (any folder on this machine)
with the messaging tools and nothing that touches a checkout. `connector
install` runs the same as a login service; `connector status` and the
desktop's Tools screen show its address and pairing code. On macOS and Linux,
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
daemon directly. See [the desktop guide](DESKTOP-UX.md) for every interaction and
[remaining work](REMAINING-WORK.md) for engineering and release limitations.

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
- **Activity** shows the recent project journal, newest first. **More → Channels** shows
  project rooms and messages queued for you. **More** also holds **Files in use**,
  **Command line**, and project pin/forget actions.
- **Pause…** on the project header asks for a reason and tells every agent in
  the project to hold; the daemon refuses their new leases until **Resume**.
  What an agent already holds, it keeps; only you can pause or resume.
- **Board** (in review as PR #176) is the project's cards: file work with a
  title and what done means, agents pull Ready cards once, and the columns
  say where everything is.

On narrow windows the selected session replaces the list, with **Back to sessions**
to return. Wide windows show actions beside the list. The human `user` identity
remains available for coordination and inbox delivery but is omitted from the
session list.

### Why an agent reads "idle"

Without fresh provider observations or recent coordination, activity is unknown.
Recent coordination can establish working; an explicit provider stop produces
provisional idle that expires or is superseded by newer activity. Process
presence, provider configuration and observed activity are separate facts.
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
reverses only unchanged configuration. **Check connections** provides bounded diagnostics;
actual provider delivery requires a real round trip.

### Usage

The #194 candidate adds **Projects → Usage** and `agentdocker usage`. Check the
[installed-build status](REMAINING-WORK.md) before expecting this in an older app.
Choose the last day, week or month and group reported tokens by agent, model,
provider, project or hour. `~` marks a partial count; `—` means the source did
not report that counter. These are token totals, not a bill. The report shows
the available time range, gaps and whether collection has caught up. The CLI
names each agent or project row (`codex-96813`, `AgentDocker`) while AgentDocker
still knows it, and shows its ID once it is gone; `(unattributed)` is usage no
session could be matched to.
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
AgentDocker's own injected overhead remains **not measured** until that separate
instrumentation is implemented.

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
inspect codex-27221
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
| 5 | refused: not the caller's to do, or the project is paused | `forbidden`, `paused` |
| 6 | not now: the daemon, its storage, an engine or a build is unavailable, busy, handing over, timed out or cancelled | `storage_unavailable`, `unavailable`, `engine_unavailable`, `build_failed`, `backpressure`, `timeout`, `cancelled`, `transferring`, `event_history_lost` |

The class holds wherever the answer was read — a `daemon reload` refusal, an
`attach` the daemon refuses or ends with an error — and `adopt --all`, which
tries every process, ends with the class of the first refusal after naming
each one by pid.

### Look at the fleet

| Command | What it does |
|---|---|
| `ps` | Agents grouped by project, with INPUT readiness; `--input-details` adds reconnect guidance |
| `top` | The fleet live, redrawing as the daemon reports changes |
| `activity` | What each agent is doing: working, idle, or blocked on a named resource |
| `usage` | Tokens the providers reported, filtered explicitly with `--agent <id>` (`--as` alias) or `--project <id\|path>`, one row per agent (`--by model\|provider\|project\|hour`), each count with its coverage (`~` where some samples did not say, `—` where none did), and under the table what the totals cover: the range answered, retention, gaps, whether collection is on, and the overhead AgentDocker injected — *not measured* until it is; `--since 24h`, `--json`. `AGENTDOCKER_AGENT_ID` does not narrow this query. |
| `inspect <agent>` | Everything known about one agent, as JSON |
| `logs <agent>` | An agent's captured output, at most 16 MiB kept per agent (the newest 8 MiB and the 8 MiB before them); `-f` to follow, `--compress` for an rtk view |
| `validation <id>` | The retained log of one validation; `--compress` for an rtk view |
| `events` | The daemon's event stream; `[[webhooks]]` in `agentd.toml` posts a signed copy of chosen kinds to an address you name (see [webhooks](ARCHITECTURE.md#events)) |
| `ping` | Check the daemon is reachable |

### Start, adopt and stop

| Command | What it does |
|---|---|
| `run <command>` | Launch a command as a supervised agent |
| `register` | Announce an already-running process as an agent |
| `discover` | Agent processes nobody registered |
| `adopt <pid>` | Register one of them; `--all` for all |
| `stop <agent>` | Signal an agent to stop |
| `restart <agent>` | Replace a managed container after confirming it exited |
| `deregister` / `rm` | Mark an external agent finished / forget a finished one |
| `role <name>` | Give an agent (`--as`) a role — `reviewer`, `implementer` — so `send --to role:reviewer` and `handoff role:reviewer` reach it; `--clear` takes it away |
| `rename <agent> <name>` | Give a live agent a name of your choosing (up to 64 characters, unique among live agents); its id and everything addressed by id are unchanged |
| `deregister --as <agent>` / `rm <agent>` | Mark an external agent finished, without signalling its process / forget a finished one. `rm` on a live agent says which of the two applies: `stop` for one AgentDocker started, `deregister` for one it did not |
| `up` / `down` | Start or stop the agents in an `Agentfile.toml` |
| `heartbeat` | Report that an agent is alive |

### Talk

| Command | What it does |
|---|---|
| `send` | Message an agent (or `role:<name>`, the one agent with that role in your project), the project, a topic, or everyone. A `--link kind:target` (repeatable) travels beside the text: a path, a commit, a pr, a url, a task, a message or a memory for the reader. |
| `watch` | Stream messages for an agent or matching topics |
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
in a session without an input receiver. `agentdocker ps --input-details` names
these sessions and gives reconnect guidance; it does not restart them. Provider
limits remain separate from receiver health. An older daemon can report readiness
as unavailable rather than imply that every recipient can wake.

For an existing Claude session with no channel, save the current work, exit that
Claude session, and either press **Reconnect here** in the app's session Details,
run `agentdocker reconnect <session>` (`--claude <path>` when the tool is not on
PATH as `claude`), or use the session-specific resume command shown in Delivery
details or `ps --input-details` from its project folder. The first two bring the
session back under its own record with its conversation and the channel — what
was queued for it stays its own — and refuse with the reason while its process
is still running, in another checkout, or with somebody attached; the app opens
its pane, and `reconnect` prints the same id. Accept Claude's prompt there.
Complete Claude's startup channel consent. The AgentDocker MCP entry must include `--claude-channel`; see
[Claude channel setup](CLAUDE-CHANNEL-INPUT.md). Hooks alone cannot start an idle
turn. A copied instruction is not executed by AgentDocker.

### Share a resource

| Command | What it does |
|---|---|
| `claim` | Claim a lease on `path:`, `branch:`, `task:`, or anything |
| `renew` / `release` | Extend or give up a lease you hold |
| `leases` | Every lease held right now |
| `waiting` | Claims waiting for a resource, oldest first |
| `task` | The board of work: `task create "Fix login" --acceptance "SSO works" --column ready --link pr:#176 --link path:crates/core/src/task.rs` files a card (a `--link` is `kind:target` — path, commit, pr, url, task, message or memory — and `task update --link …` replaces them, `--no-links` clears them); `task pull <id> --as <agent>` takes a Ready card once and holds it as a `task:<id>` lease (four hours; `renew` extends it, an exit releases it; a card whose hold lapsed is taken over only with `--take-over-from <holder>`, naming whom it is expected from); `task move <id> review` (an agent moves only a card it holds; back to Ready or to Done ends the hold), `task update <id> --assignee bob` (a hand ends the old hold and takes the lease for a running agent), `task archive <id>`, `task list [--column ready] [--archived] [--limit 100] [--offset N]` (a page; it says when the board goes on and where the next page starts) |
| `pause` | `agentdocker pause "sleeping the laptop"` tells every agent in this directory's project to hold: they get the reason as a `pause` message and their new leases are refused until `agentdocker pause --lift`; `--project` names another project; `pause --list` lists what is paused and why |

### Know what changed

| Command | What it does |
|---|---|
| `journal` | What changed and why, one line per entry; `add` appends a note; `prune --before <seq\|duration>` trims (or set `[journal] retention` in `~/.agentdocker/agentd.toml`) |
| `changes` | The ledger: file changes seen in a project, with who held each file |
| `blame <path>` | Who changed a file, oldest first |
| `overlap` | Paths changed in more than one checkout: what will collide |
| `observe` / `reads` / `stale` | Record what was read, and check it is still true |

### Work in isolation, then merge

| Command | What it does |
|---|---|
| `worktree-create --branch <name> [--from <ref>]` | A new linked checkout and branch, at your HEAD or at `--from`, without touching existing files; commits you make there with git are journaled as yours |
| `worktree-diff` | Tracked changes in an agent's checkout |
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
| `attach <agent>` | Connect this terminal to an agent's; Ctrl-] detaches |
| `daemon` | Install, start, stop, reload, inspect or `vacuum` `agentd` |
| `desktop` | Install, inspect or roll back the native desktop for this user |
| `identity-repair` | Preview a legacy identity repair; apply it only with daemon and sessions stopped |
| `report-activity` | Report an observed provider turn state (expires after five minutes) |
| `cancel-question` | Close a question you asked; messages and answers are retained |
| `hook` | Handle a hook event, or install the hook configuration |
| `mcp` | Serve our tools to an MCP host over stdio |
| `connector serve` / `status` / `install` / `enable` / `uninstall` / `grants` / `revoke` | Let an agent that works inside a browser join the messaging of any project on this machine (chosen at consent): served on loopback behind a tunnel you run or one it starts (`--tunnel tailscale` for a stable name, `--tunnel cloudflared`), as a login service with `install`, admitting only the vendors' addresses with `--allow-from`; see [the remote connector](REMOTE-CONNECTOR.md) |

---

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
`review`

`contests` · `enter_contest` · `submit_entry`

Results are compact rather than pretty-printed, and most answer with a
projection — the fields an agent uses, not every field the daemon keeps.
Pass `verbose: true` for the whole record.

## Hooks

For Claude Code, `agentdocker setup claude-code` installs handlers for
`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`, `StopFailure`
and `SessionEnd`. They are what let the daemon see an agent's session
begin and end, what it is about to edit, what it changed, and what it
should be told before it starts — the journal since it last looked, and
anything it read that has gone stale. `PreToolUse` takes a lease on the
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
agentdocker claim path:src/api --as codex-27221 --note "rewriting the router"
```

The second agent to want that path is told who holds it and waits, rather
than editing underneath the first. `agentdocker waiting` shows the queue;
`agentdocker leases` shows who holds what. When they turn out to be
changing the same files anyway, a channel opens by itself and both are in
it — `agentdocker channels`.

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
agentdocker checkpoint --as codex-27221 --task "router rewrite" \
  --next-steps "finish the error mapping"
agentdocker handoff --as codex-27221 --to claude-code-90419
agentdocker resume --as claude-code-90419        # inspect, then --accept
```

The receiving agent gets the checkpoint, the leases, what was read, what
changed since, the diff, unread messages and the journal — not just a
sentence about where things stand.

### Commit an agent's work so the journal knows whose it was

```sh
agentdocker commit --as codex-27221 --all -m "rewrite the router"
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
agent typed it is our record to keep, not a change to your history.

Add `--push` to push afterwards. A push that fails leaves the commit —
losing the work to tidy up a failed push would be the wrong trade.

### Read a long log without paying for all of it

```sh
agentdocker logs codex-27221 --compress
agentdocker validation a1b2c3 --as codex-27221 --compress
```

Where [rtk](https://github.com/rtk-ai/rtk) is installed, this pipes a
copy of the retained log through it and shows what came back. Where it
is not — or where it fails — you get the whole log and a line saying
why.

The log on disk is never rewritten. A validation log is evidence:
`integrate` refuses source that has not passed, and the log is how
somebody checks that claim later. Evidence is kept whole.

### Planned daemon replacement

`agentdocker daemon reload` requires `AGENTDOCKER_EXPERIMENTAL_RELOAD=1` on the
running daemon; without that gate it returns `unavailable` and leaves the daemon
and agents running. The gated implementation transfers coordinator ownership to
a checked successor while session owners retain managed processes and I/O.
Broader actual-provider, attached-draft and uncertain-delivery acceptance remains
open, so the gate stays experimental. See the
[remaining acceptance work](REMAINING-WORK.md). Ordinary daemon stop/start is a
separate operation: stopping terminates managed agents.

---

## What changed

Newest first. Only what changes how the product is used.

### Unreleased

- **Reconnect here** in a Claude Code session's Details, and `agentdocker
  reconnect <session>`: once the session has exited in its terminal, the daemon
  brings it back under its own record with its conversation (`--resume`) and
  the AgentDocker channel, so it takes messages live and what was queued for
  it stays its own; the app opens its pane, where Claude's consent prompt
  appears. Refused with the reason while the process still runs, in another
  checkout or with somebody attached. The copyable command stays as the
  alternative.
- Roles: `agentdocker role reviewer --as <agent>` gives an agent a role,
  and `role:reviewer` names it as the recipient of a `send`, an `ask` or
  a `handoff` — the one live agent holding that role in the sender's
  project; none is not found, two are ambiguous. `--clear` takes it away.
- Reply from the notification: on macOS a message notification has a
  **Reply** field, and what is typed there reaches the conversation — an
  answer to a question closes it — without opening the window.
- A turn's end no longer takes an agent's deliberate leases away: the
  Claude Code `Stop` hook releases only the per-file edit leases it took
  itself (`automatic`), so a worktree, branch or build-campaign lease
  claimed through `claim` or the MCP tools holds until released or expired.
  `SessionEnd` still gives everything back.
- Commits in a private checkout are attributed: a worktree made with
  `agentdocker worktree-create` (now with `--from <ref>`) is remembered as its
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
- The remote connector: `agentdocker connector serve --public-url <https://…>`
  serves an OAuth-protected MCP endpoint on loopback for a tunnel you run, so
  Claude's or ChatGPT's browser side panel can join a project as a browser
  agent — registered when its code is redeemed, after the pairing code from
  the terminal is typed on the consent page — with the messaging tools only.
  `connector grants` lists consents, `connector revoke <agent>` ends one.
  `--tunnel tailscale` exposes it through Tailscale Funnel on this machine's
  own stable name and `--tunnel cloudflared` starts a quick tunnel;
  `connector install` runs it as a login service, `connector status` shows
  its address and pairing code, and `--allow-from` admits only the vendors'
  published addresses. One connector serves every project on the machine:
  the consent page chooses the project a browser agent joins, and its
  `project` broadcast names that project, wherever the connector runs. A
  vendor may identify itself by its Client ID Metadata Document instead of
  registering (fetched only from the vendors' hosts); the desktop's Tools
  screen shows whether the connector is serving, its URL and pairing code.
- Every MCP tool carries annotations (read-only, destructive, idempotent,
  open-world), so a host that asks before risky calls lets the reads through.
- `claude attach` (later shown as `claude agents`) is the terminal in front of
  a background Claude Code session, whose `bg-spare` process registers itself:
  neither is discovered or adoptable as a second agent.
- `agentdocker setup --shell` makes every terminal `claude` carry the channel
  flag so AgentDocker can wake it; the Claude Code card in Tools offers it as
  **Wake terminal sessions**, and the MCP server now reads the flag from its
  parent `claude`, so `AGENTDOCKER_CLAUDE_CHANNEL_INPUT` is no longer required
  from a terminal.
- Agents that work inside a browser are inventoried: `runtimes` lists each
  vendor's extension per browser profile (`claude-browser`, `chatgpt-browser`)
  and says that its sessions run in the browser and cannot be listed or
  messaged; a runtime's helper such as `claude --chrome-native-host` is never
  discovered and cannot be adopted; `rm` on a live external record says
  `deregister`.
- The Board's five columns share the width, and an open card's title,
  acceptance text and moves sit beneath them where there is room to read
  them (under the card when the window is narrow). A clicked control no
  longer keeps a focus ring: the ring is the keyboard's.
- `agentdocker runtimes` heads its last column UNREGISTERED: it counts a
  tool's processes nobody registered, which `ps` never showed as sessions.
- A project pause: `agentdocker pause "reason"` tells every agent in the
  project to hold — the reason reaches each live one as a `pause` message
  and their new leases are refused with it until `pause --lift`; `pause
  --list` shows what is paused. The app has **Pause…**/**Resume** on the
  project header. Schema 23.
- Notifications open their message: a click on a notification for an
  archived message opens the conversation on the Messages screen and
  scrolls to the row, paging back a bounded number of pages for an older
  one, even when that conversation is already open.
- A provider session that comes back as a new process (a resumed Claude
  or Codex) is folded into the record it had, keeping its queue and name.
- Setup writes the channel-capable Claude MCP entry (`--claude-channel`)
  and health checks recognise it; a plain entry stays valid MCP.
- The Messages workspace: **+** for a new direct message or channel,
  invitations, `@` mentions with counts, Enter to send, resizable panes,
  an Earlier group for ended sessions' conversations.
- Opt-in local token collection, hourly accounting and the `usage` command and
  screen are implemented in candidate #194, with explicit unknown/partial
  coverage. Final integration and sustained acceptance remain tracked in
  [Remaining work](REMAINING-WORK.md).
- Conversations: every message is archived in the one conversation its
  destination names (`everyone:<project>`, `all`, `channel:<id>`,
  `dm:<a>:<b>`, `notices:<agent>`), beside the queue it is delivered to and
  never instead of it, bounded by a per-conversation cap and `[messages]
  retention`. `conversations` lists them with unread counts per reader,
  `history` reads one back with reply counts, `thread` a root and its
  replies, `search` finds text; reading moves a cursor forward, never back.
  Channels get a `#name`; a person is no longer put in a collision room.
- The desktop app ships as `AgentDocker.app` on macOS, with its own icon,
  so the Dock and the app switcher name it properly. `agentdocker ui`
  first launches a matching sibling `agentdocker-ui`, then falls back to the
  bundle when no sibling exists. Local preview bundles use ad-hoc signing. Public macOS
  distribution still requires Developer ID signing and notarization; the
  current public release predates this desktop work.
- A new mark: three agents, in the app's own project colours, meeting at
  one host. Drawn on Apple's icon grid, and simplified below 24pt where
  the connectors would otherwise be a smudge.
- An event kind a client has never heard of is ignored instead of taking
  the event stream down with it. Upgrading the daemon under a window
  that is already open used to show it as disconnected.
- The window fits what is in it: the initial size is clamped to the
  monitor, the terminal grid is measured from the font you chose rather
  than a hard-coded 13pt cell, and a table wider than the window scrolls
  sideways instead of being cut off.
- The console looks and behaves like a terminal: dark ground, monospace,
  a prompt on the floor, the up arrow for history, and a transcript that
  accumulates.
- The Events screen is gone. `agentdocker events` is the place for the
  raw stream.
- `daemon reload` refuses replacement until live process and I/O continuity
  and successor readiness have passed actual daemon acceptance tests.
- `agentdocker commit` — an agent commits its checkout through the
  daemon, so the journal entry names it and carries its message.
- `logs --compress` and `validation <id> --compress` — an rtk view of a
  retained log, where rtk is installed. The log itself is untouched.
- The watcher covers every checkout of a project, not only the one an
  agent registered in. Commits in a worktree nobody registered now reach
  the journal and the ledger, and `overlap` compares real checkouts.
- Agents are grouped and coloured by project throughout the app.
- A Settings screen: terminal palette, text sizes, and row density,
  applied to the console and the agent terminal alike and remembered
  between runs.
- The window reads like a desktop control panel: a sidebar on its own
  ground, one accent for the selected place, and rows that breathe.

### v0.1.0

The first release: registry, supervision, messaging, leases, the project
watcher, the change journal, worktrees and validated integration,
handoffs, containers, policy and quotas, restart policies, channels,
contests, PTY sessions, the desktop app, and the descriptor handoff.

In Messages, **+** starts a direct message or named channel. Press Enter to send;
mention suggestions only include the current conversation's recipients. In an
open channel you belong to, **Add members** adds another available agent. The
CLI equivalent is `agentdocker channel invite --as <member> <channel> <agent>`.
These Messages additions merged in PR #170 and have been installed since the
September 17 `d14610b7` preview (the current installation is `652cf6a3`). Native workflows and a targeted
synthetic Enter event passed; physical keyboard and IME acceptance remain in
[Remaining work](REMAINING-WORK.md).
