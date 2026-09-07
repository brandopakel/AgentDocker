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

That puts `agentdocker`, `agentd` and the desktop app under `~/.local/bin`,
and on macOS installs `AgentDocker.app` as well. From a checkout:

```sh
cargo install --path crates/cli --locked   # agentdocker + agentd
cargo install --path crates/ui  --locked   # the desktop app
```

You do not start the daemon. The first client that needs it starts it, on
`~/.agentdocker/agentd.sock`. To have it survive a reboot:

```sh
agentdocker daemon install    # a launchd or systemd user service
agentdocker daemon status     # what is running, and where
```

Then bring in the agents already on the machine:

```sh
agentdocker runtimes        # what is installed, and whether we are wired into it
agentdocker setup           # register the MCP server and install hooks
agentdocker discover        # agent processes nobody registered
agentdocker adopt --all     # register all of them
agentdocker me              # register yourself, so agents can ask you things
agentdocker ui              # open the app
```

`setup` is the step that matters most, and the one that is easy to skip.
An agent that has not been wired up is still *seen* — it appears in `ps`
and in the app — but it cannot tell the daemon what it is holding or
reading, so it reports nothing it is doing. See
[Why an agent reads "idle"](#why-an-agent-reads-idle).

Everything respects `AGENTDOCKER_HOME`, so a throwaway daemon for
experiments costs nothing:

```sh
AGENTDOCKER_HOME=/tmp/ad-scratch agentdocker ps
```

---

## The desktop app

A native window over the same Unix socket as the CLI. No HTTP, no browser,
no localhost. `agentdocker ui` opens it, and on macOS it opens
`AgentDocker.app` if that is installed so the Dock and the app switcher
name it properly.

Agents are grouped by project, and every project keeps one colour
everywhere it appears — the heading, the dot on each of its rows, the
lease list. The colour comes from the project's id, so it is the same
colour in every session and on every machine. The **All projects** menu in
the title bar narrows every screen to one project at a time.

### Agents

Who is running, grouped under their project, with a summary line per
project — `2 agents · 1 working`, or `all idle`, or `1 blocked`.

Per agent: its name, runtime, what it is doing, the branch and commit it
is on, how many leases it holds, when it was last seen, and buttons to
stop it or attach to its terminal.

Below the live agents, **Running, not registered** lists agent processes
the daemon found that nobody registered — press **Adopt** to bring one in.

#### What is `user`, with runtime `human`?

You. `agentdocker me` registers the person at the keyboard as an agent
named `user`, and the app does it for you when it starts. Being an agent
is what lets the others address you: they can send you messages, queue
them while you are away, put questions to you that block until you answer,
and see that you hold a lease on a file so they leave it alone. It shows
as **you** in the runtime column.

#### Why an agent reads "idle"

**DOING** is derived from what the daemon actually knows: the leases an
agent holds and the working set it has reported. It is never guessed from
terminal output, because output is not evidence — an agent printing a
paragraph may be doing nothing, and an agent printing nothing may be
halfway through a refactor.

So an agent that has not been wired up has nothing to derive from, and
reads `idle` however busy it is. Hover the cell: if that is why, it says
so. Fix it on **Runtimes**, or with `agentdocker setup`.

### Questions

Questions agents have put to you. Each one has an agent blocked on the
answer, and each gives up when its time runs out — this is the one screen
where doing nothing has a cost. Type the reply under the question and send
it. Same thing as `agentdocker questions` and `agentdocker answer`.

### Terminal

The terminal of a managed agent, over `attach`. A real vt100 screen:
colours, cursor, resize, scrollback, and every keystroke goes to the
agent. **Detach** leaves it running.

Only agents started with a terminal have one — `agentdocker run --tty`, or
`run` for a runtime that needs one. An adopted process keeps the terminal
it was started in; that one belongs to whatever launched it.

### Console

See [The console](#the-console).

### Runtimes

Every agent tool AgentDocker knows about, whether it is installed here,
its version, and whether we are wired into it:

- **MCP** — the runtime can call our tools.
- **HOOKS** — the runtime tells us about its sessions and edits.
- **RUNNING** — processes of that runtime with no registered agent.

**Set up** wires one in, the same as `agentdocker setup <runtime>`.

### Journal

The project's narrative: releases, notes, commits, arrivals, departures
and handoffs, oldest at the top, newest at the bottom, following itself
as entries arrive. Pick the project from the menu. Same thing as
`agentdocker journal`.

**What is in it, and what is not.** One line per *event worth
remembering*: a commit, a branch switch, an agent arriving or leaving, a
note somebody wrote, a release, a handoff, a review. It is not a log of
edits — those are in the ledger (`agentdocker changes`, `blame`), which
records every file the watcher saw change. An hour of editing produces
ledger rows and no journal entries until something is committed.

**How it stays current.** The daemon writes each entry to SQLite as it
happens and publishes it on the event stream. The app appends live from
that stream, and re-reads the newest 200 entries whenever it opens,
reconnects, or you switch projects — so closing the app loses nothing.
The daemon holds the journal, not the app; it is still being written
while no window is open.

**Which directories it watches.** Every checkout of the project, not
only the one an agent registered in: the main checkout plus each linked
worktree, up to 32 of them per project. This matters more than it
sounds. A repository's refs are shared, so a commit in a worktree writes
into the main checkout's `.git`, and a daemon watching one directory
sees the write without ever looking at the checkout it came from. That
is how a fleet can commit twenty-seven times and have four of them
recorded — and how `overlap` can answer "nothing collides" while looking
at a single checkout.

A checkout the daemon has just learned about has its position recorded
silently the first time: its history did not happen while anything was
watching, and announcing it would be inventing a timeline.

### Leases

Every lease held right now, across projects: the resource, who holds it,
exclusive or shared, when it expires, and the note the holder left.

### Settings

What the window looks like, kept in `~/.agentdocker/ui.json` so it
survives a restart, and per `AGENTDOCKER_HOME` so a throwaway daemon gets
its own.

- **Palette** — the terminal palette, used by both the console and the
  agent terminal. A light palette turns the whole window light, because
  a light terminal inside a dark window is two products in one frame.
- **Terminal size** / **Text size** — point sizes for the monospace
  surfaces and for everything else.
- **Roomy rows** — more space per row, for a window being watched across
  the desk rather than read up close.

The palette list is the profiles people recognise — Basic, Pro,
Homebrew, Solarized, Novel — reproduced by name rather than read off the
machine. **Matching your terminal automatically is not something this
does, on purpose.** Terminal.app keeps its profiles in a binary plist of
`NSKeyedArchiver` colour blobs; iTerm2, Ghostty, WezTerm and Alacritty
each keep theirs somewhere else in some other format; and the app is
normally started from the Dock, so there is no terminal to inherit from.
A palette that is *nearly* right looks broken, so the choice is yours.

---

## The console

Any `agentdocker` command, run from the window, with its output rendered
where you typed it.

It is deliberately **not a shell**. It runs `agentdocker` subcommands and
nothing else. There is a terminal on this machine already and being a
second one is somebody else's job — what the console borrows is the feel:
the same monospace on the same dark ground, a prompt on the floor of the
panel, the up arrow for what you typed before, and a transcript that
accumulates.

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

### Look at the fleet

| Command | What it does |
|---|---|
| `ps` | Agents, live ones by default, grouped by project |
| `top` | The fleet live, redrawing as the daemon reports changes |
| `activity` | What each agent is doing: working, idle, or blocked on a named resource |
| `inspect <agent>` | Everything known about one agent, as JSON |
| `logs <agent>` | An agent's captured output; `-f` to follow, `--compress` for an rtk view |
| `validation <id>` | The retained log of one validation; `--compress` for an rtk view |
| `events` | The daemon's event stream |
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
| `up` / `down` | Start or stop the agents in an `Agentfile.toml` |
| `heartbeat` | Report that an agent is alive |

### Talk

| Command | What it does |
|---|---|
| `send` | Message an agent, the project, a topic, or everyone |
| `watch` | Stream messages for an agent or matching topics |
| `inbox` | Messages queued while an agent was not watching |
| `ask` | Ask an agent — or the human — and wait for the answer |
| `answer` | Answer a question somebody is waiting on |
| `questions` | Questions waiting for an answer |
| `me` | Register yourself as an agent named `user` |

### Share a resource

| Command | What it does |
|---|---|
| `claim` | Claim a lease on `path:`, `branch:`, `task:`, or anything |
| `renew` / `release` | Extend or give up a lease you hold |
| `leases` | Every lease held right now |
| `waiting` | Claims waiting for a resource, oldest first |

### Know what changed

| Command | What it does |
|---|---|
| `journal` | What changed and why, one line per entry; `add` appends a note |
| `changes` | The ledger: file changes seen in a project, with who held each file |
| `blame <path>` | Who changed a file, oldest first |
| `overlap` | Paths changed in more than one checkout: what will collide |
| `observe` / `reads` / `stale` | Record what was read, and check it is still true |

### Work in isolation, then merge

| Command | What it does |
|---|---|
| `worktree-create` | A new linked checkout and branch, without touching existing files |
| `worktree-diff` | Tracked changes in an agent's checkout |
| `commit` | Commit the agent's checkout, journaled and attributed to it |
| `validate` | Run a check and retain its command, log and content fingerprints |
| `validations` | Retained validation evidence |
| `integrate` | Preview or prepare an uncommitted merge of validated source |

### Hand work over

| Command | What it does |
|---|---|
| `checkpoint` / `checkpoints` | Persist task context and content identity |
| `handoff` / `handoffs` | Hand an agent's work to another, with everything around it |
| `resume` | Inspect or accept a verified handoff |
| `export` / `import` | Carry a bundle to another host |

### Agree with each other

| Command | What it does |
|---|---|
| `channels` / `channel` | The rooms agents share when they are on the same work |
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
| `runtimes` | Agent tools installed here, and whether we are wired in |
| `setup` | Wire us in: MCP registration, and hooks for Claude Code |
| `ui` | Open the desktop app |
| `attach <agent>` | Connect this terminal to an agent's; Ctrl-] detaches |
| `daemon` | Install, start, stop, reload or inspect `agentd` |
| `hook` | Handle a hook event, or install the hook configuration |
| `mcp` | Serve our tools to an MCP host over stdio |

---

## MCP tools

`agentdocker setup` registers `agentdocker mcp` with every runtime that
takes an MCP server. An agent then has these without knowing anything
about us:

`whoami` · `ping` · `list_agents` · `inspect_agent` · `activity`

`send_message` · `read_inbox` · `wait_for_messages` · `ask_human` ·
`open_questions` · `answer_question`

`claim` · `renew` · `release` · `list_leases`

`read_journal` · `journal_note` · `observe_paths` · `check_stale` ·
`overlap`

`create_worktree` · `worktree_diff` · `commit` · `integrate_worktree` ·
`validate`

`save_checkpoint` · `list_checkpoints` · `resume_checkpoint` · `handoff` ·
`list_handoffs`

`open_channel` · `list_channels` · `close_channel` · `request_review`

`contests` · `enter_contest` · `submit_entry`

Results are compact rather than pretty-printed, and most answer with a
projection — the fields an agent uses, not every field the daemon keeps.
Pass `verbose: true` for the whole record.

## Hooks

For Claude Code, `agentdocker setup claude-code` installs handlers for
`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`
and `SessionEnd`. They are what let the daemon see an agent's session
begin and end, what it is about to edit, what it changed, and what it
should be told before it starts — the journal since it last looked, and
anything it read that has gone stale.

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

### Replace the daemon without disturbing anything

```sh
agentdocker daemon reload
```

The running daemon starts its replacement, hands over the agents'
terminals over a private socket, and leaves without stopping a single
agent. Use it after an upgrade.

---

## What changed

Newest first. Only what changes how the product is used.

### Unreleased

- The desktop app ships as `AgentDocker.app` on macOS, with its own icon,
  so the Dock and the app switcher name it properly. `agentdocker ui`
  prefers the bundle.
- The console looks and behaves like a terminal: dark ground, monospace,
  a prompt on the floor, the up arrow for history, and a transcript that
  accumulates.
- The Events screen is gone. `agentdocker events` is the place for the
  raw stream.
- `daemon reload` reports a replacement that cannot start, instead of
  waiting for it forever.
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
