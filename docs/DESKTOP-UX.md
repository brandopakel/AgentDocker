# Using the desktop

`agentdocker ui` opens the native Iced app. It connects to the local daemon and
restores the last project and appearance. No browser, container engine or cloud
account is required to organize local agent work.

## Projects

The sidebar remembers projects found through agents and folders you add yourself.
**Add project…** lets you browse or enter an existing folder. It pins the project
without starting an agent, changing integrations or creating files. Later sessions
in the same repository appear there automatically. Linked worktrees share a
project and retain their session checkout details.

Quiet projects remain available. An unavailable folder stays selected and offers
**Check folder again**. **Unpin project** keeps it in recent projects; **Forget
project** removes its workspace entry. Neither deletes files nor stops sessions.
A project with active agents can be discovered again. Sessions whose project is
unknown appear under **Unassigned sessions**.

Session rows show the name, runtime, branch and observed activity. A selected row
opens details beside the list or below it, depending on the window width.
**Launch agent…** chooses an installed CLI and starts it at the project root shown
in the header. **Register session** adopts a discovered process for coordination.
Installation or configuration alone does not prove that an agent is working.

**Stop session…** changes to **Confirm stop** for five seconds. Confirm sends the
stop request. A managed live PTY offers **Open terminal**; **Detach** closes the
view while the process continues. Finished sessions remain visible.

Project tabs provide:

- **Activity:** the recent durable journal, updated from daemon events.
- **Channels:** project rooms, membership, reviews, resolution and messages still
  queued for you. This view does not drain your inbox or fabricate chat history.
  Each room retains its own draft across navigation and failed delivery.
- **Coordination:** current leases and their holders.
- **Commands:** the real bundled `agentdocker` CLI in the selected project folder.
  It keeps command history and output with a bounded execution deadline.

## Inbox and connections

Inbox presents questions addressed to you with a draft per question. Navigation,
a failed request and disconnection preserve drafts. A pending answer cannot be
sent twice. Delivery does not establish that the recipient consumed it. Direct
messages remain queued until a consumer explicitly takes them.

Connections lists installed runtimes and their reported capabilities. **Review
setup** prepares a specific plan; **Apply reviewed changes** and **Undo this setup**
use the existing checked CLI operations. Health and saved plans remain available.
A connection failure retains the last snapshot and clearly pauses daemon actions.

## Settings and installation

Settings controls light/dark appearance, text size, terminal palette and row
spacing. Preferences are stored privately in the AgentDocker state directory.
A malformed preference file is preserved and reported rather than silently reset.

**Manage installation and retained versions** previews installation, rollback,
cleanup and launcher removal. Apply is tied to the reviewed payload or cleanup
plan and refuses stale inputs. Running versions and agents remain protected by
the existing installer rules. Public macOS downloads require Developer ID signing
and notarization; locally signed builds are explicit previews.

## Keyboard and terminal

- Tab and Shift-Tab move between controls; Enter/Space activate a focused button.
- Command/Ctrl+1–4 switch Projects, Inbox, Connections and Settings.
- Escape closes session details or an add/launch form.
- F6 moves focus out of terminal input. Control+] detaches.
- Command+C/V on macOS, or Ctrl+Shift+C/V elsewhere, copy the visible terminal
  screen and paste clipboard text. Paste respects the agent's bracketed-paste
  mode. Arbitrary cell-range selection is not currently supported.

Text inputs and the terminal support native input methods. Focus indicators and
status words complement color. Native accessibility adapters expose control
labels, values and actions. Platform screen-reader and input-method trials remain
part of release acceptance; see [the design and validation contracts](ICED-DESIGN.md).
