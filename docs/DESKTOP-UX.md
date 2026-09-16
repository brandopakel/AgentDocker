# Using the desktop

`agentdocker ui` opens the native Iced app. It connects to the local daemon and
restores the last project and appearance. No browser, container engine or cloud
account is required to organize local agent work.

## Projects

With no saved selection, the app opens on **All projects**, with sessions grouped
under their project names. A saved project or Other sessions view is restored.
**Projects** in the sidebar returns to All projects; choosing a project narrows
the list. Opening a session takes you to its project and selects that session.

**Needs you** shows unanswered questions (**Answer**) and paused message
delivery (**Review**). Finished sessions keep their **Done** badge on the row. Question previews use at
most 80 characters from the first line. Answer opens and reveals the exact
question without submitting or changing drafts; full approval details remain in
Inbox. The first three items are shown; **Show more** expands the same list and
**Show fewer** collapses it. When nobody needs an answer or delivery review, **To get started** can offer
**Connect** for discovered processes and **Set up** for installed tools. Tools
also keeps the full setup controls.

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
input appear first within each project. Select a row for terminal access, reply, or stop. On a narrow
window, the session replaces the list; **Back to sessions** returns to it. On a
wide window, it opens beside the list. **Details** reveals the session ID, process,
checkout, commit and last-seen time.
**Launch agent…** chooses an installed CLI and starts it at the project root shown
in the header. **Connect** under **Running here, not connected** adopts a discovered process for
coordination. Known Codex Node launchers with a native Codex child are omitted
from discovery. Claude Chrome native-host helpers are also omitted, for native
and interpreter entry points; enabling Chrome in a real session keeps the agent
visible. A discovery row overlapping a live registration is hidden only
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
- **More → Channels:** project rooms, membership, reviews and queued messages.
  Its label counts messages waiting for you. Each room retains its own draft
  across navigation and failed delivery; viewing does not drain your inbox.
- **More → Files in use:** current leases and their holders.
- **More → Command line:** the real bundled `agentdocker` CLI in the selected project folder.
  It keeps command history and output with a bounded execution deadline.

## Messages, Inbox and tools

Sessions are shown by name. Default app launches and adapter-generated names
carry the same generated-name marker. A name generated from a runtime and
an identifier (the record says so, or it is exactly that adapter's form for the
record's own pid or session) reads as the tool and the branch it is on,
`Codex · main`; a number is added only while two live sessions of one tool
share a branch, and an ended session is never numbered. A name somebody chose
is shown as chosen, whatever it looks like. An author the window has no record
of reads as an unknown session. The session id stays under Details. In a
narrow window Messages and Inbox show either the conversation list or one
conversation; choosing one, a notification, or the next question opens that
conversation, and **Conversations** returns to the list.

**Messages** is what the rail item (named Messages then, Inbox otherwise)
opens against a daemon that keeps conversations (schema 21 and later); an
older daemon still gets the inbox below. It is shaped like a chat workspace.
The sidebar lists **Channels** (`#everyone` for the selected project, `#all`,
and named channels), collision rooms behind **Collisions**, **Direct
messages** with a presence dot for a live session (a conversation between two
agents reads `A ↔ B`), and **AgentDocker → agent** notices per agent; a
search box filters by name. Ended sessions' conversations sit behind
**Earlier (n)**. Each row shows the latest line and its unread count; the rail
badge is the sum. The pane shows the newest 200 archived messages, newest
last, with **Show earlier messages** at the top until the first is on view,
day dividers and a **New** divider before the unread part; a question keeps
its card (Answer, Allow, Deny) in place; other kinds of message carry a small
kind pill; long ones fold behind **Show more**. The window keeps as much of
one conversation as the daemon does (5,000 messages), so paging back reaches
the earliest it has; when the daemon prunes, the window drops every archive
and the open thread, reads the open conversation again, and ignores replies
from before the prune; a thread whose root was pruned closes. Opening a conversation
marks it read, which acknowledges those rows for you and nothing an agent
still owns; in a narrow window only the conversation on view is read, never
the list or a thread shown instead of it. **Reply** (or *n replies*) under a message opens
its thread beside the conversation, or in place of it when narrow with
**‹ Conversation** to return; the thread is read whole. The thread has a
composer of its own with its own draft, and only it sends with `reply_to`;
the conversation's composer stays under the conversation and never becomes a
reply. It sends to the channel, to the project (`#everyone`), to every agent
(`#all`) or to that agent; it reads **This session has ended** for a direct
conversation whose agent is gone, says so for one between two agents, and has
nothing to send for notices. Drafts survive navigation, a failed request and
disconnection. A notification opens the message's conversation even after it
has been read.

Inbox reads like a messenger. The left column lists one conversation per agent
with its mark, the latest line and how many items wait; **Everyone** shows all
of them. The conversation on the right starts with that agent's open questions
as cards, because they carry Answer, Allow, Deny and review controls, followed
by its messages as bubbles, newest last; long messages fold after eight lines
behind **Show more**. Under an open conversation the composer sends to that
agent (**Send**) or to every agent in its project (**Send to everyone**); the
receipt or error shows under the box, and drafts survive navigation, a failed
request and disconnection. A pending answer cannot be sent twice. Delivery
does not establish that the recipient consumed it. **Clear** removes one
message from your queue and **Clear shown** the visible ones; direct messages
otherwise remain queued until a consumer explicitly takes them. The rail badge
counts open questions and waiting messages together. Notification navigation
opens the requested message, including an older one outside the recent window.

Tools shows **Input receiver active** only with a fresh report from a receiver
bound to a live session. **Connected · idle delivery not verified** requires
recent MCP or hook contact; activity and configuration alone cannot establish
it. Other states distinguish **Needs setup**, **Setup needs review**,
**Configured · waiting for contact**, unavailable integration and **Not installed**.
Peer input needs the opt-in adapters in [CODEX-INPUT.md](CODEX-INPUT.md) and
[CLAUDE-CHANNEL-INPUT.md](CLAUDE-CHANNEL-INPUT.md). Missing supported setup offers
**Set up**. **Details** holds versions, commands, per-channel configuration and
each live session's contact and delivery evidence, plus **Review setup**,
**Check connections** and **Setup history**; **Other supported tools** expands
the inventory. Delivery distinguishes an active receiver awaiting its first
receipt, verified delivery, paused delivery and stale evidence; with words
queued behind a current receiver that no receipt names it reads **Queued ·
awaiting provider receipt**, since an earlier receipt says nothing about them
(a receipted message still in the queue counts as delivered). The session
inspector shows the same readiness alongside the queue and latest receipt.
Applying a plan says **Setup saved**, with **Undo** available afterwards. Fresh
sessions load saved setup; only the provider can request its required approval.
The connection check is one line per installed tool, says **Configuration
checked** when configuration passes, and closes with **Close**. A connection
failure retains the last snapshot and clearly pauses daemon actions.

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
