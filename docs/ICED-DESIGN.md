# The Iced desktop

The `agentdocker-ui` entry point uses Iced. It reads the same local
daemon as the CLI. Packaging ships the CLI, daemon and desktop together.

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
checkout and branch details. A launch uses the selected repository root, the
folder on the header title's tooltip. Other sessions have their own entry when a project cannot
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

Shared chat is the default project workspace.
Project switches preserve each conversation's draft and thread history without
marking hidden messages read. **Open project terminal** opens a fresh native shell
in the selected physical project folder, not the AgentDocker command console.
On macOS the directory is passed to Terminal as a file argument, never typed into
an existing prompt. Agent **Terminal** attaches an app-managed PTY for any runtime;
for external macOS sessions it brings the matching Terminal tab forward after
checking the provider's PID and birth time. It sends no keystrokes. Sessions in
other terminal applications show an actionable failure rather than opening a
second agent. Linux project-terminal launching is supported.
The chat composer stays in the viewport while long headers/forms scroll above it.
Compact threads have **Back to chat**; side panes collapse before the composer
loses its minimum width. Removing a project, including a vanished folder, selects
the replacement project's chat as well as its heading and preserves other drafts.
External Terminal focus allows 30 seconds for macOS Automation consent and explains
where to check the permission if the action times out.


| Destination | Everyday purpose |
| --- | --- |
| Projects | All projects home; selecting a project opens its shared Chat with a compact list of current agents. Agents is the second tab and Board the third; History, Files in use, Usage and AgentDocker commands are under More. Open project terminal is visible in the project header. |
| Inbox | Questions, retained answer drafts and messages addressed to the user; Messages (conversations, threads, read cursors) against a daemon that keeps them |
| Tools (rail id `connections`) | Installed tools and connection status. **Set up** appears for an installed tool missing MCP or hooks when no active session report exists. Expanded Details offers **Review setup**, health and history; setup plans use **Connect**, and applied plans offer **Undo** |
| Settings | Appearance, terminal, update checks, installation and retained versions |

Light and dark appearances share the same hierarchy and the same visual
system, which is drawn from the mark. The look never changes what a
control does, its id or its accessible label:

- **Brand.** The rail leads with the cube mark (`crates/ui/src/mark.png`, the
  transparent mark downscaled to 96 px, drawn at 22 points) and no wordmark:
  the window title names the app. An empty place is the mark in a quiet tile
  at reduced opacity, or the one thing to do there, with no title or sentence
  (`view::empty`).
- **Every project has its own mark.** A rounded tile with the project's
  initial on a tint derived from the project's identity (`style::identity`:
  an FNV hash of the repository fingerprint, or of a root-path UUID without one,
  picks one of twelve hue stops; saturation and lightness are fixed per theme
  and a unit test keeps the ink at 4.5:1 on every stop). Colours stay the same
  across machines when they use the same repository fingerprint. Root-path UUID
  seeds can differ between machines or checkouts. Nothing needs to be chosen or
  stored. The tile leads each rail row at 20 points, the project heading
  at 32 and each session row at 28.
- **Every agent carries its tool's mark.** A session, a tool, a launch
  choice, a conversation with an agent, a transcript run, a board card's holder and a usage row by agent, model or provider show
  the vendor's logo on a quiet tile (`view::agent_mark`, `logo_tile`):
  Claude for Claude Code, Desktop and the browser extension; OpenAI for
  Codex and ChatGPT; Gemini, GitHub Copilot, Cursor, Windsurf, VS Code and
  OpenCode for theirs. A tool with no mark here (Aider, Goose, Amp), a
  person and an unknown runtime keep the identity monogram, and the
  notices AgentDocker sends carry the cube. The marks are the vendors'
  trademarks, shown only to say which tool a session is; they come from
  svgl.app's official logos, rasterised once to 96-pixel transparent PNGs
  (`crates/ui/src/logos/`, light and dark variants for one-colour marks)
  and decoded once each, like the cube — the build has no SVG renderer
  (`app/logos.rs`).
- **One standing per agent.** Every agent's signals fold into a few
  states with a fixed precedence.
  `agentdocker_core::standing::Standing::of` is the one function: **needs
  you** (an open question, a provider stopped at its own prompt, or held
  messages), **failed** (until seen), **working**, **parked** (waiting on a
  lease), **starting**, **finished since you looked**, **idle**, **ended**,
  loudest first, so sorting by it is the attention order. Every view draws it
  the same way (`view::standing_glyph`, `view::standing_tone`): an amber dot
  for needs you, a red triangle for failed, a muted dot for working, an
  hourglass for parked, a dashed ring for starting, an accent dot for news,
  nothing for idle or ended. The word comes in a tooltip, and a session
  row's spoken name says what the agent is doing; no row prints it.
- **Finished, not yet viewed.** When a session's activity goes from working
  or blocked to idle or finished while the user is not looking at its project
  in a focused window, the session is marked unviewed: its standing is
  *finished since you looked* (the accent dot, its words in a tooltip and in
  the row's spoken label), its name is set semibold, its project's rail row
  takes the glyph, and the Projects entry carries a total. Opening the project, selecting the
  session or going to its terminal clears it, as does the session starting to
  work again; waiting for the person (`Activity::Waiting`) is not finished and
  drops the mark. An idle report on its own never counts as viewing, and the
  observed state stays in the row's spoken name unchanged. Window-local state,
  never persisted (`shell::State::unviewed_done`, filled by
  `App::note_completions`).
- **Pull request chip.** An agent row whose branch has a pull request on
  GitHub carries one `Kind::Inline` chip (`App::pull_request_chip`): the
  number and a glyph for its checks — `Icon::Check` green, an amber dot,
  `Icon::Close` red — or, once merged, the muted done glyph
  (`Icon::LaneDone`); its words are its spoken name; it opens the page with `/usr/bin/open` or
  `xdg-open`, the URL one argument. `pull_request.rs` reads it with
  `gh pr list --repo <owner/name> --head=<branch> --state all --json …`, the
  repository taken from the git configuration's GitHub remote (upstream, then
  github, then origin), `gh` found on `PATH` or in the Homebrew directories,
  each call bounded at ten seconds. One round of at most eight branches runs
  off the UI thread and answers in one message, so a refresh costs one frame;
  a branch is asked again after two minutes, or fifteen seconds once the
  window regains focus. A failed lookup keeps the last answer; nothing is
  persisted.
- **Colour roles** live in `app/style.rs`: ground, rail, card, raised,
  overlay, text, muted, faint, line, line-strong, accent, accent-hover,
  accent-soft/ink, primary, primary-hover, on-primary, cyan, green, amber, red
  and danger (the solid fill of an armed destructive action). The surfaces are
  one neutral family (dark ground rgb(21, 21, 20), light rgb(246, 246, 244) with white cards)
  and colour is spent on state, the one primary action filled with ink. The sidebar
  sits a step off the page, never a contrasting block. `Colors::rail()` is the sidebar step of either appearance
  (lighter than the page in dark, darker in light) with washes of the ink for
  the selected and hovered rows. A unit test keeps text, muted and faint ink
  above WCAG thresholds on every page surface and on the rail and its rows;
  the label on the primary action, white on the armed danger fill and status
  tones on a card are checked too.
- **One radius scale** (`RADIUS_XS` 4 checkboxes and an empty list's placeholder bars, `RADIUS_SM` 6
  controls, rows and tooltips, `RADIUS_MD` 8 menus, tracks and tiles,
  `RADIUS_LG` 10 cards) and one control height: a 13-point Medium label on an
  18-point line inside 7 points of padding is 32 points, for buttons, inputs
  and the split button alike. Depth is tint and hairline: rail under ground
  under card under raised, with `overlay` (menus, popovers, tooltips, the
  launch dialog, the palette) the only surface above a card. Small menus and
  tooltips carry its small shadow; the dialog and the palette do not
  (`Colors::dialog_style`): they are large, and the scrim lifts them.
- **No blurred shadows on large surfaces.** tiny-skia renders a quad's shadow
  by building a per-pixel colour buffer for the whole shadow area on every
  frame, including the part scrolled out of view, which costs gigabytes of
  allocation with a hundred session rows. Cards and
  panels get depth from tint and hairline only. Tiny shadows on tooltips and
  the selected segment are a few hundred pixels and remain.
- **Status is a mark that says its words.** The standing glyph sits on every
  agent row and beside the name in the session panel, where its tooltip adds
  the tool and where the agent runs; no row prints the word. Elsewhere a state
  is a dot (`view::status_dot`), a failure the amber alert mark
  (`view::alert_mark`, or `view::error_line` with its first line clipped
  beside it) and a time left a ring or a meter, each with its words as its
  tooltip and its accessible name; any other glyph that stands for words
  carries them as its accessible name through `view::said`, usually with the
  same words on the pointer. A screen reader and the workflow driver hear
  what the eye reads. Cyan marks a process available to connect.
- **Controls have kinds** (`controls::Kind`) on one ladder: one filled
  *primary* action per screen, *secondary* actions as hairline outlines with
  no fill, *ghost* words for tertiary actions (Cancel, Dismiss, Details,
  Pause…), *quiet* rows that only gain a surface when hovered or selected (and
  *inline* words with the same quiet look at their own width), underline
  *tabs*, *segments*, *nav* rows inked with the rail's roles, red
  *destructive* words for a menu's Remove or Forget, and a solid red *danger*
  fill for an armed stop. The primary fill is ink (`primary`, its label
  `on_primary`), and a selected control is a neutral wash with the full ink,
  never the accent. Hover and press are washes of the ink over whatever
  the control sits on, so one rule reads on the ground, a card and the rail.
  The keyboard focus ring is a 2-point accent edge drawn inside the control.
  Every kind is the same keyboard-focusable, AccessKit-labelled control; a
  custom-content button still carries a spoken label.
- **Menus float.** `controls::popover` lays out its anchor where it stands
  and, while the application says it is open, places its popup as an
  overlay under the anchor (above it when there is no room), aligned to the
  anchor's start or end and kept inside the window; a press outside both or
  Escape asks for it to close. Opening a menu moves nothing on the page, and
  a press anywhere on an open menu — its padding, a separator, a line of
  text — stays in it rather than reaching the control beneath. A menu opened
  from the keyboard takes the focus to its first entry (`shell::State::
  pressed` tells a pointer-opened menu, which leaves the focus alone), since
  overlays come last in the Tab order.
  Iced runs operations over overlays too, so the popup's controls are
  focusable, carry accessibility nodes and are found by the workflow driver.
  The project row's **⋯** menu (Rename…, Pin, Remove from list, the path),
  the **More** tab's menu (History, Files in use, Usage, AgentDocker commands,
  then Pin and Forget project, the screen on view ticked) and the
  **Launch agent** split button's menu of installed tools all float this way.
  A project row's **⋯** shows only under the pointer, on the selected row and
  while its menu is open; elsewhere it is drawn transparent but stays a
  control for the keyboard and assistive technology.
- **Type.** IBM Plex Sans is bundled (`crates/ui/src/fonts/`, Regular, Medium
  and SemiBold, SIL Open Font License; the license text ships beside the files
  and belongs in the app bundle's licenses). It is the default face, so weights
  and glyph coverage do not depend on the host: the macOS system sans had no
  Bold face for the default family and borrowed heavier glyphs from a
  monospace fallback. Headings are SemiBold; Bold is never requested. Section
  labels are sentence case, never tracked capitals; paths and identifiers use
  the system monospace.
- **Colour.** One neutral family, a tone step from surface to surface. The
  primary action is filled with ink (near-black in light, near-white in dark),
  not a hue; blue is the signal for new, active and focus; green, amber and red
  are standing. The rail carries the mark alone, without a wordmark.
- **Jump to… (⌘K, Ctrl+K elsewhere).** A search-looking field under the
  brand (`command-palette`) and the shortcut from anywhere, including a
  focused text field, open one palette (`app/palette.rs`): a search field
  (`palette-search`) over grouped rows, the groups parted by hairlines rather
  than headings — Agents (unsearched, only those whose
  standing calls for attention, loudest first; searched, every live one, its
  standing word in the description; the row's mark is its standing glyph or
  its tool's logo; a row may carry a second action, run with ⌘Enter and
  marked with a terminal glyph while highlighted, which for an agent is its
  terminal), Go to
  (All projects, Messages or
  Inbox, Tools, Settings, with their ⌘2–⌘4 shortcuts), Projects (each with its
  mark), Actions (Launch agent… with a nested page of installed
  tools, Open project terminal, Pause…, Add project…, the other appearance)
  — each row an existing message (`palette-item-<kind>-<key>`). A row's
  description (an agent's standing word, a project's folder) is searched and
  spoken, and its shortcut and second action spoken, none of them drawn:
  there are no keycaps and no key footer, and a search that matches nothing
  shows the search glyph. Matching is
  case-insensitive, in tiers that never mix: label prefix, then every term
  starting a label word or the label's initials starting with the query, then
  a substring, then words of the description, then (three or more letters)
  the query's letters in order in the label. Up/Down or Tab move the
  highlight, Enter runs it, Escape clears the search, then leaves a nested
  page, then closes; Backspace on an empty search leaves a nested page. The
  keys are read by a wrapper at the window root because a focused field
  otherwise types the *k* and keeps Escape. The window is always the first
  layer of one stack, with the launch dialog and the palette as optional
  layers over it, so opening either resets nothing underneath.
- **Segmented controls** (`controls::segment` in a `segmented` track) pick one
  of a few peers: the session filters and the theme. The track is an inset
  with a hairline; the selected segment is a card-coloured thumb with a
  hairline (and, in light, a one-point shadow), the others muted words.
- **Marks explain themselves.** Where a mark has no words beside it, pointing
  at it shows them: a rail project tile gives the folder, how many agents are
  live and how many finished unviewed (`hint`); an icon button says its name
  (`tip`); a status mark says its words (`status_dot`, `alert_mark`, `said`
  under a `tip`). The hover target is padded to about sixteen points; the mark itself
  stays small.
- **Tabs carry glyphs** (stacked bars, pulse, speech bubble, three dots) drawn
  the same way as the rail icons. Icon geometry is cached between frames and
  redrawn only when its colour changes, so idle frames repaint nothing for it.
- **An idle window costs nothing to speak of.** Every message Iced delivers
  ends in a whole frame: on macOS the software surface starts from a fresh
  buffer each time, so the window is painted entire and copied to the
  compositor. Frames are
  therefore rationed rather than made cheap. State arrives through the
  daemon's event stream and the workers' wake, never by polling for a
  redraw; the clock sweep that keeps "12s ago", a question's time left and
  the toast's eight seconds (`STATUS_FOR`) honest runs every two seconds while the window has focus
  and every thirty while it does not (`FOCUSED_SWEEP`, `UNFOCUSED_SWEEP`),
  since nobody reads those labels in a window they are not looking at; and a
  wake becomes one frame forty milliseconds later (`WAKE_SETTLE`), so the
  seven answers to a sweep's snapshot requests, which land within a few
  milliseconds of each other, are one frame rather than seven.
- **Board, History, Files in use, Usage.** A board's lanes are tinted wells
  with no border (a lane that is the drop target firms its edge; one past
  its limit tints red), headed by the lane's shape, name and a count; cards
  are hairline cards (no shadow) whose edge firms under the pointer, turns
  accent when open and red for high priority. A card is dragged with a
  `mouse_area` press, and released over a lane or beside a card it moves
  there; released where it was pressed, it opens in place. Every card stays
  one AccessKit control named by its key, title, lane and holder, so the
  keyboard and the smoke open it as before. Filing is a title line at each
  lane's foot. An open card's moves are icon buttons named in their
  tooltips; its **Save** is the card's one filled action. History is one row
  grammar in one card: an icon tile per journal kind, the line, a muted line
  of kind, branch and short sha, the time at the right. Usage leads with
  range and grouping as segmented controls and four totals, then one framed
  table with sentence-case headers and right-aligned numbers (a share bar
  from 1180 points wide).
- **Messages** (`app/messages.rs`, helpers in `app/messages/look.rs`). The
  conversation list is the project chats — each project's chat on its project
  monogram, named for the project, opening that project's Chat — then, under
  a rule and with no group label, the direct messages, rows of a 28-point mark
  with a presence dot, the name alone, cut with an ellipsis, and a quiet
  unread count (the `@n` count in the accent). Only conversations with something in them
  are listed, an ended session's only while it holds something unread, and
  nothing folds: what two agents said to each other is not listed. `#all`, channels, the
  daemon's contested-path rooms and its notices to an agent are not listed
  (`messages::hidden_kind`); their unread is read through as each list
  arrives (`App::read_hidden_conversations`, once per newest message), and
  the CLI reads them (`agentdocker history`, `agentdocker channels`). The
  history groups messages into runs at render time — one mark and one header
  (the name, its time on the pointer, a question or hand-off as its glyph)
  until the sender or kind changes, five minutes
  pass or a message names the person — with flat rows in the full text
  colour; AgentDocker's notices are single muted lines on a small disc. The
  day and the start of the unread part are bare rules named for a reader
  (`said`); unread rows are tinted under the accent **New** rule; the open thread's message
  and a notification's target carry a 2-point accent rail, a mention an
  amber one. The person's own message carries one delivery dot in its lead
  state's tone, the summary its name and every recipient its tooltip.
  **Reply** is drawn only under the pointer or keyboard focus
  (Iced's `hover` widget), so it is always present for the driver and
  assistive technology. `controls::framed_composer` is the field, a hairline
  and a footer holding only a 28-point send arrow (`send-<key>`,
  spoken *Send*), the whole frame showing focus; the keys are in the field's
  spoken name. Lines under it say the error, the queued state with
  **Details** and recoveries, and the recipient's readiness is a named dot
  beside **Connection**. Mention offers float above it. The thread is headed by
  the channel glyph, where it is on the pointer, and quotes
  its parent on the accent rail; the new-conversation form is a card listing
  the agents a direct message can go to. Loading is a faint ring. A question put to every agent
  shows its card in the direct conversation with whoever asked it. A
  conversation's header is its name, the topic on its tooltip, and a
  project's Chat has none. The project chat's side panel
  lists agents as the standing glyph, the mark and the name (*status · 3m* on
  its tooltip), the pull request chip and a chevron to the agent's details,
  under **All agents** and, when agents ask, **Answer N**.
- **Tools, Settings, Installation** (`app/tools_view.rs`). Tools is one card of
  rows: a two-letter mark on the tool's tint, the name beside a state dot
  whose tooltip and accessible name carry its status words and version, then
  one action — **Set up** filled only for the first tool
  that needs it (an outline for the rest, and for all while a setup is under
  review) — and a
  **Details** disclosure with a definition list of the facts the tool has (MCP,
  Hooks, Shell, Version, Command, Browser, URL, Code), its live sessions as a
  name with a readiness dot and MCP and Hooks contact dots, and no
  explanatory notes: where to paste the connector URL is its tooltip, and an
  incomplete inventory the alert mark. Busy is a ring and a failure one line
  beside the alert mark. The connection check is a tick or the alert mark
  per tool, the problems on its tooltip. A setup review is one card named
  for its tool (or Shell): a block per file
  with a path header, +N and its MCP, Hooks or Shell lines tinted with a left
  bar, and a footer of the state's named dot whose one filled action is
  **Connect**. Settings is a grouped form, as macOS
  forms are (`form_section`): each group's name, 13 pt semibold, sits above a
  card of rows; a row is its 13 pt name on the left and the control on the
  right, padded 8 by 14, with rules inset 14 (the same rows serve
  Installation), and no row or group has a line of why: the theme as a
  segmented control, text sizes as `− 14 pt +` steppers, switches for
  notifications, roomier rows, update checks and preview builds, terminal
  colours as preview swatches with an accent ring. The terminal
  pane has a header (status dot, agent over its folder in mono, Live and a
  small Detach) above the grid on its palette's ground, and no key footer;
  Commands is a panel
  with an amber `$` prompt. Installation leads with one update row: a disc
  whose tooltip and accessible name say what the state means, the version
  alone, then **Later**
  as a ghost and the update the one filled action (`UpdateLater` steps it down
  for that version, window-local); a schema change is the alert mark, busy a
  ring and a failure one line. Then versions in mono with status words, the
  Package and Prefix fields, and a stage rail for a plan under review.
- **Questions and approvals** (`app/view/approvals.rs`). A question card is
  headed by an amber tile, the question as its title (an MCP request leads
  with its server instead), and the time left as a small ring whose words
  (*4m left*, *Expired*) are its tooltip and accessible name; an answer on its
  way is a faint ring named *Sending*, and a refusal one line beside the
  alert mark. Structured choices are full-width
  option rows in one card, the label on the left and its consequence on the
  right (the consequence is also the option's accessible value), a refusal
  (Deny, Cancel, Stop) in red; once one is chosen the others go and the
  chosen row stays filled, with a tick, while the answer is delivered
  (`shell::State::chosen_answers`, display only). A request to run, change
  files or reach the network is one framed card: a strip of the request's
  glyph (what it wants, on the pointer), who asks and the time-left ring, the
  command in a darker mono well behind an
  amber `$` with a copy button, the folder and the reason as lines led by
  faint glyphs named on the pointer, the deny note's *Why not?* line, and a
  footer that steps down — solid Allow, outline Review changes (file
  requests), plain-word Deny. Consent and form-safety wording are Accept's
  and Submit's tooltips, a required form field an asterisk with what it takes
  on the pointer, and an encoded domain the alert mark. **Needs you** is one
  card headed by an amber dot and a count chip (spoken *Needs you, n*), its
  rows the asker (with the leave glyph once its session has ended, or the
  errand's glyph for a wait at its own terminal), the ask on one line,
  the ring and **Answer** as an outline, with **Show N more** as a ghost.
- **Meters.** A lease row carries a thin bar of the time left on it, the time
  on its tooltip; it turns amber under one fifth. A question carries the time
  left to answer it as a small ring drawn once per change of fraction (cached
  like the glyphs), red under one fifth. Rows do not say when a session
  started, and a screen that holds a series (History, Files in use) is its
  card of rows with no pane header.
- **Rail glyphs are drawn, not shipped.** `app/icons.rs` strokes five icons
  (folder, envelope, joined nodes, sliders, plus) on a sixteen-point grid
  through the canvas widget, inked in the row's own colour, so they stay crisp
  at any density and in both appearances with no icon font or bitmap. The
  build enables `canvas` for this (tiny-skia geometry; no GPU renderer).
- **Scroll anchors.** Question cards and transcript lines carry container ids
  `notification-question-<id>` and `notification-message-<id>`; `controls::reveal(id)` scrolls one into view
  without moving keyboard focus, for notification routing and **Answer**. The
  message histories are anchored to their end (their scrollables' ids start
  `history-`): Iced reports their position from the top but takes a scroll
  offset from the end, so reveal converts it.
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
  starting a direct message `new-conversation` (the + beside the search) and
  `new-direct-<agent>`; a mention offer `mention-<agent>`. On the Board
  Usage under More (`project-tab-Usage`): `usage-since-24h|7d|30d`,
  `usage-by-Agent|Model|Provider|Hour`. On the Board
  tab (`project-tab-Board`): a lane's `lane-add-card-<lane>` opens its
  `quick-add-<lane>` (activating it files), `lane-archive-<lane>`, a card
  `task-<id>` (opens it), its moves `task-back-<id>`/`task-next-<id>`,
  `task-approve-<id>`/`task-bounce-<id>` in a Review lane that is the
  person's, `task-hand-<id>-<agent>`, `task-release-<id>`,
  `task-archive-<id>`, `task-close-<id>`; `board-<slug>` switches boards;
  `board-more` for the next page; `board-filter-agent`, `board-filter-mine`,
  `board-tags` and `board-tag-<tag>`, `board-layout`, `board-density`; a
  table row `task-row-<id>`; on an open card `card-plan`, `card-attach`,
  `card-upload`, `card-link`, `card-start`, their choices `card-plan-<id>`,
  `card-attach-<id>`, `card-start-<runtime>`, `card-link-kind-<kind>`,
  `card-link-target`, `card-link-add`, and per file or link
  `card-file-<id>`, `card-file-unlink-<id>`, `card-link-open-<n>`,
  `card-link-copy-<n>`, `card-link-remove-<n>`; in the board's settings
  `tag-new`, `tag-color-<tag>`, `tag-delete-<tag>`, `rule-enter`,
  `rule-idle`, `rule-lane-<lane>`, `rule-days`, `rule-comment|move|archive|
  start`, `rule-text`, `rule-to-<lane>`, `rule-runtime-<runtime>`,
  `rule-name`, `rule-add`, `rule-on-<id>`, `rule-delete-<id>`. To-do
  (`todo`): `todo-<id>` opens a card, `todo-done-<id>` ticks it. Plans
  (`project-tab-Plans`): `plan-new`, `plan-<id>`, `plan-title`,
  `plan-status-set-<status>`, `plan-save`, `plan-card-<id>`, `file-add`,
  `file-<id>`, `file-delete-<id>`. The plan's Markdown is an Iced
  `text_editor`, not a composer: Enter is a new line there. When the page beside the rail fits every lane at 150 points with
  12 between (about 800 for the classic five) the lanes share it and the
  open card's detail — its fields, moves, comments and history — sits
  beneath them; narrower than that (a narrow window, or a
  wide rail in a small one), the lanes stack and the detail sits under
  its card. A composer's
  accessibility node carries its send as the input's action, which is what
  Enter does, so the smoke drives Enter as a click on the input's id. The screen takes an explicit height from the window (the
  window less the chrome, at least 320) because it sits inside the workspace's
  own scroll, where `Fill` has nothing to fill; `scripts/iced_workflow_smoke.py`
  asserts that reading a conversation acknowledges its rows and clears its
  unread count.
- **Layout.** A neutral rail (236 points to begin with, 204 when narrow) whose
  selected entry is a wash of the ink; a project row shows its loudest agent's
  glyph, a number only for agents that need the person, and a semibold name
  when something there is new; the rail has no Projects label. Only a project has a page header;
  elsewhere the selected rail row names the screen. It is one toolbar
  line: the project's mark, its name (its folder on the name's tooltip), then three icon
  buttons named in tooltips (**Pause…**, the project terminal, **Open in…**)
  and the one labelled and filled action, the **Launch agent…** split button whose chevron lists the installed tools
  (`launch-menu`, `launch-with-<tool>`, each tool's command path on its
  tooltip). Then the section tabs (glyph and
  label, a 2-point bar in the text colour under the selected one on the tabs' hairline).
  Typing a pause reason is its own tinted row under the header; while paused
  the header shows the amber pause mark, the reason on its tooltip (spoken
  *Paused · reason*), beside **Resume**. **Launch
  agent…** opens a dialog over a flat scrim (no blur), with no title, that starts from the
  prompt: a composer (*What
  should it work on?*, sent as the new agent's first message once it is
  launched, optional), the installed tools as chips under it (a chosen one a
  neutral chip with an ink edge), the idle
  delivery switch for Claude and Codex and **Own worktree** on one row, each
  saying what it means on its tooltip, then **More
  options** for the name and the arguments, each field named by its
  placeholder, and Cancel and Launch at the foot. Pressing the scrim closes it. The rail and the workspace, and on the
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
  hovered or dragged; at rest a hairline parts the rail from the page. Scrollbars are quiet: no
  track, a thin scroller in a wash of the ink. A narrow window keeps the
  fixed rail and the one column on view. Lists are rows inside a panel; prose sits in cards; the terminal
  and command output sit in a bezel of the chosen terminal palette's ground.
  Filters sit left and search right on one row. The background service is
  said once, at the rail's foot (`service_chip`), as a dot and a word with the
  full words on the dot's tooltip, and only when it calls for
  something: reconnecting, older than the app with **Restart…**
  (`daemon-restart`), or an update to install (`open-available-update`); a healthy one says nothing, and a lost
  connection has no second banner on the page. The bottom
  strip exists only while a restart is confirmed or under way. What just
  happened is a toast at the window's foot for eight seconds (`STATUS_FOR`),
  and a success the screen already shows (an answer, a launch, a rename, a
  saved setup) raises none; a page error is one clipped line beside the
  alert mark, with **Dismiss**.
  The agents list is one card of one-line rows separated by rules: the
  standing glyph, the tool's small mark, the name (semibold when it calls for
  the person, muted when ended), and one trailing slot: the time left to
  answer as a ring while the agent needs the person, else the pull
  request chip; no status word or age is printed. Tool, branch
  and tokens are in the name's tooltip, the row's spoken name adds what it is
  doing, and an action shows only where there is one
  (**Reconnect here**). The chat's agent list uses the same row, with what the
  agent is doing and for how long on the name's tooltip. An empty list shows three faint static rows in
  the shape of what will fill it; an empty project adds its next step, **Launch agent…**
  (`empty-launch`), and no words; a search that finds nothing
  offers **Clear search** (`session-search-clear`) alone. A
  session's panel has one filled action — **Answer** when it asks something,
  otherwise its terminal — Details as a definition list, and Stop at the
  foot: the first press turns that slot solid red, **Confirm stop**, with a
  square **Keep running** (`stop-session-cancel`) beside it.
- **Say each fact once.** Counts live in the sidebar (agents that need you
  per project, open questions on Inbox) and in the filter chips; the header does not repeat
  them. Messages in Inbox are transcript lines (when · who · what)
  rather than a card per message. As a rule, text on a screen is the
  labels of controls and the person's data (names, message bodies, card
  titles, paths), with one- or two-word placeholders: subtitles, explanatory
  paragraphs, empty-state sentences, key-hint footers and ids under headings
  are left out, and words that carry information move to a tooltip or a
  mark's accessible name.

Blue marks what is new, active and focused; selection is a neutral wash and
the primary action ink. Status always has words, as the tooltip and
accessible name of its mark where the screen shows only the mark. Session
actions sit beside a wide list and replace a narrow one, with an explicit
return button. Long content scrolls; focused controls are revealed.
The socket path is not shown; installation internals live in the plan a preview shows.
Shared Chat is the project default. The Agents tab defaults to current sessions;
finished runs sit in the collapsed Earlier
group under them (`sessions-earlier`) and unanswered questions have a
project-scoped Needs input filter. Current rows prioritize
questions, then newest sessions, with ID as a stable tie-breaker. Search includes
name, runtime, branch and session ID. Filter counts reflect that search.
Board, History, Files in use, AgentDocker commands and project management
live under More. The project terminal remains visible in the header. Tools shows
installed tools first and expands technical details on request. Full daemon
records remain intact: these are view filters, not registry deletion or migration.
Discovery suppresses known Codex Node launchers with a native child; the UI also
suppresses overlapping discovery/registration snapshots with matching known
PID and birth time. Live records the daemon holds for one provider session
(same known PID and birth time, runtime, project and `session_id`) are one row
and one count, under the record that stays (`identity::keeper`: input
delivered through it, then the first registered) until the daemon's sweep folds
the other into it. A browser agent from the connector counts as live only
within an hour of its last tool call (`AgentRecord::is_present`); past that it
sits under **Earlier**, *not connected* in its spoken name, still registered. Unknown identities
and distinct registered sessions remain separate. Legacy duplicate registry reconciliation is the offline `identity-repair`
command.

## Interaction contracts

- Observed process, installed tool, configured integration and current activity
  are separate facts. The app displays daemon observations and keeps unknown
  states explicit. Disconnection retains the last snapshot, said once by the
  rail foot's *Reconnecting…*; the session panel's readiness dot adds *Last known*.
- Questions preserve drafts across navigation and failed sends. Duplicate sends
  are disabled while waiting. A successful reply means delivery, not proof that
  an agent consumed it or resumed work.
- Conversation/thread, session, question-answer and Board-card text persists under the desktop's
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
  A file's channel drafts (the `channels` field, kept so older files load) and
  conversation drafts for `#all` or a channel are let go on loading; nothing
  in the app can send them.
- Each conversation keeps its own draft and pending send. A late
  acknowledgement clears only the text it sent. Reading never drains the
  queue. Inbox messages follow their envelope destination and duplicate IDs
  appear once; payload fields cannot move them to another room.
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
original session, conversation or thread. The default presentation is
one status line under the composer with a quiet **Details** link (spoken
*Delivery details*); expanded details scroll within a
180-point area and show named recipients plus explicit open/copy controls. The
snapshot is not persisted and never changes message acknowledgement, provider
consent or queue order. Expanded session/tool details, the readiness dots and
the Tools overview render the one readiness decision
(`agentdocker_core::readiness::decide`): its words, a tone by its state, and
one action for its one fix.

## Keyboard and native accessibility



All action buttons participate in Tab/Shift-Tab traversal and activate with
Enter/Space. Repeated key events do not repeat an activation. Escape closes
forms/details; Command/Ctrl+1–4 switch primary sections and Command/Ctrl+K opens
Jump to…. F6 leaves terminal input;
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
commit and multiline paste; they do not replace physical keyboard/IME validation.
Unchanged view layouts retain the editor's shaped text so native captures keep
their drawn glyphs. Edits, navigation, font changes and resized bounds reshape it;
a renderer regression checks glyph availability across repeated layout passes.

The rendered controls supply AccessKit labels, roles, values, actions, focus and
physical-pixel bounds. The native adapter is installed before showing the window.
macOS uses NSAccessibility, Linux AT-SPI, and Windows UI Automation. Keyboard
widget tests and native capture automation complement these adapters; they do
not substitute for a human VoiceOver/Orca/Narrator and input-method trial.

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
export CARGO_TARGET_DIR="$PWD/target/iced"
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
navigation, terminal attachment, the Messages rail (no channel, `#all` or
contested room listed, a channel's messages read through), setup preview/apply/undo,
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
or changing the saved conversation.

The standard suite includes strict lint, nextest, doctests, installer/package
checks and release builds. Desktop CI packages and runs native workflow acceptance
on macOS and Linux; Windows builds and exercises the daemon/CLI over its native
named pipe and tests the core/host foundations. Equivalent package size, launch time, memory and CPU
measurements must accompany release decisions, using exact binary provenance.
See [DISTRIBUTION-SETUP.md](DISTRIBUTION-SETUP.md) for public release gates.

Mentioning an agent does not change recipients. Suggestions only name current
recipients.

The app keeps to each project's chat and direct messages: it has no Channels
screen, no channel creation or invitation, no **Reviews** or **Add members** in
a conversation's header and no `#all` or contested-room rows; the daemon and
CLI keep all of them (`agentdocker channels`, `agentdocker channel …`,
`agentdocker history <conversation>`). A notification about a channel or `#all`
message opens the direct conversation with its sender (an agent) or Messages,
with the history command in a toast; a reply typed into such a
notification is refused with the `agentdocker send --to …` command instead.

The new-conversation button keeps a compact width beside search; opening its form does not divide the search row into two equally wide controls. Its accessible name is “New message” (or “Close” for the open form).

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
missing.
