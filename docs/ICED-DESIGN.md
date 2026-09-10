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
| Connections | Installed tools, explicit capabilities, reviewed setup, health and undo |
| Settings | Appearance, installation, retained versions and diagnostics |

Light and dark appearances share the same hierarchy. Blue marks selection and
primary actions; status always has words. The existing icon and system fonts
avoid an additional decorative asset bundle. Session details sit beside a wide
list and below a narrow one. Long content scrolls; focused controls are revealed.
Socket paths and installation internals live in diagnostics and detailed reports.

## Interaction contracts

- Observed process, installed tool, configured integration and current activity
  are separate facts. The app displays daemon observations and keeps unknown
  states explicit. Disconnection retains the last snapshot with a stale notice.
- Questions preserve drafts across navigation and failed sends. Duplicate sends
  are disabled while waiting. A successful reply means delivery, not proof that
  an agent consumed it or resumed work.
- Each channel has its own draft and pending send. A late acknowledgement clears
  only the text it sent. Channels show membership, reviews, resolution and queued
  human messages; reading never drains the queue or invents a complete transcript.
- Launch uses an installed supported CLI, explicit arguments, the selected folder
  and a managed PTY. It does not restart on window launch. Stop requires a second
  explicit activation within five seconds; detaching leaves the agent running.
- Terminal transport retains bounded input/output, resize coalescing, scrollback,
  replay and shutdown behavior. The Iced widget draws the real VT grid, including
  ANSI/true colors, styles, wide Unicode cells and a cursor. Input supports native
  IME, Unicode, clipboard paste and application cursor mode. Copy currently copies
  the visible screen; selecting an arbitrary cell range is not implemented.
- Commands run the actual bundled CLI in the selected project, without a shell.
  Output, history, request queues and execution deadlines remain bounded. Late
  results do not steal navigation focus.
- Setup and installation preserve their existing exact-preview/apply and
  undo/rollback checks. A framework migration does not relax process identity,
  release lifetime pins, queue limits or private state requirements.

## Keyboard and native accessibility

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
I/O. The build enables `tiny-skia`, `crisp`, Tokio, X11/Wayland and advanced widgets.
It excludes the default GPU renderer and image codecs. PNG decoding serves the
existing window icon. CLI-only installs do not pull the GUI dependencies.

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
