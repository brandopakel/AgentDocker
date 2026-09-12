# Using the desktop

`agentdocker ui` opens the native Iced app. It connects to the local daemon and
restores the last project and appearance. No browser, container engine or cloud
account is required to organize local agent work.

## Projects

The app opens on **All projects**: every agent on this Mac in one list, each row
carrying its project's mark, with a **Needs you** strip above it. The strip holds
whoever is waiting on you, one action each: an unanswered question (**Answer**),
a session that finished while you were not looking (**Open**), an agent running
here that nobody connected (**Connect**), and an installed tool that is not yet
connected to AgentDocker (**Set up**). It shows at most six items and points to
Inbox for the rest; it disappears when nothing is waiting. Choosing a project on
the left narrows the list and the strip to that project; **Projects** in the rail
returns to all of them.

The sidebar remembers projects found through agents and folders you add yourself.
**Add project…** lets you browse or enter an existing folder. It pins the project
without starting an agent, changing integrations or creating files. Later sessions
in the same repository appear there automatically. Linked worktrees share a
project and retain their session checkout details.

Quiet projects remain available. An unavailable folder stays selected and offers
**Check folder again**. **More → Unpin project** keeps it in recent projects; **More → Forget
project** removes its workspace entry. Neither deletes files nor stops sessions.
A project with active agents can be discovered again. Sessions whose project is
unknown appear under **Other sessions**.

**Current** shows live sessions; **History** holds finished sessions, including
previous runs with the same name. **Needs input** shows this project's unanswered,
unexpired questions, including questions from a session that has since finished.
Search applies to the selected project and all three filters. Switching projects
returns to Current. Nothing is deleted when a row moves to History.

Session rows show the name, runtime, branch and observed activity. Sessions needing
input appear first. Select a row for terminal access, reply, or stop. On a narrow
window, the session replaces the list; **Back to sessions** returns to it. On a
wide window, it opens beside the list. **Details** reveals the session ID, process,
checkout, commit and last-seen time.
**Launch agent…** chooses an installed CLI and starts it at the project root shown
in the header. **Connect** under **Running here, not connected** adopts a discovered process for
coordination. Known Codex Node launchers with a native Codex child are omitted
from discovery. A discovery row overlapping a live registration is hidden only
when its PID and process birth time both match. Separate registrations are never
merged by display name.
Installation or configuration alone does not prove that an agent is working.

**Stop session…** changes to **Confirm stop** for five seconds. Confirm sends the
stop request. A managed live PTY offers **Open terminal**; **Detach** closes the
view while the process continues. Finished sessions remain available in History.

**Message** opens a small composer for the selected agent. **Send message** uses
the same inbox queue as messages from other agents. Its queued receipt confirms
local acceptance; it does not mean the agent has read or completed the request.
The enabled Claude channel can wake an idle session. Codex and other connections
still require their supported input integration. Drafts survive navigation and
failed sends while this window remains open. A late receipt keeps any new text
you have typed, and uncertain requests are never retried automatically.

Project tabs provide:

- **Activity:** the recent durable journal, updated from daemon events.
- **Channels** (under More, the Advanced door): project rooms, membership,
  reviews, resolution and messages still queued for you. This view does not drain your inbox or fabricate chat history.
  Each room retains its own draft across navigation and failed delivery.
- **More → Channels:** as above; the More label counts messages waiting for you.
- **More → Files in use:** current leases and their holders.
- **More → Command line:** the real bundled `agentdocker` CLI in the selected project folder.
  It keeps command history and output with a bounded execution deadline.

## Inbox and connections

Inbox presents questions addressed to you with a draft per question. Navigation,
a failed request and disconnection preserve drafts. A pending answer cannot be
sent twice. Delivery does not establish that the recipient consumed it. Direct
messages remain queued until a consumer explicitly takes them.

Tools (the rail entry keeps the `connections` id) starts with installed tools. **Details** reveals versions, executable
paths and MCP/hooks configuration. **Other supported tools** expands the inventory
of tools that are not installed. **Review
setup** prepares a specific plan; **Apply reviewed changes** and **Undo this setup**
use the existing checked CLI operations. Health and saved plans remain available.
A connection failure retains the last snapshot and clearly pauses daemon actions.

## Settings and installation

Settings controls light/dark appearance, text size, terminal palette and row
spacing. Preferences are stored privately in the AgentDocker state directory.
A malformed preference file is preserved and reported rather than silently reset.
**Daily update checks** is off by default. Enable it to check once per day while
the app is open; a known update appears in the footer. Download and installation
remain explicit actions.

**Manage installation and retained versions** previews installation, rollback,
cleanup and launcher removal. Apply is tied to the reviewed payload or cleanup
plan and refuses stale inputs. Running versions and agents remain protected by
the existing installer rules. Public macOS downloads require Developer ID signing
and notarization; locally signed builds are explicit previews.

## Keyboard and terminal

- Tab and Shift-Tab move between controls; Enter/Space activate a focused button.
- Command/Ctrl+1–4 switch Projects, Inbox, Tools and Settings.
- Escape closes session details or an add/launch form.
- F6 moves focus out of terminal input. Control+] detaches.
- Drag across terminal cells to select text. Command+C on macOS, or Ctrl+Shift+C
  elsewhere, copies the selection (the visible screen when no range is selected).
  Selected text stays stable while new output drains; copy or resumed input
  releases it. Command+V/Ctrl+Shift+V pastes using the agent's bracketed-paste mode.

Text inputs and the terminal support native input methods. Focus indicators and
status words complement color. Native accessibility adapters expose control
labels, values and actions. Platform screen-reader and input-method trials remain
part of release acceptance; see [the design and validation contracts](ICED-DESIGN.md).
