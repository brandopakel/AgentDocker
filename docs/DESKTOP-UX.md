# Using the desktop

`agentdocker ui` opens the native Iced app. It connects to the local daemon and
restores the last project and appearance. No browser, container engine or cloud
account is required to organize local agent work.

After a send, **Queued · N sessions need attention** appears when recipients
have missing or stale input receivers, paused delivery, ended sessions or provider
limits. **Delivery details** names them and offers **Open session** and **Copy
instructions**. These are the facts when the message was queued, not a receipt.
The details stay with that conversation or thread even if you switch while
sending. Session **Details** and expanded tool connection details show current
reconnect guidance too. Nothing is restarted or resent by opening these controls.

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

A vendor's browser extension (Claude, ChatGPT) has its own Tools row, marked
**Installed in Chrome · works inside the browser, its sessions are not visible
here**, with the browser, profile and version of each copy under **Details** and
the bridge the browser launches for a command-line tool, when one is registered.
The row offers no **Set up** and never lists a session: what the extension is
doing runs in the browser and on the vendor's side, and nothing on this machine
speaks for it. The bridge is that tool's helper; **Connect** never offers it and
adopting it by pid is refused with what it is.

The sidebar remembers projects found through agents and folders you add yourself.
**Add project…** lets you browse or enter an existing folder. It pins the project
without starting an agent, changing integrations or creating files. Later sessions
in the same repository appear there automatically. Linked worktrees share a
project and retain their session checkout details.

Quiet projects remain available. An unavailable folder stays selected and offers
**Check folder again**. Each project row has its own **⋯** menu: **Rename…**
gives the entry a name of your own in the sidebar and All projects headings (an empty name goes back to the
folder's), **Pin**/**Unpin**, and **Remove from list**, which keeps the folder
off the list even when its sessions are discovered again, until you add it
again (the list of removed folders is bounded like the project list, at 512;
at the bound a removal is refused with a message and the project stays, so
nothing removed earlier comes back on its own). **More → Forget project** does the same for the selected project. None
of these deletes files or stops sessions. Two projects with one name show
their parent folder under it, on one line. A folder discovered because an
agent ran there leaves the list by itself once it no longer exists, and a
folder under the per-user temporary directory (where test fixtures come and
go) is never listed by discovery, only by a pin; a pinned folder stays
either way. Shared scratch roots such as `/tmp` and `/var/tmp` remain
discoverable, including when Linux reports one as its default temporary
directory. Sessions whose project is unknown appear under
**Other sessions**.

**Current** shows live sessions; **Needs input** shows this project's
unanswered, unexpired questions, including questions from a session that has
since finished. Ended sessions are not a tab: they sit in one collapsed
**Earlier (n)** group under the current ones, including previous runs with the
same name, and a search that finds one opens the group. When only an earlier
session matches, its result appears without a contradictory empty-state card.
Search applies to the
selected project, both filters and the Earlier group. Switching projects
returns to Current. Nothing is deleted when a row moves to Earlier.

Session rows show the name, runtime, branch and observed activity. Sessions needing
input appear first within each project. Select a row for terminal access, reply, or stop. On a narrow
window, the session replaces the list; **Back to sessions** returns to it. On a
wide window, it opens beside the list. **Details** reveals the session ID, process,
checkout, commit and last-seen time.
**Board**, between Sessions and Activity, is the project's work: five
columns — Backlog, Ready, In progress, Review, Done — of cards with a title
and what done means. **File a card** at the top takes a title and the
acceptance text and files it **as Ready** (for the next agent to pull) or
**in Backlog** (yours to think about). A card shows who holds it with a
presence dot, or *for the taking* in Ready; opening a card shows its
acceptance text, its typed links (a kind — path, pr, commit, url, task, message,
memory — and the target, shown as text; the app neither opens nor copies
them, that is the person's tools' work) and its
moves: one column back or forward, **Hand to** an agent running here (or
*nobody*), and **Archive**. An agent pulls a Ready
card with the `pull_task` tool and the board shows it in progress under that
agent at once; two agents never get one card. The pull is a `task:<id>`
lease: a card whose holder's lease has lapsed — expired, released, or the
agent gone — says *hold lapsed* beside the holder; nobody takes it by a
plain pull, only by naming that holder (`pull_task` with `take_over_from`)
or by your **Hand to**, which ends the old hold and gives a running agent
the card's lease in one step. A move back to Ready or Backlog, or to Done,
ends the hold. The board reads again on every board or lease event; when it
could not be read the last board stays and the status says why. A card's draft is the project's: text typed for one board waits
while another is on view, and filing it is answered by its own reply — a
move or hand of some other card never clears it, and a filing the app could
not queue says so under the form. Unfinished title and acceptance text also
survive closing and reopening the window, under their original project. Reopen
does not file a card; only a confirmed filing clears its saved text. Storage
pressure refuses new text without discarding an earlier draft. The board is read a page at a time (100
cards, Backlog to Done, within a byte budget); when it goes on, **Show
more** appends the next page where the board ends — every ask is numbered
and only its own reply moves the board, so a late or unsolicited page is
ignored; a refresh asks for as many cards as are on view and supersedes
every ask still on its way — a page or an earlier refresh — and Show more
reads *Loading…* and takes no click while any ask is out, so the board never
folds back whichever reply lands first; choosing another project forgets the old asks — and at five pages
the board says so and points to archiving or `agentdocker task list
--column`. Narrow, the columns stack.

**Pause…** beside it asks for a reason and tells every agent in the project
to hold: they read the reason as a `pause` message, the daemon refuses their
new leases until **Resume**, and the header shows **Paused · reason** while
it holds (what an agent already holds, it keeps; your own actions are not
held; only you can pause or resume, an agent asks with a message).
**Launch agent…** chooses an installed CLI and starts it at the project root shown
in the header. Claude and Codex launches default to **Idle messages: On**;
turning it off visibly warns that messages may wait. Claude still requires its
channel consent. Other tools disclose that automatic idle delivery is unavailable.
This launch choice does not connect or restart an existing session.
**Connect** under **Running here, not connected** adopts a discovered process for
coordination; the row names the tool and the folder it runs in, not a
process number. Known Codex Node launchers with a native Codex child are
omitted from discovery, as is Codex's `app-server` sidecar (an API helper a
receiver or reviewer speaks to, never a session) and anything a bound
receiver started. Claude Chrome native-host helpers are also omitted, for native
and interpreter entry points; enabling Chrome in a real session keeps the agent
visible. A discovery row overlapping a live registration is hidden only
when its PID and process birth time both match. Separate registrations are never
merged by display name.
Installation or configuration alone does not prove that an agent is working.

**Stop session…** changes to **Confirm stop** for five seconds. Confirm sends the
stop request. A managed live PTY offers **Open terminal**; **Detach** closes the
view while the process continues. Finished sessions remain available under Earlier.

**Message** opens a small composer for the selected agent. **Send message** uses
the same inbox queue as messages from other agents. Its queued receipt confirms
local acceptance; it does not mean the agent has read or completed the request.
Direct-message and thread composers show the recipient's current input readiness
with a **Connection** shortcut to Tools. The shortcut preserves both drafts.
MCP/hook contact alone cannot verify idle wake; disconnected views show unavailable
readiness. The enabled Claude channel can wake an idle session. Codex and other connections
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
The sidebar lists **Channels** (`#everyone` for the selected project, or
`#everyone · project` when every project is on view, `#all`, and named
channels; a room opened before names or a collision room gets a short name
from its task or paths), collision rooms behind **Collisions**, **Direct
messages** with a presence dot for a live session, conversations two agents
had with each other behind **Between agents** (read as `Codex ↔ Claude
Code`), and **AgentDocker → agent** notices per agent; a search box filters by
name. Ended sessions' conversations sit behind **Earlier (n)**. Every row is
one line each for the name and the latest line. Unread counts and the rail
badge cover what is yours to answer: channels, broadcasts and your own direct
messages, never what two agents said to each other, what AgentDocker told
them, or a collision room (AgentDocker opens those between two checkouts
and fills them with its own contested-path notices; you are not a member,
though the room's own row still shows what is unread in it); **Mark all
read** beside the count reads all of it at once. The
sidebar, the conversation and the thread are columns with a divider between
each that drags, like the rail's beside the workspace: a name the sidebar
clips gets its room by dragging, and the widths are kept in pixels and
remembered with the workspace preferences, so a wider window gives the
conversation the room. In a smaller window or with larger text, side columns
shrink to keep the conversation usable; when the columns cannot fit, Messages
shows one pane with a way back. Expanding restores saved widths (rail 180–440
points, sidebar 200–560, thread 240–640).
The pane's header is the name on one line and, under it, what the room is
about (the task or contested paths, a pair's branches, a broadcast's
members). The pane shows the newest 200 archived messages, newest
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
has been read, and even when the sender's record or the channel is gone: the
archive outlives both. A notification for a message has a **Reply** field
(macOS): what is typed there goes from you to where that message went — the
project's everyone, its channel, or back to the agent who wrote to you — as a
reply to it, so a typed answer closes the question it answers, without the
window opening. Only the daemon's `sent` counts as sent. A reply that did
not go is said as a notification (**Open the app to recover your reply**)
and comes back to the app: the conversation opens with your words in its
composer, after anything already drafted there, and the status line says why — **Reply not sent** when the
daemon refused it, **Reply may not have been sent** when the connection went
before an answer, in which case read the history before sending again.
When the draft cannot take the words (draft storage is full) they wait beside
the composer with **Copy** and **Dismiss**; when the conversation itself cannot
be opened (its message, project or channel is gone) they wait at the top of the
Messages list instead, with the same two controls, whatever is on view; the
window keeps up to eight such replies and says so when a further one cannot be
kept. A notification from
another workspace's daemon hands the words to that workspace's window when
it is running; otherwise the notification carries what fits and says the app
could not keep them. A reply to a question goes to whoever asked it, wherever
it was asked, since that is the reply that closes it. Nothing typed is sent
twice on its own; what can be lost is said each time: a reply cut to a
draft's 16,000 characters, a ninth kept reply, or one another workspace's
window could not take. **Enter sends** in every composer — a
conversation's, a thread's, the inbox reply, the session message and an
answer — the same action as the button beside it, and nothing while the
draft is empty or already sending. **+** beside the search starts a
conversation the way Slack's New message does: **Direct message** is one
pick from the agents running here (in the selected project when one is);
**Channel** is a name (kept to lowercase letters, digits and hyphens as it
is typed), what it is for, and who is in it — everyone here when nobody is
picked — and the person is in it as its opener; the room opens as soon as
the daemon has it. `@` in a composer offers who is here and a pick finishes
the name (`@codex-51242`, the record's own name, which is what a mention
reaches); a row whose unread rows name the person shows an **@n** pill beside
its count, and such a message carries **mentions you** in its header.
The selected project also scopes archived direct conversations. Project message search retains finished sessions' direct messages and AgentDocker notices after restart.

Conversation, thread, channel, session and unfinished answer text is saved locally for
its daemon and restored after a normal window close. Text that was in flight
returns as an editable draft and is never sent automatically; check the history
before retrying an uncertain submission. Newer edits survive older send replies.
A failed save keeps the window open with **Retry saving** and an explicit
**Close without saving** choice. An unreadable saved file is preserved. Storage
limits refuse new text visibly while keeping existing nonempty drafts. Answers
return under their original question ID without restoring approval/review or
sending state; a confirmed answer or fresh completed-question snapshot removes
the saved text. Command input and other forms remain window-local.

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
A message that carries typed links shows them under its text, each as its kind
and its target. On the Messages screen the message is a row of the archive: the row is marked
and scrolled to once its page is here, and if the conversation was already open
at its newest the pages before are read back for it, one at a time, up to five
(one in flight at a time, so refreshes of the newest page do not spend the
bound), before the person is told how many messages were searched and that
**Show earlier messages** reads further back. When the archive's start is
reached first the message is said to be no longer in the archive, and a
conversation already holding all the window keeps (5,000 messages) is not
paged further and is told that limit instead. Navigating away — another
screen, project, session or conversation, or another notification — or
losing the daemon ends the search, and a page that arrives late moves
nothing.

Tools shows **Input receiver active** only with a fresh report from a receiver
bound to a live session. **Connected · messages wait for its next prompt**
requires recent MCP or hook contact and no input route at all — the session
is in touch, but nothing reaches it while it is idle — and activity and
configuration alone cannot establish it; a session whose receiver is paused
reads **Connected · input receiver paused** and one whose receiver has stopped
reporting **Connected · input receiver silent**, since a prompt does not
release what such a route holds. Other states distinguish **Needs setup · missing …** (which says
what: the MCP entry, the hooks, or the one or two hook events a release began
to require, so a machine wired before that release reads as missing
*StopFailure hook*, not as never set up), **Setup needs review**, **Configured ·
waiting for contact**, unavailable integration and **Not installed**.
Peer input needs the opt-in adapters in [CODEX-INPUT.md](CODEX-INPUT.md) and
[CLAUDE-CHANNEL-INPUT.md](CLAUDE-CHANNEL-INPUT.md). Missing supported setup offers
**Set up**. **Details** holds versions, commands, per-channel configuration and
each live session's contact and delivery evidence, plus **Review setup**,
**Check connections** and **Setup history**; **Other supported tools** expands
the inventory. Delivery distinguishes an active receiver awaiting its first
receipt, verified delivery, paused delivery and stale evidence; with words
queued behind a current receiver that no receipt names it reads **Queued ·
awaiting provider receipt**, since an earlier receipt says nothing about them
(a message still in the queue with a receipt from the current process counts
as delivered). The session
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
  The focus ring is the keyboard's: a click focuses a control without one, and
  the next key press shows it on whatever is focused.
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
