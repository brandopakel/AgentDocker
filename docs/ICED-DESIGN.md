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
in the header. Unassigned sessions have their own entry when a project cannot
be established. An installed desktop provider alone does not reveal its projects.

The app restores the last selected project and appearance on launch. Restoring
only changes the view; it does not restart sessions. Projects remain after their
agents finish. A missing folder keeps its saved entry and offers a recheck.
Unpin keeps a recent project; Forget removes the catalog entry without touching
files or sessions. Active discovery may subsequently restore a forgotten project.

`workspace.json` lives in the AgentDocker state directory. Its project catalog
is bounded to 512 entries and 2 MiB. Atomic private-file replacement, one save in
flight and generation tracking prevent late writes from reverting newer choices.
Closing the window waits for the current preference changes. Corrupt or unsafe
preferences are preserved and reported instead of overwritten. Existing `ui.json`
appearance preferences are read when no new appearance has been saved.

## Navigation and visual decisions

| Destination | Everyday purpose |
| --- | --- |
| Projects | Sessions and contextual Activity, Channels, Coordination and Commands |
| Inbox | Questions, retained answer drafts and messages addressed to the user |
| Connections | Installed tools, explicit capabilities, reviewed setup, health and undo. A tool whose hooks are missing or unverified gets an explicit **Install hooks** action and one sentence on what hooks add (live working, waiting and finished states) |
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
  rail entries that only gain a surface when hovered or selected, underline
  *tabs* for the project sections, and a red-tinted *danger* surface for an
  armed stop. Every kind is the same keyboard-focusable, AccessKit-labelled
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
- **Layout.** A 236-point rail (204 when narrow) with the selected entry marked
  by an accent bar, then a workspace that leads with the project name, its
  path and the project's one primary action, then the section tabs over a
  hairline. Lists are rows inside a panel; prose sits in cards; the terminal
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
Current sessions are the default; finished runs live in History and unanswered
questions have a project-scoped Needs input filter. Current rows prioritize
questions, then newest sessions, with ID as a stable tie-breaker. Search includes
name, runtime, branch and session ID. Filter counts reflect that search.
Coordination, Commands and project management live under More. Connections shows
installed tools first and expands technical details on request. Full daemon
records remain intact: these are view filters, not registry deletion or migration.
Discovery suppresses known Codex Node launchers with a native child; the UI also
suppresses overlapping discovery/registration snapshots with matching known
PID and birth time. Unknown identities and distinct registered sessions remain
separate. Legacy duplicate registry reconciliation remains engineering work.

## Interaction contracts

- Observed process, installed tool, configured integration and current activity
  are separate facts. The app displays daemon observations and keeps unknown
  states explicit. Disconnection retains the last snapshot with a stale notice.
- Questions preserve drafts across navigation and failed sends. Duplicate sends
  are disabled while waiting. A successful reply means delivery, not proof that
  an agent consumed it or resumed work.
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
  replay and shutdown behavior. The Iced widget draws the real VT grid, including
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

## Keyboard and native accessibility

See [the current engineering and release backlog](REMAINING-WORK.md) for open
implementation and acceptance work.

All action buttons participate in Tab/Shift-Tab traversal and activate with
Enter/Space. Repeated key events do not repeat an activation. Escape closes
forms/details; Command/Ctrl+1–4 switch primary sections. F6 leaves terminal input;
Control+] detaches. Button focus follows stable identity when session rows move.
Text inputs use Iced's native input-method support.

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
export CARGO_TARGET_DIR=/private/tmp/agentdocker-iced-production
python3 scripts/build_storage.py
bash scripts/verify.sh check
python3 scripts/build_native.py
python3 scripts/iced_workflow_smoke.py \
  --binary-dir "$CARGO_TARGET_DIR/release" \
  --output artifacts/iced-workflows
```

The workflow driver opens actual native windows in private disposable state.
A capture step waits one extra beat after the step before it: a screenshot
renders the last drawn frame, and text whose widget state changed since that
frame is skipped, so a capture taken in the same beat as a change can show a
stale layout with missing text. Captures from debug builds are slower to
settle than release builds and are for review, not for the acceptance report.
It drives the rendered controls' callbacks through question delivery, draft
navigation, terminal attachment, channel messaging, setup preview/apply/undo,
folder pinning, agent launch/stop, CLI commands, focus reveal, resizing, appearance
and a second launch. Daemon and on-disk assertions verify outcomes. macOS also
probes the app's native NSAccessibility hierarchy. The driver does not claim
physical keyboard injection, provider consumption or a screen-reader trial.

The standard suite includes strict lint, nextest, doctests, installer/package
checks and release builds. Desktop CI packages and runs native workflow acceptance
on macOS and Linux; Windows compiles the desktop/adapters and tests its existing
core/host foundations. Full Windows daemon/ConPTY/service packaging remains
separate platform work. Equivalent package size, launch time, memory and CPU
measurements must accompany release decisions, using exact binary provenance.
See [distribution and signing](DESKTOP-DISTRIBUTION.md) for public release gates.
