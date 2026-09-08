# A quieter desktop with Iced

Status: design proposal and working native preview, starting from `862162a`.
The existing egui application remains the production entry point. The Iced
preview uses fictional data and never contacts a daemon or provider. It exists
to make navigation, hierarchy, and state transitions reviewable before porting
the operational workflows.

## What needs to change

The current window offers ten peer destinations: Agents, Questions, Channels,
Terminal, Console, Runtimes, Journal, Leases, Settings, and Installation. That
mirrors implementation features. A person must assemble project context by
moving between them. The frame also gives a socket path permanent prominence,
while most of the window can remain empty beneath a short agent table.

The central questions should be:

1. What is happening in this project?
2. What needs my attention?
3. What can I do with this session?

Changing frameworks alone will not answer those questions. The redesign also
changes navigation, language, grouping, and the visibility of advanced tools.

## Proposed structure

| Destination | Contents | Current screens it brings together |
| --- | --- | --- |
| Projects | Project list, sessions, recent activity, selected session details | Agents; project-scoped Journal and Leases |
| Inbox | Human questions, answers, review requests, and explicit outcomes | Questions; relevant Channels content |
| Connections | Installed tools, supported capabilities, verified health, reviewed setup and undo | Runtimes |
| Settings | Appearance, installation, updates, retained releases, diagnostics | Settings; Installation |

Terminal belongs to a selected session and should preserve that session's
identity when switching projects. Advanced CLI commands belong in an optional
command panel. Durable project channels remain available inside their project;
they must not become a fabricated chat transcript or silently drained inbox.
The later migration must retain these workflows even though their old sidebar
items disappear.

Projects are listed beneath the primary navigation. The default proposal is to
restore the last selected project. A project with no sessions has a useful empty
state, not an empty table. Whether an all-project overview should be the default
remains a design decision for review.

## Visual direction

A quiet desktop utility with a compact sidebar, generous spacing between
sections, and compact session rows. Light and dark appearances use the same
hierarchy. Keep the existing brand; avoid bundling a decorative font or inventing
a replacement icon system.

- Blue means selection or a primary action. It is not also a project identity.
- Green accompanies a positive observation; amber marks a question or problem
  that needs attention. Unknown activity uses neutral text.
- Every status has words as well as colour.
- Session names lead each row; provider and branch are secondary.
- A question gets a short attention row with a direct route to the Inbox.
- Details appear after selection. A wide window places the inspector beside
  the sessions; a narrow window places it below them in the scrollable content.
- Socket paths, process identifiers, lease keys, and build metadata belong in
  contextual details and diagnostics. They should not dominate the app frame.

The prototype has Command/Ctrl+1–4 section shortcuts, Escape to dismiss session
details, and Tab/Shift-Tab operations for text-input focus. Iced 0.14's stock
buttons handle pointer/touch events but do not participate in focus traversal;
the preview does not yet provide a complete keyboard path through its buttons.
That is an explicit migration gap, not an accessibility claim. Production
cutover requires focusable actions and platform screen-reader, contrast,
scaling, and input-method trials.

## Interaction contracts to preserve

An observed process, an installed tool, configured wiring, and fresh activity
are different facts. An unknown state is a legitimate result. Sending an answer
does not prove the recipient consumed it or resumed work.

Keep answer drafts across navigation, disconnection, and failed delivery.
Display action failures beside the action and preserve the input needed to
retry. Disconnection retains the last snapshot with an explicit stale-state
notice. Setup and installation continue to preview exact changes before apply,
and retain their undo/rollback checks. Stop remains a clearly confirmed action;
detach remains distinct from stopping a process.

Preserve command queue and byte limits, late-reply rejection, terminal output
bounds, process identity checks, and release lifetime pins. Moving to Iced is
not permission to relax the already tested behavior.

## Why Iced, and what we are measuring

The current stable release checked on September 8, 2026 is
[Iced 0.14.0](https://docs.rs/iced/0.14.0/iced/). Its
[state/message/update/view model](https://book.iced.rs/architecture.html)
provides explicit boundaries between presentation, state transitions, and
effects. `Task` and `Subscription` support commands and ongoing observations;
they still need application-level deadlines, cancellation and bounded queues.

The optional preview enables the `tiny-skia` software renderer, a Tokio
executor, X11/Wayland support, and image handling for the existing icon. It
does not enable Iced's default GPU renderer. The lockfile may contain optional
GPU dependencies; the resolved preview build graph determines what is compiled.
This is a renderer experiment. Scrolling, terminal output, CPU and idle power
must be measured before selecting a production renderer.

Iced's declared compiler minimum is 1.88. The CLI's existing minimum remains
1.87. The current egui application already declares 1.95. New dependencies are
confined to the optional preview target and are not shipped in the current app.

The previous verified Apple Silicon package is 13.7 MB compressed / 32 MB
installed, with a 7.1 MB CLI/daemon download. A design preview with sample data
cannot establish that a complete Iced port is smaller or faster. Retain the
existing release size gates and measure equivalent complete packages at cutover.

The local Apple Silicon preview measures approximately 2.5 MB compressed and
5.3 MB installed, including its icon. This is only the design app: it contains
neither the daemon/CLI nor a working terminal. The capture manifest records the
exact byte counts and source/binary hashes. The review bundle is locally signed,
not a notarized public release.

## Migration in reviewable increments

1. **Design preview (this change).** Native Projects, Inbox, Connections and
   Settings; search, project selection, contextual inspector, draft retention,
   appearance changes, empty and disconnected scenes, actual window captures.
2. **Read-only operational view.** Extract the existing framework-independent
   client/snapshot boundary. Feed typed daemon observations into Iced away from
   the UI thread. Preserve physical project/session identity, bounded refreshes,
   stale-state notices and rejection of replies for an old selection.
3. **Actions and attention.** Port questions/answers, reviewed setup, adoption,
   stop confirmation and project channels with the current failure semantics.
   Keep effects separate from view construction and never infer success from a
   button press.
4. **Terminal and advanced workflows.** Preserve parser/transport behavior,
   attach/detach, resize, replay bounds, Unicode/IME input and keyboard handling.
   Port the console, journal, coordination details and installation workflows.
5. **Cutover.** Run equivalent packaged acceptance on macOS/Linux and the
   supported Windows foundations. Check keyboard/screen-reader behavior,
   scaling, memory, CPU, launch time and complete package size. Switch the
   production entry point and remove egui/eframe only after parity is verified.

A second GUI toolkit does not provide the missing Windows daemon/ConPTY/service
adapters. The full Windows application remains separate platform work.

## Run the preview

Use a single build cache for this campaign:

```sh
export CARGO_TARGET_DIR=/private/tmp/agentdocker-iced-build
python3 scripts/build_storage.py
cargo run --locked -p agentdocker-design --features preview --release
```

The `preview` feature is required, so normal workspace builds do not compile
this binary. Release packaging continues to select the existing CLI, daemon and
GUI executables. To capture an actual preview window and exit:

```sh
cargo run --locked -p agentdocker-design --features preview --release -- \
  --details --screenshot /private/tmp/iced-project.png
```

The PNG path must not already exist. Other options: `--dark`, `--empty`,
`--offline`, `--width 720`, and `--page inbox|connections|settings|projects`.
The preview keeps all interactions in memory. It is not a new installed release.

Validate the optional target explicitly and capture the review scenes:

```sh
python3 scripts/build_storage.py
cargo test --locked -p agentdocker-design --features preview --release
cargo clippy --locked -p agentdocker-design --features preview \\
  --all-targets --release -- -D warnings
python3 scripts/iced_design.py \\
  --binary "$CARGO_TARGET_DIR/release/agentdocker-design" \\
  --output artifacts/iced-design/local-review
```

The output directory must be new. The driver captures ten actual rendered
windows, including narrow layouts, and packages a local macOS review app.
These captures check layout; the model tests check draft/selection retention
and disconnected answer handling. They do not establish operational parity,
full pointer/keyboard acceptance, or production performance.

## Decisions for the next discussion

- Last project or an all-project overview on launch?
- Inspector beside the list, or a full detail screen after selection?
- Keep the proposed light default, or follow the operating system?
- Is the main daily workflow monitoring several agents, or working closely
  with one agent's terminal and questions?

The first prototype assumes focused project work and a quiet native utility.
These choices are intentionally visible and reversible before a full migration.
