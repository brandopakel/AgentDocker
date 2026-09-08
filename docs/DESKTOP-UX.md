# The desktop app, screen by screen

What the window shows, what every control does, and what it does *not*
claim. Written from a pass over each screen and each control against a
running daemon, and updated as the pass found things and they were
fixed. Where a control is missing, disabled, or says something unusual,
the reason is here rather than in somebody's head.

The guiding rule, and the one most of the fixes came back to:

> The window says what it knows. When it does not know, it says that
> instead of guessing, and the guess it used to make is named here so
> nobody reintroduces it.

## The frame

**Title bar** — the mark, the product name, the connection state, and,
when more than one project has agents, a project filter that applies to
every screen. The connection dot is green when the daemon answered and
red when it did not, with the socket path beside it. A disconnected
window keeps painting the last thing the daemon said; hovering the
indicator says so, because a frozen table with no explanation reads as a
hung app.

**Status line** — the last thing that happened, and only for twenty
seconds. It expires on purpose: nothing lives only there (a failed setup
keeps its plan, a lost socket has its own indicator), and a line that
never clears is a line the eye stops reading. An error from ten minutes
ago reported as news is worse than no line.

**Sidebar** — the eight screens, with a live count beside Agents,
Questions and Leases. It is painted rather than built from widgets, so
it draws its own focus ring: Tab reaches the rows and Enter chooses one,
and until that ring existed a keyboard user had no idea which row they
were on. A test tabs to the third row and presses return.

**Colour** — three, and they mean one thing each. Green: connected,
wired, working. Amber: needs a look — unverified, blocked, not heard
from. Red: absent or refused. They live in `theme.rs` and nothing else
uses them, so a colour in this window always means a connection state.

## Agents

Live agents grouped by project, each project row carrying its own colour
and a summary of what its agents are doing.

| Column | What it means |
|---|---|
| NAME | The name the agent registered under. One process is one agent; see below. |
| RUNTIME | `you` for the person at the keyboard. |
| DOING | working, blocked on a named resource, idle — or **not heard from**. |
| BRANCH | Branch and short HEAD of the agent's checkout. |
| LEASES | How many leases it holds right now. |
| SEEN | When the daemon last heard from it. |

**"not heard from" is the important one.** "Idle" is a claim about the
agent; "we have not heard from it" is a claim about us, and the window
used to say the first when it only knew the second. Two cases produce
it, and the hover says which:

- **The runtime has no hooks adapter.** Codex is the one people actually
  run. A hooks adapter reports every turn whether the agent asks it to
  or not; without one, AgentDocker hears from a session only when it
  calls an AgentDocker tool. A Codex session editing files right now
  reported `idle`, and that is what made the whole table untrustworthy.
- **The last report is older than fifteen minutes.** Hooks are loaded
  when a session starts, so a session older than the setup that
  installed them cannot report until it is restarted, however alive its
  process is.

The project summary counts the same way: it will say "2 not heard from"
rather than "all idle".

**Stop** asks once. It is the only control here a person cannot take
back, and it sits in a dense row beside Attach. The first click arms it
and the button becomes `Stop?`; a second click within five seconds
stops the agent, and after that it disarms itself, so a click forgotten
and returned to cannot complete. **Attach** opens the agent's terminal
and is offered only for an agent that has one.

**Running, not registered** lists agent processes the daemon can see but
does not manage. **Adopt** registers one without restarting it; **Adopt
all** does the lot. Both say so on hover, because "adopt" does not
obviously mean "leave it running".

## Questions

Questions other agents have put to the person at the keyboard, each with
who asked, which project, how long is left, and the message id. Type an
answer and press Enter or click **Answer**.

Answer is disabled while the draft is empty. It used to be live and drop
the click, which to the person clicking is a button that does not work.
The draft and the question both stay until the daemon confirms it took
the answer, so nothing typed is lost to a daemon that was not listening.

## Terminal

The attached agent's terminal, or the list of agents that have one. It
draws its own scroll region and takes every keystroke, so it sits
outside the shared scroll area and no other screen competes for keys.
The grid is measured from the actual laid-out glyph width rather than
assumed, because the font size is the reader's to choose and a grid
computed from a constant lays the agent out past the edge of the window.

**Jump to live** returns to the bottom of the scrollback and appears
only when scrolled back. **Detach** stops watching; it does not stop the
agent, and says so.

## Console

Any `agentdocker` command, run from the window and rendered as it came
back. Not a shell and not a second system terminal — the machine has one
of those and being another is somebody else's job. What it takes from a
terminal is the feel: the same monospace on the same ground, a prompt on
the floor of a growing transcript, the last commands on the up arrow,
and output that accumulates rather than a box that is replaced.

Commands are bounded at twenty seconds, because `watch`, `events` and
`logs -f` never finish on their own. They run on a lane of their own, so
one running to its whole limit no longer stops the agent list, the
leases and the questions behind it — and because nothing else on screen
moves while one runs, the prompt says how many are still going.

## Runtimes

The agent tools on this machine, what was found of each, and whether
AgentDocker is wired into its channels.

Each connection cell carries a colour and says on hover what the state
means and what would change it — this table printing a bare `no` is how
a reader ends up asking what it means and how to clear it.

| State | Meaning |
|---|---|
| yes | Registered. Not proof a session has used it; start a fresh one. |
| no | Nothing registers AgentDocker here yet. Review setup will plan it. |
| unverified | Something under our name is disabled, malformed, or runs a different command. Setup will not overwrite it — open the file and decide. |
| - | No such channel, or no adapter for it yet. Nothing to do. |

**Review setup** appears on a row that needs one and plans the change
without writing anything.

**Check connections** reads every runtime's configuration and reports
it, changing nothing. **Saved setup plans** lists every plan previewed
on this machine, newest first, coloured by phase; each keeps what the
files said before it, which is what lets it be undone.

A plan lists each change as runtime · channel · path with the action
beneath it, then its notes, then **Apply changes**, **Undo this setup**
and **Close**. A plan with nothing in it says so rather than showing a
greyed Apply, and every disabled button says why it is disabled — the
thing this screen was rebuilt to stop doing.

One action reads differently from the rest: **Claude Code's MCP
registration is made by `claude mcp add`, not by us.** Its MCP servers
live in `~/.claude.json`, which is also its live application state and
which it rewrites throughout a session; a byte-for-byte plan against it
would fail preflight nearly always and take the hooks change down with
it. So the plan carries the command rather than the bytes. It is still
previewed, still listed, and still undone by the matching remove — and
the plan takes back only a registration it made itself, never one that
appeared between the preview and the apply.

## Journal

What changed in the selected project and why, one line per entry, newest
at the bottom and following as it grows.

Entries attributed to **`external`** are commits the watcher saw HEAD
move for with nobody attached. An agent that commits through the
`commit` tool is named instead, with the message it wrote — worth
knowing, because the journal's value is *who* changed something, and
until the tool was named in the MCP instructions no agent ever reached
for it.

## Leases

Every lease held right now: project, resource, holder, mode, expiry and
the note the holder left. The note is the point — it is what a blocked
agent reads to find out what it is waiting for.

## Settings

Palette, terminal text size, window text size, and roomy rows, applied
live and kept per AgentDocker home in `ui.json`. The palette preview
sits on the palette's own ground, because a swatch on the window's
ground says nothing about what it will look like. **Reset** returns to
what the window ships with.

The palettes are reproduced from published colours by name rather than
read off the machine. Terminal.app keeps profiles in a binary plist of
archived colour blobs and every other emulator keeps theirs somewhere
else, the app is usually started from the Dock so there is no terminal
to inherit from, and a palette that is nearly right looks broken.

## Installation

Install an extracted desktop package, or return to the previous retained
version. Activation takes effect at the next launch; the running daemon
and agents continue until explicitly restarted.

**Show installed versions** reads the prefix and changes nothing.
**Preview installation** checks a package and reports what installing it
would do. **Preview rollback** is offered only when the last status
found a version to go back to — a live button whose only outcome is "no
active desktop installation" is an error the screen invited. **Apply
this installation** activates exactly the release reviewed above, and is
disabled when the preview does not name it, because an install that is
not pinned to what was reviewed is not the install that was reviewed.

## What the window will not do

- **Assert what it has not been told.** See "not heard from".
- **Block on its own work.** The console, guided setup and installation
  each shell out somewhere other than the thread that talks to the
  daemon. A window that keeps painting stale numbers while it waits is
  worse than one that says it is waiting.
- **Offer a button that does nothing.** Every control is either enabled
  and effective, or disabled and explains itself.
- **Speak over the CLI.** Every button runs a command that a person can
  run themselves, and the Console is there so they can.

## Known gaps

- **Notifications carry the wrong icon.** A notification wears the icon
  of the bundle that posted it and every way of overriding that is
  closed, so AgentDocker posts its own from `AgentDocker.app`. macOS
  refuses to register a notification client without a stable signing
  identity, so an ad-hoc build falls back to `osascript`, which belongs
  to Script Editor. It resolves with a Developer ID and nothing else
  changes. See `DISTRIBUTION-SETUP.md`.
- **Which of a session's two names survives is a race.** One process is
  one agent now, but whichever half registers first owns the name, and
  that is not settled.
- **The graphical acceptance run needs an unoccluded window.** Not a
  window bug — the renderer skips the paint for an occluded surface and
  the screenshot is taken during the paint. See
  `TESTING-AND-BENCHMARKS.md`.

## Testing it

Every screen is rendered headlessly with content on it, asserting each
frame actually drew — that is what catches a layout mistake on the
screen nobody happened to have open. The installation panel is rendered
in each of its states including both failures. Beyond that: the lanes
keep their order and never wait on each other, the status line expires,
the sidebar is reachable and choosable from the keyboard, an agent
nobody has heard from is not reported as idle, and asking for
notifications outside a bundle is refused rather than fatal.

```sh
cargo test -p agentdocker-ui
```
