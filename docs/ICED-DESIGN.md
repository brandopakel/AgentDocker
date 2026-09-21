# The Iced desktop

The production `agentdocker-ui` entry point uses Iced. It reads the same local
daemon as the CLI and replaces the previous egui window. The separate fictional
design preview has been removed; its original design and captures remain in
history at `8be9d82`. Packaging still ships the CLI, daemon and desktop together.

## Project model

Projects combine two paths into one catalog:

- **Discovered projects** appear when the daemon observes registered or supported
  running agents and can identify their working directory.
- **Add project…** chooses an existing folder and pins its project. Adding does
  not launch anything, create repository files, or edit provider configuration.

Identity follows the existing host project discovery rules. A canonical physical
repository root joins pinned folders, discovered processes and registered
sessions. Registered project fingerprints supersede weaker discovery metadata.
Linked worktrees stay grouped under their repository; individual sessions retain
checkout and branch details. A launch uses the selected repository root shown
in the header. Other sessions have their own entry when a project cannot
be established. An installed desktop provider alone does not reveal its projects.

The app restores the last selected project and appearance on launch. Restoring
only changes the view; it does not restart sessions. Projects remain after their
agents finish. A missing folder keeps its saved entry and offers a recheck.
Unpin keeps a recent project; Forget removes the catalog entry without touching
files or sessions. Active discovery may subsequently restore a forgotten project.

`workspace.json` lives in the AgentDocker state directory. Its project catalog
is bounded to 512 entries and 2 MiB; it also keeps the column widths a person
dragged (`panes`). Atomic private-file replacement, one save in
flight and generation tracking prevent late writes from reverting newer choices.
Closing the window waits for the current preference changes. Corrupt or unsafe
preferences are preserved and reported instead of overwritten. Existing `ui.json`
appearance preferences are read when no new appearance has been saved.

## Navigation and visual decisions

The September 18 user review chose shared chat as the default project workspace.
Project switches preserve each conversation's draft and thread history without
marking hidden messages read. **Open project terminal** opens a fresh native shell
in the selected physical project folder, not the AgentDocker command console.
On macOS the directory is passed to Terminal as a file argument, never typed into
an existing prompt. Agent **Terminal** attaches an app-managed PTY for any runtime;
for external macOS sessions it brings the matching Terminal tab forward after
checking the provider's PID and birth time. It sends no keystrokes. Sessions in
other terminal applications show an actionable failure rather than opening a
second agent. Linux project-terminal launching is implemented; native Linux
acceptance and external-terminal focus beyond macOS Terminal remain open.
The chat composer stays in the viewport while long headers/forms scroll above it.
Compact threads have **Back to chat**; side panes collapse before the composer
loses its minimum width. Removing a project, including a vanished folder, selects
the replacement project's chat as well as its heading and preserves other drafts.
External Terminal focus allows 30 seconds for macOS Automation consent and explains
where to check the permission if the action times out.


| Destination | Everyday purpose |
| --- | --- |
| Projects | All projects home; selecting a project opens its shared Chat with a compact list of current agents. Agents is the second tab; Board, History, Channels, Files in use and AgentDocker commands are under More. Open project terminal is visible in the project header. |
| Inbox | Questions, retained answer drafts and messages addressed to the user; Messages (conversations, threads, read cursors) against a daemon that keeps them |
| Tools (rail id `connections`) | Installed tools and connection status. **Set up** appears for an installed tool missing MCP or hooks when no active session report exists. Expanded Details offers **Review setup**, health and history; setup plans use **Connect**, and applied plans offer **Undo** |
| Settings | Appearance, installation, retained versions and diagnostics |

Light and dark appearances share the same hierarchy and the same visual
system, which is drawn from the mark:

- **Brand.** The rail leads with the cube mark (`crates/ui/src/mark.png`, the
  transparent mark downscaled to 96 px for a 30-point slot at 2× density) and
  the two-tone wordmark: *Agent* in the text colour, *Docker* in the accent.
  The mark also anchors empty states at reduced opacity.
- **Every project has its own mark.** A rounded tile with the project's
  initial on a tint derived from the project's identity (`style::identity`:
  an FNV hash of the repository fingerprint, or of a root-path UUID without one,
  picks one of twelve hue stops; saturation and lightness are fixed per theme
  and a unit test keeps the ink at 4.5:1 on every stop). Colours stay the same
  across machines when they use the same repository fingerprint. Root-path UUID
  seeds can differ between machines or checkouts. Nothing needs to be chosen or
  stored. The tile leads each rail row at 20 points and the project
  heading at 30.
- **Finished, not yet viewed.** When a session's activity goes from working
  or blocked to idle or finished while the user is not looking at its project
  in a focused window, the session is marked unviewed: an accent pill on the
  row ("Done"), a count on its rail row, and a total on the Projects entry.
  Opening the project or selecting the session clears it, as does the session
  starting to work again. An idle report on its own never counts as viewing,
  and the observed state stays in the row's meta line unchanged. Window-local
  state, never persisted (`shell::State::unviewed_done`, filled by
  `App::note_completions`).
- **Colour roles** live in `app/style.rs`: ground, rail, card, raised, text,
  muted, faint, line, accent, accent-soft/ink, cyan, green, amber, red. Dark
  is a deep navy; light is cool off-white with white cards. A unit test keeps
  text, muted text and selected ink above WCAG contrast thresholds on every
  surface in both appearances.
- **No blurred shadows on large surfaces.** tiny-skia renders a quad's shadow
  by building a per-pixel colour buffer for the whole shadow area on every
  frame, including the part scrolled out of view. With a hundred session rows
  in light mode that was gigabytes of allocation and a third of a core; the
  same window in dark mode, whose cards had no shadow, sat at 150 MiB and
  under two percent (measured 2026-09-10 on the packaged build). Cards and
  panels get depth from tint and hairline only. Tiny shadows on tooltips and
  the selected segment are a few hundred pixels and remain.
- **Status is a dot and a word.** Green is live, amber needs input, faint is
  finished, cyan marks a pinned project or a process available to connect.
  Header pills count live sessions and open questions for the project.
- **Controls have kinds** (`controls::Kind`): one filled *primary* action per
  screen, quiet raised *secondary* actions and filter chips, *quiet* rows and
  rail entries that only gain a surface when hovered or selected (and
  *inline* words with the same quiet look at their own width, so a card's
  hand-to names sit side by side), underline *tabs* for the project
  sections, and a red-tinted *danger* surface for an armed stop. Every kind is the same keyboard-focusable, AccessKit-labelled
  control; a custom-content button still carries a spoken label.
- **Type.** Inter is bundled (`crates/ui/src/fonts/`, Regular, Medium and
  SemiBold, SIL Open Font License; the license text ships beside the files and
  belongs in the app bundle's licenses). It is the default face, so weights and
  glyph coverage no longer depend on the host: the macOS system sans had no
  Bold face for the default family and borrowed heavier glyphs from a
  monospace fallback. Headings are SemiBold; Bold is never requested. Section
  eyebrows are 11-point capitals; paths and identifiers use the system
  monospace.
- **Segmented controls** (`controls::segment` in a `segmented` track) pick one
  of a few peers: the session filters and the theme. The selected segment lifts
  off the track; the others sit quietly on it.
- **Marks explain themselves.** Where a mark has no words beside it (the
  rail's project tiles), pointing at it shows a tooltip with the words the row
  would otherwise print: live agents, pinned or discovered, and how many
  finished unviewed (`hint`); dots that already sit next to their words get no
  tooltip. The hover target is padded to about sixteen points; the mark itself
  stays small. Verified with the real macOS pointer (a CGEvent helper moving
  the cursor over the running release window while the smoke captured the
  frame), not only with widget callbacks.
- **Tabs carry glyphs** (stacked bars, pulse, speech bubble, three dots) drawn
  the same way as the rail icons. Icon geometry is cached between frames and
  redrawn only when its colour changes, so idle frames repaint nothing for it.
- **Meters.** A lease row carries a thin bar of the time left on it; it turns
  amber under one fifth. A question card carries the time left to answer it,
  red under one fifth. Session rows say when they started. Panels that hold a
  series (Activity) start with a pane header: what the pane holds on the left,
  one quiet fact on the right.
- **Rail glyphs are drawn, not shipped.** `app/icons.rs` strokes five icons
  (folder, envelope, joined nodes, sliders, plus) on a sixteen-point grid
  through the canvas widget, inked in the row's own colour, so they stay crisp
  at any density and in both appearances with no icon font or bitmap. The
  build enables `canvas` for this (tiny-skia geometry; no GPU renderer).
- **Scroll anchors.** Question cards, transcript lines and channel cards carry
  container ids `notification-question-<id>`, `notification-message-<id>` and
  `notification-channel-<id>`; `controls::reveal(id)` scrolls one into view
  without moving keyboard focus, for notification routing.
  Direct questions retained in Inbox after leaving the pending list use a
  160-character first-line preview and explicit Show/Hide question controls.
  Their full text remains intact. Notification routing expands its target before
  revealing it; details and navigation never submit or rewrite another draft.
  On the Messages screen the sidebar rows are `thread-<agent>` for a direct
  conversation (the inbox's id, so the same smoke drives both) and
  `conversation-<id>` otherwise; the composer is `reply-<agent>` or
  `compose-<conversation>`, a thread's `reply-thread-<message>` (its draft is
  keyed `<conversation>#<message>`, apart from the conversation's), thread
  links `thread-<message>`, the back controls `thread-back` (to the list) and
  `close-thread` (the thread's one close, a header action when wide and the
  way back when narrow, never both), earlier pages `earlier-<conversation>`;
  starting a conversation `new-conversation` (the + beside the search),
  `new-kind-direct`/`new-kind-channel`, `new-direct-<agent>`,
  `new-channel-name`, `new-channel-purpose`, `new-member-<agent>`,
  `new-channel-create`; a mention offer `mention-<agent>`. On the Board
  Usage under More (`project-tab-Usage`): `usage-since-24h|7d|30d`,
  `usage-by-Agent|Model|Provider|Hour`. On the Board
  tab (`project-tab-Board`): `task-title`, `task-acceptance`,
  `task-file-ready`, `task-file-backlog`, a card `task-<id>` (opens it),
  its moves `task-back-<id>`/`task-next-<id>`, `task-hand-<id>-<agent>`,
  `task-release-<id>`, `task-archive-<id>`; `board-more` for the next
  page. When the page beside the rail is at least ~800 px wide the five
  lanes share it and the open card's detail — title, acceptance text,
  moves — sits beneath them; narrower than that (a narrow window, or a
  wide rail in a small one), the lanes stack and the detail sits under
  its card. A composer's
  accessibility node carries its send as the input's action, which is what
  Enter does, so the smoke drives Enter as a click on the input's id. The screen takes an explicit height from the window (the
  window less the chrome, at least 320) because it sits inside the workspace's
  own scroll, where `Fill` has nothing to fill; `scripts/iced_workflow_smoke.py`
  asserts that reading a conversation acknowledges its rows and clears its
  unread count.
- **Layout.** A rail (236 points to begin with, 204 when narrow) with the
  selected entry marked by an accent bar, then a workspace that leads with
  the project name, its path and the project's one primary action, then the
  section tabs over a hairline. The rail and the workspace, and on the
  Messages screen the sidebar, the conversation and the thread, are
  `pane_grid` panes with a draggable divider between each (`app/panes.rs`):
  the widths are kept in logical pixels, not shares, so a window resize
  restores preferred widths whenever they fit. Preferred widths are bounded
  (rail 180–440, sidebar 200–560, thread 240–640) and rounded to whole pixels;
  a changed drag saves them in `workspace.json` (`panes`). Rendered side columns
  shrink to reserve at least 320 logical pixels for the conversation. If even
  the minimum columns cannot fit, Messages uses its existing one-pane
  conversation/thread navigation. Window resizing and text zoom recompute this
  layout without overwriting saved preferences. History only marks a conversation read
  when that same effective layout actually shows it; an open compact thread
  cannot acknowledge the hidden conversation. The two
  grids number their splits separately, so a resize event carries which grid
  it came from. The thread column is split off and closed with the thread.
  The dividers are 8 points wide, drawn as a 2-point accent line while
  hovered or dragged. A narrow window keeps the fixed rail and the one
  column on view. Lists are rows inside a panel; prose sits in cards; the terminal
  and command output sit in a bezel of the chosen terminal palette's ground.
  Filters sit left and search right on one row. A footer bar says the daemon
  connection and the version once, so no page repeats them.
- **Say each fact once.** Counts live in the sidebar (live agents per project,
  open questions on Inbox) and in the filter chips; the header does not repeat
  them. Messages in Channels and Inbox are transcript lines (when · who · what)
  rather than a card per message, following the chat clients in the iced
  showcase (Halloy); the pane-header, footer-bar and inline-meter patterns come
  from Kraken Desktop and Sniffnet there.

Blue marks selection and primary actions; status always has words. Session
actions sit beside a wide list and replace a narrow one, with an explicit
return button. Long content scrolls; focused controls are revealed.
Socket paths and installation internals live in diagnostics and detailed reports.
Shared Chat is the project default. The Agents tab defaults to current sessions;
finished runs sit in the collapsed Earlier
group under them (`sessions-earlier`) and unanswered questions have a
project-scoped Needs input filter. Current rows prioritize
questions, then newest sessions, with ID as a stable tie-breaker. Search includes
name, runtime, branch and session ID. Filter counts reflect that search.
Board, History, Channels, Files in use, AgentDocker commands and project management
live under More. The project terminal remains visible in the header. Tools shows
installed tools first and expands technical details on request. Full daemon
records remain intact: these are view filters, not registry deletion or migration.
Discovery suppresses known Codex Node launchers with a native child; the UI also
suppresses overlapping discovery/registration snapshots with matching known
PID and birth time. Unknown identities and distinct registered sessions remain
separate. Legacy duplicate registry reconciliation is the offline `identity-repair`
command ([IDENTITY-REPAIR.md](IDENTITY-REPAIR.md)); applying it to the production
database remains a manual step.

## Interaction contracts

- Observed process, installed tool, configured integration and current activity
  are separate facts. The app displays daemon observations and keeps unknown
  states explicit. Disconnection retains the last snapshot with a stale notice.
- Questions preserve drafts across navigation and failed sends. Duplicate sends
  are disabled while waiting. A successful reply means delivery, not proof that
  an agent consumed it or resumed work.
- Conversation/thread, channel, session, question-answer and Board-card text persists under the desktop's
  state-root/daemon-socket identity. Only text is restored, never send state or
  queued receipts. Serialized atomic saves preserve the newest generation;
  close waits for it or reports failure with retry/explicit unsaved-close controls.
  Saves debounce for 250 ms with a one-second maximum typing delay. Admission
  keeps at most 128 drafts per kind, 16,000 characters per draft and 4 MiB of
  aggregate UTF-8 text; only empty drafts can be pruned. Files are private,
  versioned and bounded to 32 MiB; invalid loads disable writes and preserve the
  file. Explicit structured choices use the answer queue directly even when draft
  storage is full; failed delivery preserves earlier typed text. They still require
  a current question and any applicable file review. Answer drafts retain their original question IDs; confirmed completion
  removes them, while failed delivery keeps them. No approval, review or send
  state is restored. Version 3 reads version-1 message and version-2 answer files
  without rewriting them until the next edit. Board titles and acceptance text
  remain keyed to the original project, with 200/4,000-character limits and the
  same aggregate storage budget. A confirmed filing clears only that project's
  draft; a refused or old response cannot clear newer text. No filing state is
  restored and reopening never creates a card. Other forms remain window-local.
- Each channel has its own draft and pending send. A late acknowledgement clears
  only the text it sent. Channels show membership, reviews, resolution and queued
  human messages, plus confirmed sends from this window. Reading never drains
  the queue. The sent-message cache retains at most 128 entries and 256 KiB of
  text; it survives navigation and polling but not window closure. The view
  labels its partial history. Messages follow their envelope destination and
  duplicate IDs appear once; payload fields cannot move them to another room.
- Launch uses an installed supported CLI, explicit arguments, the selected folder
  and a managed PTY. It does not restart on window launch. Stop requires a second
  explicit activation within five seconds; detaching leaves the agent running.
- Terminal transport retains bounded input/output, resize coalescing, scrollback,
  replay and shutdown behavior. Read deadlines bound a stalled reader's closure
  check; partial frames retain their bytes across deadlines, including splits
  inside UTF-8 characters. Closure is also checked between chunks so continuous
  unterminated output cannot hide it. Frames remain limited to 256 KiB and only
  complete JSON is decoded. The first input/output failure remains visible.
  The Iced widget draws the real VT grid, including
  ANSI/true colors, styles, wide Unicode cells and a cursor. Input supports native
  IME, Unicode, clipboard paste and application cursor mode. Drag to select a
  cell range, then use Command+C or Control+Shift+C to copy it. Selection holds
  only the visible grid while the parser continues draining output, so copied
  text stays tied to the highlight. Copy, Escape, typing, scrolling, resizing or
  leaving the window returns to current output. Wide and combining characters
  remain whole, and soft-wrapped lines join without extra newlines. With no
  selection, copy retains the existing visible-screen behavior.
- Commands run the actual bundled CLI in the selected project, without a shell.
  Output, history, request queues and execution deadlines remain bounded. Late
  results do not steal navigation focus.
- Setup and installation preserve their existing exact-preview/apply and
  undo/rollback checks. A framework migration does not relax process identity,
  release lifetime pins, queue limits or private state requirements.

Send receipts carry bounded recipient-readiness metadata into the draft for the
original session, channel, conversation or thread. The default presentation is
one attention line with **Delivery details**; expanded details scroll within a
180-point area and show named recipients plus explicit open/copy controls. The
snapshot is not persisted and never changes message acknowledgement, provider
consent or queue order. Expanded session/tool details compute current guidance
from the same provider-block and receiver-evidence rules.

## Keyboard and native accessibility

See [the current engineering and release backlog](REMAINING-WORK.md) for open
implementation and acceptance work.

All action buttons participate in Tab/Shift-Tab traversal and activate with
Enter/Space. Repeated key events do not repeat an activation. Escape closes
forms/details; Command/Ctrl+1–4 switch primary sections. F6 leaves terminal input;
Control+] detaches. Button focus follows stable identity when session rows move.
The focus ring follows the input modality: a click focuses a control without
it (the pointer knows where it clicked) and it stays off that control until a
key is pressed — whatever asks for the focus meanwhile, the click included —
while Tab and a focus the app moves to another control show it; losing focus
forgets the click
(`a_clicked_control_is_focused_without_a_ring_until_the_keyboard_asks`).
Text inputs use Iced's native input-method support. Message and answer composers
use Iced's multiline editor: plain Enter sends through the existing button action,
Shift+Enter inserts a line, and clipboard paste retains line breaks. Native
selection/copy/navigation bindings remain intact. Preedit and repeated Enter
events never submit. The editor grows to 120 points and scrolls inside that
bound; receipt/error feedback retains its separate bounded scroll area.
Application drafts remain authoritative and limited to 16,000 characters;
cursor/selection/preedit are widget-local and never restored as a submission.
Changing destination resets focus. A send receipt, mention or notification edit
refreshes the editor from the same draft used by the accessibility SetValue path.
Native widget-event regressions cover these boundaries, including Unicode IME
commit and multiline paste; they do not replace physical keyboard/IME trials.
Unchanged view layouts retain the editor's shaped text so native captures keep
their drawn glyphs. Edits, navigation, font changes and resized bounds reshape it;
a renderer regression checks glyph availability across repeated layout passes.

The rendered controls supply AccessKit labels, roles, values, actions, focus and
physical-pixel bounds. The native adapter is installed before showing the window.
macOS uses NSAccessibility, Linux AT-SPI, and Windows UI Automation. Keyboard
widget tests and native capture automation complement these adapters; they do
not substitute for a human VoiceOver/Orca/Narrator and input-method trial. On
September 21, installed macOS preview `30ce582a` exposed 716 nodes, 212 buttons
and four terminal actions through external AXUIElement inspection after unlock;
More expansion/collapse and Agents/Chat navigation passed through AXPress. This
closes the locked-desktop inspection gap, not the human screen-reader/IME trial.

## Build and validation

[Iced 0.14](https://docs.rs/iced/0.14.0/iced/) supplies the
[state/message/update/view model](https://book.iced.rs/architecture.html),
`Task` effects and `Subscription` observations. `app/shell.rs` owns transitions,
`app/view.rs` renders daemon state, and the existing bounded workers own blocking
I/O. The build enables `tiny-skia`, `crisp`, Tokio, X11/Wayland, advanced widgets,
canvas geometry for the drawn rail glyphs, and raster images without codecs. It excludes the default GPU renderer and image
codecs. The `png` crate decodes the window icon and the embedded mark. CLI-only
installs do not pull the GUI dependencies.

Use one cache for a build campaign:

```sh
export CARGO_TARGET_DIR=/private/tmp/agentdocker-iced-production
python3 scripts/build_storage.py
bash scripts/verify.sh check
python3 scripts/build_native.py
python3 scripts/iced_workflow_smoke.py \
  --binary-dir "$CARGO_TARGET_DIR/release" \
  --output artifacts/iced-workflows
```

The workflow driver opens actual native windows in private disposable state.
A smoke window is kept beneath every ordinary window (`Level::AlwaysOnBottom`
whenever `--smoke-test` is given): it holds fixture data, so it must never land
on top of the installed app and be taken for it; captures come from the
renderer, not the screen, so they are unaffected.
A capture step waits one extra beat after the step before it: a screenshot
renders the last drawn frame, and text whose widget state changed since that
frame is skipped, so a capture taken in the same beat as a change can show a
stale layout with missing text. Captures from debug builds are slower to
settle than release builds and are for review, not for the acceptance report.
It drives the rendered controls' callbacks through question delivery, draft
navigation, terminal attachment, channel messaging, setup preview/apply/undo,
folder pinning, agent launch/stop, CLI commands, focus reveal, resizing, appearance
and a second launch. The Stop sequence waits for its control to disappear after confirmation, then
checks that the exact launched record exited; two clicks alone are not success.
Daemon and on-disk assertions verify outcomes. macOS also
probes the app's native NSAccessibility hierarchy. The driver does not claim
physical keyboard injection, provider consumption or a screen-reader trial.

Readiness trials explicitly enter the compact Messages layout and use its
**Conversations** back control before selecting a direct message. Opening a
project selects shared chat; Messages preserves that conversation, so the list
is hidden on a compact display until the person goes back. The fixture asserts
both the list entry and destination composer instead of assuming a wide sidebar
or changing the saved conversation. This covers the Mac ARM CI failure at
`Click thread-<receiver>` without extending deadlines or skipping the case.

The standard suite includes strict lint, nextest, doctests, installer/package
checks and release builds. Desktop CI packages and runs native workflow acceptance
on macOS and Linux; Windows builds and exercises the daemon/CLI over its native
named pipe and tests the core/host foundations. ConPTY, full desktop/service
packaging and real-provider Windows acceptance remain separate platform work. Equivalent package size, launch time, memory and CPU
measurements must accompany release decisions, using exact binary provenance.
See [distribution and signing](DESKTOP-DISTRIBUTION.md) for public release gates.

Messages review (September 17): mentioning an agent does not change recipients
or grant channel membership. Suggestions only name current recipients. A late
channel-creation reply may update only its originating form, preserving a newer
form and its draft. These corrections are merged in #170 and installed; source
and rendered workflow validation passed. Physical keyboard and IME acceptance
remain open, as recorded in [remaining work](REMAINING-WORK.md).

For an open named channel the person belongs to, **Add members** opens a list
of available agents who are not members yet. Adding one shows progress and
keeps failures in the form; confirmed members disappear from the available list.
Closing/replacing the form prevents a late reply from changing the new form.
A refused local queue submission releases the busy state and preserves the form.

The new-conversation button keeps a compact width beside search; opening its form does not divide the search row into two equally wide controls. Its accessible name remains “New message or channel” (or “Close” for the open form).

Project pause forms retain a separate bounded draft for each project. Their
buttons and Enter action carry that project, and a unique request identity
distinguishes pause from resume and old replies from new ones. Refusal, full
queues and transport failures preserve the reason and release pending controls;
a lost reply is an uncertain outcome to check before retrying, not an automatic
resubmission. The pause reason and actions occupy separate rows on narrow
windows. A durable pause blocks new agent lease claims but does not stop a model
process or establish that every recipient has consumed the pause message.

Notification reply recovery forwarding reserves the full JSON byte budget for
16,000 reply characters, a 512-character reason and validated destination
metadata. Escaped control characters and Unicode cannot silently reduce that
limit. Both ends still reject oversized frames. A full recovery list shows a
200-character excerpt and preserves uncertain-delivery wording.

Usage reads carry request identities across project, range and grouping changes.
Duplicate refreshes share the outstanding read; superseded replies are ignored,
and a refused queue leaves the last totals visible with an explicit error.
Usage filters stack and totals become labelled cards when the workspace cannot
fit the table, including a wide project rail or increased text size. Each card
keeps all five counter categories and the unknown/partial markers visible;
the wide table remains available when at least 940 logical pixels fit beside
the rail.

The default startup smoke capture waits for a redraw after its first ready
snapshot, as scenario captures already do. Capturing in the same update that
changes Inbox to Messages can otherwise retain the old frame with its text
missing. This affects capture evidence; it does not establish a persistent
interactive-window defect. The Windows startup repeat checks the new image.
