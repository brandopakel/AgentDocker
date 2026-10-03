# Native Windows delivery work

Windows is an intended native platform. The Windows implementation is an incomplete port,
distributed as an unsigned portable preview. It does not use WSL, a browser server or
a required container engine to substitute for native execution.

The user confirmed native Windows, alongside macOS and Linux, for the **first
coworker rollout** on September 19 UTC. This port is therefore a delivery
requirement for that rollout, not a deferred platform enhancement. Track its
completion in [Remaining work](REMAINING-WORK.md) and use the same
[first-run acceptance](LOCAL-TRIAL.md#stage-5--other-machines-and-systems) as the
other platforms. Keep the public support matrix truthful until it passes.

The first boundary is core and host I/O: full-resolution process identities,
same-user process inventory without reading environments or requesting extra
privileges, nonblocking exclusive file locks, explicit protected user/SYSTEM
ACLs, local named-pipe names, bounded file observations, and Job Object command
ownership. State creation checks existing ancestry without changing its ACLs,
refuses foreign write/delete access and reparse points, and retains directory
handles while creating state. Existing owned broad-read state can be narrowed;
foreign-writable state is refused. Administrators and SYSTEM remain machine
administrators, as root does on Unix.

The Windows workflow runs core/host on a real Windows runner, including
ACL refusal, process identity and command descendant cancellation. Slice one
(#206, merged `eadae70`) added daemon/CLI named-pipe acceptance. Replacement #214 (superseding #210) builds all three binaries and adds managed sessions plus an opt-in
native window trial. On `cadf3d58`, 284 native tests and all 50 smoke steps
passed on Windows Server 2025, including terminal lifecycle, database crash
recovery and fresh-home desktop startup/capture. The full daemon/CLI test suites,
physical input and broader GUI/provider acceptance, installer/update path and
service acceptance remain open. Release workflows include a Windows portable
prerelease target. A successful cross-compile alone is not runtime acceptance.
Unix CI remains required.

File observations on Windows track native read-only attributes and change
metadata; Windows has no Unix executable permission bits. Captured Windows
container build inputs normalize files to 644, or 444 when read-only. An image
recipe must set container executable bits explicitly. Reparse points are not
accepted as ordinary files. Engine workspace transport is explicitly unavailable
on Windows until a checked named-pipe/VM transport is implemented.

## Portable desktop archive

Windows x64 desktop packaging now produces an unsigned ZIP containing the three
`.exe` files together, exact source/build and executable hashes, licenses and
opening instructions. The Windows workflow builds release binaries, checks PE
architecture, and runs its full daemon/CLI/terminal/desktop trial only after
extracting that ZIP outside the checkout into a path containing spaces and
Unicode. This archive path passed its native run on `13e87591` (run 35666723079: 284
native tests, 51/51 steps on the extracted bytes); it does not close Windows
installer/update/rollback, services, clean-machine or actual-provider
acceptance. Prerelease tags publish the accepted portable Windows target separately
from the macOS/Linux update feeds. See
[distribution setup](DISTRIBUTION-SETUP.md#windows-portable-preview).

## Slice one: the daemon and the CLI answer

The first integration slice is in source: `agentd` and `agentdocker` build
and lint natively for Windows, the daemon serves the shared named-pipe
transport (`agentdocker_host::ipc`, the same listener and stream the Unix
socket uses), and the CLI registers, lists, sends, reads, claims and stops
through it. The Windows workflow builds both, lints them strictly and runs
`scripts/windows_daemon_smoke.py` on a real `windows-latest` runner: a daemon
on a private home answers `ping`, two agents register with their pids, `ps`
lists them, a direct message and a project broadcast are read from an inbox,
a lease is held and a second claim refused, `stop` ends a registered process
whose identity matches its record, `daemon stop` ends the daemon, `daemon
start` brings one up on demand for the home (the ordinary first run: a
client with nothing to talk to starts the daemon and waits for it to
listen), `daemon stop` ends that one too, and on a home no daemon has made
yet a plain `ping` creates it and starts a daemon. Its report is the run's
`windows-daemon-smoke` artifact — on `7b3fd108` all 17 steps passed on
Windows Server 2025, the [record](verification/INDEX.md)
carries the report and the three failed runs before it; the same script
runs on macOS and Linux, so it is checked before the runner sees it —
there it ends its private
daemon directly when the user has a daemon service installed, since
`daemon stop` on macOS and Linux also drives that service, which is filed
per user and not per home.

What the slice changes in the shared code, on every platform:

- Ending a process goes through `agentdocker_host::procinfo::end(pid,
  started_at, force)`: the recorded birth is compared before anything is
  sent. On Unix that is the same `SIGTERM`/`SIGKILL` as before; on Windows
  the birth is read from the very handle that is then terminated, so the
  check and the act cannot straddle a pid recycling. Liveness is
  `procinfo::alive` on both (an access refusal is a yes, as with
  `kill(pid, 0)`). The daemon's stop path keeps its record checks unchanged.
- A file's identity across renames is `agentdocker_host::files::identity`:
  device and inode on Unix, volume serial number and file index on Windows,
  read from the open handle. The channel-receipt cursor stores it.
- The hook adapter's bounded stdin read and deadline-bound stdout write use
  a helper thread on Windows, where a console or pipe handle cannot be
  polled; the deadline is the same, and nothing partial is acknowledged.
- Private state reads (`read_private_file`, `check_private_dir`,
  `open_private`) exist on Windows, opening read-only with the kind and the
  ACL checked and nothing narrowed: a read is never a write.
- A daemon the CLI starts on demand does not hold the client's standard
  handles: on Windows a child inherits every inheritable handle of its
  parent, and a script's or shell's capture of `agentdocker` output is
  such a handle, so the daemon kept the capture open for as long as it
  ran — the third runner sat 26 minutes in `daemon start` that way, until
  the job's own timeout. `command::detach` makes the client's standard
  handles non-inheritable before the daemon is started; the daemon gets
  its own stdio from the command. The smoke captures every command through
  pipes as a script would and fails a command whose pipes are still held
  after it exits, so this cannot come back silently.
- Both binaries do their work on a thread with a 32 MiB stack. A Windows
  main thread has 1 MiB (Unix has 8), and the first Windows runner
  overflowed it in the CLI on `ping` (`thread 'main' has overflowed its
  stack`) — clap's derived parser for this many commands and the one future
  behind every command are large in a debug build. The reservation is address space
  until touched.

What the first real runner taught, kept in the smoke: the daemon creates
its home itself, directly under the temporary directory, as it does on a
person's first run. A directory the smoke made first was foreign-owned
state — objects an administrator creates on Windows belong to the
Administrators group, not the user — and the daemon refused it by design
(`state or ancestor belongs to an untrusted Windows principal`); a home the
daemon made under such a directory was refused as writable by another
principal, so nothing the daemon owns sits under one, and the report
records what a directory made there carries: CPython's
`os.mkdir(mode=0o700)` gives it an explicit DACL of SYSTEM, Administrators
and `OWNER RIGHTS` (S-1-3-4). The follow-up resolves that entry to the
object's already validated owner. It does not trust that SID globally or
change ownership checks: foreign-owned state and other principals' write
grants remain refused. A native regression checks those refusals and
unchanged parent ACLs; the runtime smoke starts a fresh home beneath an
actual Python OWNER RIGHTS parent. Native `cadf3d58` passed this compatibility trial, including unchanged parent
owner/ACL and protected child state; broader platform work remains in
[Remaining work](REMAINING-WORK.md). What the daemon creates
is owned by the user. The same held
for the CLI: a client starting the daemon on demand made the home with a
plain directory creation, which from an elevated shell belongs to
Administrators and was then refused by the daemon it started — the client
now makes the home the way the daemon does. A person who points
`AGENTDOCKER_HOME` at a directory they made from an elevated shell sees the
refusal, which names the path and the principal, and the fix is to let
the daemon make it. On a refusal the smoke records the owner and the
access-control entries of the home and its ancestors, since the runner is
the only place to observe them.

The native Codex queue now shares the receiver, hook and explicit recovery code
with Unix. Its Windows endpoints are private named pipes, authenticated by the
kernel peer PID and same-user token before protocol input. Process birth, provider
generation, hook ancestry and exact transcript receipts remain required. Windows
uses a host-wide monotonic deadline, file-handle identity and write-through private
ledger publication. Native CI and actual Windows Codex delivery acceptance remain
open; source support alone does not establish idle, busy or draft preservation.

What the slice refuses on Windows, in words rather than with a hang or a
crash, and what that means for a person:

- Live daemon reload and the descriptor handover (`daemon reload`): the
  daemon holds no descriptors a successor could inherit; stop and start it.
- Container workspace transport and grants: the endpoint is a Unix socket.
- The connector service remains unavailable. Per-user desktop installation and
  local preview update/rollback passed the native lifecycle described below.
  Daemon login startup is now implemented through
  a limited per-user Task Scheduler task, with a private ownership receipt and
  a bounded crash supervisor. `daemon install`, `uninstall`, `start`, `stop`,
  `restart` and `status` handle that task; start/stop still operate on demand
  when no task is installed. Real product lifecycle/provider-survival
  acceptance remains separate from source tests and the isolated supervisor
  prototype. See the [user commands](GUIDE.md) and
  [service semantics](ARCHITECTURE.md#starting-the-daemon).

  Manual Windows workflow runs can enable `service_acceptance` to exercise
  install, stop, start, restart, daemon-crash recovery and uninstall using the
  exact extracted portable binaries. The trial creates one random private home
  and its owned Task Scheduler task, disables client autostart, verifies the
  serving process image and home before inducing a crash, and requires cleanup.
  The report is retained beside package evidence. An interactive-logon pass
  does not establish login/reboot behavior or managed-provider survival. Its
  first native run on `bbfb4dcb`
  passed the 62 portable checks, then failed service install/uninstall before
  creating a task: the missing-task query returned a silent PowerShell failure.
  Lookup now selects the exact task from successful enumeration and propagates
  query errors instead of treating every error as absence. Corrected native
  run 36958504870 on `bf48452d` passed all 62 portable checks and eight service
  checks, including crash recovery without client autostart and repeat
  uninstall. The private task, processes and scratch were removed with no
  cleanup errors. The original failure remains retained. Lookup now compares
  names without regard to case before checking ownership. The native driver
  additionally creates a harmless, unstarted foreign task under a case-variant
  name and requires install/uninstall refusal with its XML unchanged. That
  regression passed on `c0dd784e` in native run 36963606339: all 62 portable
  checks and 11 service checks passed, the foreign task XML stayed identical,
  and all owned tasks, processes and scratch were removed cleanly.
  Failed cleanup retires verified process generations, supervisor first, while
  retaining the failed result and any cleanup errors.
- A validation command is ended on a timeout, but only the command itself:
  there is no process group and no Job Object around it yet, so whether its
  descendants survived is not reported.

Work still required before platform support can be claimed:

- `attach` from a real Windows console, by a person: the console modes,
  the keystroke reader and the size polling are in source and unexercised
  by the runner, which has no console.
- The native Codex queue over the named pipe with the same peer checks.
- Windows provider configuration: a provider's own tool under a pseudo
  console, and a person's setup on a Windows machine. What is in source: an
  npm-installed provider is a `.cmd` shim on `PATH` (`claude.cmd`,
  `codex.cmd` beside `node.exe`), and the launch gate now starts one the way
  a shell and the standard library do — one resolver for the runtime
  inventory and the launch (`command::find_program`: a bare name by
  launcher extension in `PATHEXT`'s order, never a data file, never the
  working directory), and a batch launcher run by `cmd.exe` from the
  system directory with the standard library's own batch command line
  (`cmd.exe /e:ON /v:OFF /d /c ""script" args…"`) and argument rules: a
  line-breaking argument is refused before anything runs, `%` is
  neutralised, arguments are quoted unless made of characters cmd leaves
  alone, and a canonical `\\?\` path is given as the plain path cmd
  understands only when the plain path names the same file (a verbatim
  path the plain rules would rewrite is refused, never run as another
  file). The lookup uses the child's own `PATH` and `PATHEXT` — the
  command's overrides over this process's, matched without case — and a
  relative script is made absolute where it was checked, so cmd.exe running
  from the child's directory runs that file and not a namesake there. The
  session's checkout is given to cmd.exe as a plain path too: a canonical
  Windows path is verbatim, and cmd.exe started in one falls back to the
  Windows directory ("UNC paths are not supported"), which the first runner
  showed — a provider would have run in the wrong folder. A checkout on a
  network share (a UNC path) is refused for a batch launcher before
  anything is created, for the same reason: cmd.exe would run in the
  Windows directory and say so only on stderr. A cold runner
  missed the client's three seconds for a first start of a fresh
  `agentd.exe`, with the daemon's log not yet written — what held it up
  was not observed (the system's scan of a new executable is one
  candidate) — so a client starting the daemon on demand allows ten
  seconds on Windows (three elsewhere). On the extracted archive's runner the
  smoke's own first daemon was alive but had neither answered nor written
  its log within the step's ping budget, which was a count of pings: the
  step is now a ninety-second diagnostic allowance by the clock that
  records what a first start takes — process creation, then readiness —
  so the next run says the number; it makes no claim about the client's
  own bound, which later steps exercise on a binary the system has already
  run. Host
  tests on the runner: the batch line against the standard library's
  shape, a `.cmd` shim run through the gate with a space and a `&` intact
  in its arguments, a refused argument leaving a marker-writing shim
  unrun, `PATHEXT` precedence between `shim.exe` and `shim.cmd` in both
  orders from the command's own variables, a relative script run from a
  different child directory, and a verbatim path refused when its plain
  form would not round-trip. The smoke's shim also prints its working
  directory, which must be the session's checkout. The smoke starts a managed session from a shim on the
  daemon's `PATH` by its bare name and reads its arguments back from its
  log. Not established: a real provider's shim (Node under the pseudo
  console) — that is the provider trial.
- Start menu/PATH integration, hosted Windows update/rollback, signed packages,
  and physical actual-provider service/session login and reboot acceptance.
- The daemon and CLI test suites on the Windows runner (they still carry
  Unix-only fixtures), native graphical acceptance, then a fresh
  real-provider integration and sustained lifecycle trials.

The supported download/platform matrix remains unchanged until those acceptance
stages pass. See [ARCHITECTURE.md](ARCHITECTURE.md#process-supervision) for the macOS/Linux stack and [verification/INDEX.md](verification/INDEX.md) for its trials.

## Slice two: managed sessions on Windows

In source: a managed session on Windows is the same session owner, the
same wire and the same daemon-side controller as on macOS and Linux
(`agentdocker_core::session`: `Activate`, keystrokes, window sizes, output
from an offset, the exit acknowledgement; the owner outliving any daemon),
with the platform pieces below answering what the Unix pieces do. `run`,
`run --tty`, `logs`, `stop` and `attach` are no longer refused on
Windows. The table is what each Unix piece is and what answers it.

| Unix piece | Where | Windows answer |
| --- | --- | --- |
| The terminal pair: `posix_openpt`, the child gets the slave as its standard streams and controlling terminal | `host::pty::Pty` | A pseudo console: `CreatePseudoConsole` over two pipes; the daemon side reads the output pipe and writes the input pipe; the child is created with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`. There is no slave to hand over: the attribute is what binds the child. |
| The launch gate: fork, hold before exec until the record is durable, `exec denied` on refusal, the pid and birth readable meanwhile | `host::launch::{prepare, Pending, OwnedChild}` | `CreateProcessW` with `CREATE_SUSPENDED` (plus `EXTENDED_STARTUPINFO_PRESENT` for the console attribute): the pid and the birth time are readable from the handle while the first thread has never run; `activate` is `ResumeThread`, refusal is `TerminateProcess` of a process that never executed an instruction. Rust's `Command` cannot carry the attribute list on stable (`raw_attribute` is unstable), so this is a direct `CreateProcessW` with the standard command-line quoting and an environment block built from the launch. |
| The process group the daemon signals: `setsid` in the child, `kill(-pid)` | `take_controlling_terminal`, `OwnedChild::drop`, `group_exists` | A Job Object the owner holds, the child assigned to it before it resumes (`CREATE_SUSPENDED` makes that a certainty, not a race). Stop is `TerminateJobObject`; "does the group still exist" is the job's active process count, which trails an exit by a moment (the first runner showed a just-exited child still counted), so the owner asks again rather than concluding from one answer. The owner holds a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: a daemon crash leaves the owner and child intact, while an owner crash ends every descendant even without destructors. Explicit `disown` clears that flag before surrendering ownership and returns an error if the clear fails. A Ctrl-C is queued through the bounded keyboard writer (`\x03`) without blocking the supervisor; a full queue leaves Ctrl-C unqueued but preserves the two-second stop grace; a missing or closed input ends the job immediately. When written into the pseudo console input, the console turns Ctrl-C into the child's `CTRL_C_EVENT`; there is no `SIGTERM`, so the graceful stop is that, then the job after the grace period. The child is not created in a new process group of its own: that flag makes a process ignore Ctrl-C and a child inherits the ignoring, so the owner clears its own inherited ignore before creating the child. A piped child has nothing to be asked with; its polite stop is the end. The daemon's own liveness question about a group (`group_exists`) is answered by the leader's liveness, since only the owner holds the job. |
| Window size: `TIOCSWINSZ` and `SIGWINCH` | `Pty::resize` | `ResizePseudoConsole`; the console tells the child. A mutex protects the handle through resize; close takes it once under the same mutex, then releases the mutex before native teardown. Later resizes fail with a closed-console error. |
| The owner's socket: `<home>/sessions/<agent>.sock`, `0600`, a lock beside it | `owner::serve`, `supervisor::Controller::connect` | The shared named pipe (`agentdocker_host::ipc`, protected DACL, same-user peer check) under a per-agent name derived from the home and the id; the lock stays a file in `sessions/`. |
| The owner survives the daemon: its own session, `SIGHUP` ignored | `owner::main` | Started with `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP` and, if the daemon is ever inside a job, `CREATE_BREAKAWAY_FROM_JOB`; the client's standard handles are non-inheritable before the start (as `command::detach` does for the daemon), so the owner holds none of the daemon's pipes. |
| Liveness and identity of the owner and the child | `procinfo::{alive, start_time, end}` | Already on Windows since slice one: the handle's creation time, `end` from the very handle it terminates. |
| `attach`: raw mode, the window size, cancellable readiness-driven stdin, `Ctrl-]` | `pty::{RawMode, window_size, nonblocking_input}`, `cli::attach` | `SetConsoleMode`: input without line, echo and processed input, with virtual-terminal input; output with virtual-terminal processing; both restored on drop. The size from `GetConsoleScreenBufferInfo`'s window; a resize seen by polling it, since there is no signal. A console input handle is waitable, so readiness-driven reading is `WaitForMultipleObjects` on it and a cancel event. |
| The log: every byte of output appended | `owner::write_log` | Unchanged: the output pipe's bytes. |

Where it lives: `crates/host/src/pty/windows.rs` (the pseudo console,
`window_size`, `RawMode` over console modes) and
`crates/host/src/launch/windows.rs` (the suspended creation, the
attribute list, the job, the standard command-line quoting and the
environment block); `crates/agentd/src/owner.rs` and `supervisor.rs` are
one source for every platform, over `agentdocker_host::ipc` (the owner's
endpoint is `session::endpoint`: the socket on Unix, a private pipe named
for the home and the agent on Windows, the lock and the exit file still
files in the sessions directory); `crates/cli/src/attach.rs` with
`attach/input_windows.rs` (a console reader thread, cancelled on drop,
handing UTF-16 keystrokes on as UTF-8; the window size looked at four
times a second in place of `SIGWINCH`).

What the smoke checks for it, on every platform and on the Windows runner:
a piped managed command runs under an owner and its output reaches its
log; a terminal command runs on its own console, what is typed through
the attach wire reaches it and its answer comes back on the screen and
into its log; `stop` ends a terminal session through its owner; a session
outlives a daemon that is ended abruptly, as a crash would end it (a
deliberate `daemon stop` stops managed sessions first, by design), the
next daemon finds it running under its owner, and stops it through that
owner. What the smoke cannot check: `attach`
from a real console (it has none; the console modes, the reader thread
and the size polling wait for a person at a Windows terminal), and a
provider's own tool under a pseudo console.

The lifetime follow-up explicitly closes ConPTY after the child and its descendants
exit, on a blocking worker while the output pump continues to drain. The pump can
hold the shared terminal until EOF without keeping its own write end alive.
This follows the [native close contract](https://learn.microsoft.com/en-us/windows/console/closepseudoconsole),
including older Windows versions where close waits for output drainage. The
exit report is flushed before a same-directory write-through move on Windows;
it no longer tries to open a Windows directory as a plain file for `sync_all`.
The regression smoke demands an `End` frame and final unterminated log line,
kills a piped session owner and checks the child and grandchild through retained
process handles, and fills a nonreading console's input before requiring stop
within ten seconds. Its async runtime has one worker; wire writes are bounded.
All fallible process/job handle clones are acquired before the suspended child
is resumed. Test teardown waits for exit after requesting stop (`stopping`
is still live), then uses force-stop and verified owner process handles as
fallbacks. Daemon stop must succeed and leave no answering endpoint before
private state is removed. The disown test owns its sole process directly.
The `b24f983d` local gate passed 1,331 Rust tests (7 skipped) and 104 Python
checks (1 skipped), packaging and release build; the portable daemon smoke
passed 24 steps on macOS. At that revision native acceptance was still pending;
cross-compilation and macOS execution do not establish it. The first follow-up
Windows run passed all 281 core/host tests, including console closure and disown,
but the test client timed out writing an inspection request over its synchronous
named-pipe handle. That failed run remains in the existing verification record;
the test client now uses bounded overlapped read/write operations and waits
for cancellation before releasing native I/O storage. That rerun passed transport and attach, but exposed a real terminal launch
defect: without `STARTF_USESTDHANDLES`, Windows copied the detached owner's
redirected handles into its ConPTY child, causing immediate input EOF and
invisible output. The correction supplies explicit null standard handles for
ConPTY, following the [Microsoft Terminal implementation](https://github.com/microsoft/terminal/discussions/15814).
The console's startup pipe ends also remain open until child creation completes,
as required by the [ConPTY startup sequence](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session).
A native regression launches a detached parent with redirected streams, then
checks its child's three console handles, real keyboard input, stdout/stderr,
exit and output EOF under bounded deadlines. Native run `35651993280` on
`dcceff2d` passed that regression and the terminal echo, final output, owner-death
and full-input stop trials (stop completed in 2.375 seconds). It then failed
crash recovery because SQLite had created its WAL with Administrators as owner.
That failed run remains recorded. Follow-up `cadf3d58` passed the entire
50-step smoke, including crash recovery and terminal survival under its owner.

The storage correction installs a permanent `CreateFileW` override in SQLite's
win32 VFS before any connections or worker threads start. New database, WAL,
journal and shared-memory files receive an explicit current-user owner and
protected user/SYSTEM DACL, including when SQLite recreates them. It preserves
access, sharing, creation disposition, inheritance and Win32 error results;
existing owners and ACLs are not adopted or rewritten. The initializer refuses
an unsupported VFS and caches failure. Both binaries, all store entry points
and raw test fixtures use the same initialization gate; library embedders must
initialize before using raw SQLite connections. The native smoke checks DB,
WAL and SHM ownership/protection before the crash, after it, and after recovery.
Native `cadf3d58` passed all nine DB/WAL/SHM ownership/protection assertions
and recovered the original running terminal after the daemon crash. The broader daemon/CLI
test fixtures still contain Unix-only APIs and do not yet compile as native Windows tests.

The desktop terminal pane already uses the shared blocking IPC transport,
including Windows named pipes. Native graphical acceptance of its rendering,
keyboard input, resize and lifecycle still needs a packaged Windows trial.
Desktop startup now secures the state home before creating its autostart lock,
and CLI/GUI sibling lookup uses the platform executable suffix. These fix
first-run ownership and `.exe` lookup. The Windows workflow now links all three
executables and runs an opt-in `--desktop` trial: a source-built window starts its
own private daemon from an absent home, connects, renders a PNG and exits under
bounded deadlines. The test retains the GUI result/log/capture and verifies that
its private daemon remains reachable before cleanup. Capture uses private
scratch outside the fresh home, then exports after the owned window exits;
reusing an old capture destination is refused. On `cadf3d58` the native window
connected and exited in 3.61 seconds with 16 runtime rows and a 48,216-byte PNG.
The retained image has an unlabeled Inbox/Messages navigation row. Review
traced this to capture ordering: a snapshot changes Inbox to Messages and
screenshot reads the previous primitives before their text is redrawn. Default
capture now waits 400 ms after readiness, matching the existing scenario capture
settling. Final code `e3ada9db` repeated all 284 tests and 50 steps successfully
in run `35657437544`; the inspected PNG includes Messages. Startup/capture took
4.27 seconds and full-input stop took 2.062 seconds on that repeat. This bounded startup pass does not establish physical input, broad
visual acceptance, installation, real-provider or clean-machine behavior.
Opening an external project terminal or focusing an external agent terminal is
not implemented on Windows. Provider inventory, services and the desktop
installer remain outside this slice.

The opt-in `AGENTDOCKER_STARTUP_TRACE=1` diagnostics distinguish home security,
database-file protection, SQLite connection, compatibility checks, WAL setup,
schema creation, migrations, search indexes and state restoration. They emit
only fixed stage labels, PID and elapsed time, and are silent by default.
Extracted-package runs 35932937310 (OWNER RIGHTS home) and 35935871942 (ordinary
fresh home) stopped after coordinator-lock readiness. The cause remains unknown;
finer stages and later passes do not establish a fix or justify a longer timeout.
Failed packages and original reports remain retained.

A manual `windows.yml` dispatch can select `startup_samples` (0, 5, 10 or 20) to
sample each ordinary and OWNER RIGHTS ancestry using distinct fresh homes on the
same extracted package. It stops at the first failed assertion, preserves each
home's bounded startup log and keeps the existing startup deadline. A passing
series does not erase earlier failures. Ordinary CI and release runs keep zero
additional samples unless explicitly selected.
Additional-sample dispatches have a 100-minute fixture-step budget and a
180-minute job budget to cover all bounded commands, output capture and cleanup;
ordinary runs retain their 10-minute step and 60-minute job budgets. The daemon's
ten-second readiness deadline is unchanged.

The 40-sample native run on `0184e1d8` passed all 142 checks, with a slowest fresh
start of 4.203 seconds and a schema phase of 2.991 seconds. An earlier traced
fresh start spent 4.137 seconds in that phase. Initial schema creation now uses
one transaction with the same `FULL` durability, avoiding a separate commit for
each new table/index. A late-schema-error regression verifies rollback, retained
existing data and successful initialization after repairing the fixture. A separate native
run on `89400171` passed another 40 samples and all 142 checks: median startup
188 ms, maximum 219 ms; median schema phase 39 ms, maximum 62 ms. The earlier
run had median startup 375 ms and median schema phase 216.5 ms. These runs used
different CI hosts and do not establish a fix for the historical ten-second
failures.

## Local connection boundary

The shared IPC layer uses Unix sockets on macOS/Linux and named pipes on
Windows. Windows pipe creation supplies a protected user/SYSTEM DACL, reserves
the first instance, and rejects remote clients. Clients check the server process's
user and reject untrusted read grants on the pipe before sending application data.
Servers identify clients through the connected pipe's token: a desktop process
cannot necessarily open its own user's SSH process across Windows logon sessions.
The synchronous identity query admits only the current user's SID, refuses an
existing thread token and restores the thread before returning on success or
failure. A failure to restore terminates the process. No application request or
async suspension runs under a client token; clients explicitly set
`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, making identification available
before their first write without granting impersonation privileges. The smoke
client uses the same flags; Windows' default context is unavailable before input.
No debug privilege is enabled. This is a same-user,
per-host boundary across that user's local logon sessions. Other machine
administrators remain privileged.

Active Windows connections are limited to 254 while one instance waits for the
next connection; admission waits instead of exhausting the OS's 255-instance
limit. Desktop workers use overlapped I/O with read/write deadlines and shared
cancellation for terminal clones. Named pipes do not provide stream half-close:
explicit shutdown closes the whole Windows connection. The native tests cover
transfer beyond the pipe buffer, name ownership, cancellation-safe admission,
broad-read ACL refusal, desktop read deadlines and connection cancellation.

The implementation follows [Microsoft's pipe security model](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
and the [Tokio named-pipe API](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/struct.ServerOptions.html).

Policy reload uses the volume serial number and 128-bit native file identifier,
plus file change metadata, before reusing a previous policy. Last-write time
and length alone missed an equal-size replacement in native Windows CI. Reads
open regular files without following a final reparse point and compare the
handle's stamp before and after the bounded read. The replacement regression
sets identical last-write times explicitly; a timing delay is not its fix.

## Provider setup and message acceptance

The native Codex draft adds `scripts/windows_native_codex_smoke.py` to extracted
archive acceptance. It launches the actual pinned Codex 0.155.1 TUI in ConPTY
with a private profile and loopback Responses fixture. Assertions cover real
SessionStart bootstrap, idle wake, an unsent draft, FIFO input during a held
provider request, exact receipts, read-only recovery preview and automatic
receiver replacement without replay. The workflow checks the provider download's
SHA-256 and retains its version, binary/driver hashes, logs and result. Its
fixture process selects its own user as the default object owner before creating
files, matching ordinary desktop ownership on elevated CI; no existing file ACL
or saved provider configuration is changed. The first native run passed all 62 existing package checks, then exposed an
immediate-ping race in the new harness before Codex started. The harness now
observes its original daemon within the existing ten-second bound. The second
run reached readiness in 0.81 seconds, then exposed a separate harness error:
it parsed plain-text CLI `ping` output as JSON. Only the JSON recovery preview
now receives JSON parsing, and the restart predicate waits through a missing
binding instead of treating that transient state as a replacement. The corrected
Windows run reached the actual TUI, then failed zero-prompt startup after 45
seconds with no receiver, hook or model request observed and clean cleanup.
The workflow retains that strict failure and separately runs the explicit
`established` scenario, which begins one fixture prompt before testing delivery.
A narrower pass cannot change the failed startup result or make that workflow
pass. Its first established-session run reached SessionStart and one model
response, then exposed a missing Windows host primitive: querying another
process's loaded executable still returned the unsupported-platform error.
Windows now reads that image with `QueryFullProcessImageNameW` through an owned
process handle, checks liveness before and after, preserves UTF-16 paths and
refuses failed/truncated queries. Existing provider birth and ancestry checks
remain required. A native regression covers a separate executable in a path
with spaces and Unicode, plus invalid and exited processes. All 314 native
core/host tests passed. The `c479a014` extracted-package diagnostic then passed
after an explicit initial prompt: idle delivery, draft preservation, busy FIFO,
five exact receipts and receiver replacement without replay, with clean cleanup.
The separate zero-prompt startup trial still failed, leaving the overall workflow
failed and lifecycle acceptance open. Separate Mac trials on the same receiver
source passed delivery after an initial prompt but failed fresh startup and
reopen without a prompt; both complete trial failures remain recorded. It does not establish real-account, physical keyboard or service
acceptance; the held HTTP response is not a tool or permission wait.

Setup publishes flushed receipts and configuration files through the host's
native helper (write-through moves on Windows; rename and directory sync on
Unix). Undo uses native deletion without a directory-file open. Receipt staging
uses exclusive current-user-owned private-state creation, including elevated
runs. Executable recognition accepts the native `.exe` suffix; bundled skill
frontmatter accepts LF and CRLF. These correct failures observed on the physical
Windows test host; original failed runs remain in the verification index.

Actual Claude 2.1.280 configuration acceptance on `d736d483` passed preview,
selected-profile registration, health, exact undo, changed-entry refusal and
malformed-state refusal. User configuration was unchanged and scratch removed.

The later `137547c0` CI package passed 61 native daemon/CLI/ConPTY/window checks,
including manual/channel MCP startup and a real managed SessionStart that binds
the provider session without changing its agent ID, PID or owner. Native Windows
ancestry inspection and root-only verified binding allow lifecycle hooks to
recover automatic channel receipts; child hooks cannot bind or acknowledge the
root session.

Using that same package, actual Claude 2.1.280 passed four correlated replies
with automatic durable receipts while explicit ACK tools were unavailable. The
trial preserved an unsent terminal draft, queued a message during a real
`sleep 8` tool call, and received its reply after the tool. Owned processes ended
and user configuration hashes were unchanged. It reused existing account
authentication in a private profile; it does not establish fresh-account
onboarding, physical keyboard input, sustained use, or final hosted-package acceptance. Source and archive pins are in the
[verification index](verification/INDEX.md) and [#241 evidence](https://github.com/brandopakel/AgentDocker/pull/241#issuecomment-5787871075).

An additional actual-provider trial on those bytes queued a message, restarted
the private daemon and used the ordinary CLI reconnect command. Agent identity,
conversation and queued ID survived; the queued message and subsequent idle
message both received correlated replies and automatic receipts. Claude required
local-development channel consent again. The scheduled task was removed and no
owned processes remained. This is bounded restart/reopen evidence, not reboot
or multi-day acceptance.


For intermittent first-start investigation, `AGENTDOCKER_STARTUP_TRACE=1` adds
fixed startup-stage labels, the process ID and elapsed milliseconds to stderr,
including stages before the normal logger starts. It emits no command arguments,
environment values or state contents, and does not extend startup deadlines.
The native acceptance driver enables it and retains a bounded 16 KiB log tail
from every owned home before cleanup. Failed CI packages are retained separately
as `windows-failed-package-diagnostics`, never as an accepted preview. The
OWNER RIGHTS fresh-home timeout in run35821898700 remains an unresolved failure;
added diagnostics and any later pass alone do not establish its cause or a fix.

The native desktop installer candidate has passed isolated native lifecycle
acceptance; hosted and physical acceptance remain open. Its private atomic
activation record and receipt-checked launcher resolver reject malformed
records, modified launchers, missing activation and escaped payloads. Bootstrap
entrypoints select and pin the immutable executable, inherit arguments/stdio and
wait for its exit; Windows advertises launcher contract 2 to distinguish this
from the earlier Mac-only contract. The process-parent lookup unwraps only a live bootstrap whose kernel image,
receipt, private store and process births agree; caller-supplied environment
variables do not select an identity. The kernel image query is the existing
`3c95ab3e` prerequisite also used by the native Codex candidate. Native original-parent acceptance passed in run 36970015868 on `9b79675b`
(317 core/host tests, eight skipped, zero retries). The native CLI now wires
`desktop install`, `status` and `rollback` to `%LOCALAPPDATA%/AgentDocker/desktop`
(or a private `--prefix`) with explicit `--local-preview`, payload/schema and
preview guards. Stable bootstrap bytes and their receipt survive activation;
provider and service registrations use those stable paths. Native run 36971473270 on `f0426f72` passed 14 private-prefix installation/
rollback checks plus 62 portable checks on the extracted binaries. The second
payload changed only a fixture README: this proves activation/rollback mechanics,
not a second hosted release. Modified bootstrap/retained bytes were refused and
scratch was removed. The updater now accepts unsigned Windows preview feeds
only, verifies archive bytes and the payload source/schema, and extracts only
the seven exact portable files with bounded expansion and no links. Native preview-feed run 36976883757 on `8ae2a50c` also passed its
local-fixture update/rollback, wrong-checksum and stable-policy refusal checks.
This is not a hosted Windows upgrade. Maintenance now plans and guards removal
of verified inactive versions, retaining active/rollback/held versions and stopped
service references. Uninstall reserves bounded private retirement storage,
publishes an inactive record, then renames only receipt-verified launchers out of
the public directory. A hash receipt precedes each retirement; loaded images
finish without deletion and later activation/prune/uninstall collects closed
images. Unexpected or changed content is preserved; eight directories and 256 MiB
limit new retirement. Settings and running immutable releases are retained. An interrupted cleanup is resumable
from the portable CLI. Native maintenance/service interlocks and the isolated
installed lifecycle passed on `e190d919`; Start menu/PATH, hosted upgrades and
physical login/reboot acceptance remain open.

An initial Mac concurrency regression found that strict private-file reads
rejected an opened handle unlinked by record replacement. A separate read-only
snapshot API retains owner/ACL/type and hard-link checks while accepting that
zero-link handle. Seven boundary tests and six existing state-directory tests
pass locally. Native Windows run 36968029285 on `43e3b384` passed 314 tests but
failed concurrent publication: `MoveFileEx` returned access denied while readers
held the target. The failure is retained. A dedicated snapshot publisher now
uses documented POSIX rename semantics for open readers; native acceptance of
that change and the bootstrap passed within run 36968759795 on `75013c48`. The
run still failed two lifetime-pin fixtures that constructed the old Unix layout
on Windows; those fixtures now exercise the native store. Native run 36970015868 on `9b79675b` passed all 317 core/host tests and
the extracted portable checks. The later `f0426f72` installer trial passed its separate 14 checks; updater
acceptance remains separate from that source.

Installed service acceptance on `3fbbd8c2` exposed a DOS-versus-verbatim path mismatch: the active CLI was falsely classified as obsolete during registration. Registration now compares canonical filesystem paths, still checks the active release and verified bootstrap receipt, and includes native path-spelling and actual copied-process regressions. The original failed trial is retained. Native `2bc7b2e6` passed all 13 installed Task Scheduler checks; loaded-bootstrap uninstall subsequently failed.

The Windows CLI installation candidate also stages a separate preview update
channel and refuses publication unless both installation and installed service
acceptance pass on the exact archive. `91dd78bd` passed native host registration
regressions but run 36980029481 failed at portable `daemon install`: the bounded
PowerShell command timed out after 20 seconds before installed tests began.
This does not establish installed-service or uninstall acceptance. The original
failed service report and full native log remain retained; the timeout cause is
still open.

Native loaded-image regression `a978d269` isolated access denied to
`FileDispositionInfoEx` after the DELETE-capable open succeeded. The candidate
now uses private rename retirement and ordinary collection after image closure.
Exact-head run 37069489729 on `e190d919` passed 319 core/host tests, 62 portable
checks, 33 installer checks, 11 portable Task Scheduler checks and 13 installed
Task Scheduler checks. Loaded-bootstrap uninstall, collection after reinstall
and bounded retirement refusal passed; hashes match the clean native build,
and both service fixtures removed owned tasks/scratch without cleanup errors.
No hosted Windows update, physical console or login/reboot result is implied.

The native Codex candidate now includes these installer/current-main prerequisites
and tests pinned Codex 0.160.0 by default, with an explicit 0.155.1 workflow option.
Both downloads require their recorded SHA-256. Its private fixture also collects
detached 0.160 app-server processes by kernel executable path and process birth,
and retains bounded provider logs. The earlier zero-prompt startup failure is
still open; this version refresh alone is not native acceptance.
Native `2d88d19f` on Codex 0.160.0 still failed zero-prompt startup with no model
request. The established-session diagnostic bound and delivered four queued
inputs, then its ordinary Python ledger read returned permission denied during
delivery; the retained ledger later contained all four exact receipts. This is
consistent with a publication/read-sharing race, but does not prove data loss.
Both fixture cleanups succeeded. The candidate now publishes native ledgers
through the existing atomic snapshot API, permits verified concurrent readback,
and gives the fixture Windows read/write/delete sharing. A held-reader regression
checks old/new complete records. Corrected native acceptance remains required;
no timeout or private-file ownership check was relaxed.
The first corrected native run (`70120fdd`) stopped earlier in an unchanged
accounting fixture: a valid cooperative `Budget` return was mistaken for a
required single-pass `Complete`. The fixture now resumes its two records within
three bounded passes and verifies progress, identical counters and prefix proof.
Product scan deadlines are unchanged; native delivery acceptance is still open.
Native `68e020ba` then passed all 322 core/host checks and progressed through
five exact delivery receipts, preserved drafts, receiver replacement without
replay, and recovery preview in its established-session diagnostic. That trial
still failed its final byte-for-byte fixture-profile assertion; it is not an
overall pass. The fixture now retains its two synthetic configuration files'
before/after bytes and hashes to diagnose the mutation without relaxing that
assertion. Strict zero-prompt startup still timed out. Codex 0.160.0 source queues
SessionStart on session creation and executes it in turn processing, explaining
why the hook alone cannot provide startup-before-first-turn registration. A
verified alternative startup route remains required.
The `5daffdfd` diagnostic identified the profile difference in both strict and
established controls: Codex normalized TOML line endings and persisted only
`tui.screen_reader_detection_done` plus its `gpt-6.1-sol` introduction counter.
The hook file was identical. The fixture now starts with LF and those observed
0.160.0 TUI defaults, retaining its strict final byte assertion. This does not
establish physical screen-reader acceptance or fix pre-first-turn registration.
The `4c1c2777` trial retained all five established-session receipts, draft and
restart checks, but failed the same strict profile assertion: the introduction
counter advanced from one to two. Pinned 0.160.0 source confirms it increments
on startup until four displays, unless TUI tooltips are disabled. The isolated
fixture now disables that tooltip; it still requires byte-identical configuration
and hooks after delivery. Corrected acceptance remains pending. The provider's
MCP initialization supplies client capabilities/version, without a root thread
identity, so it does not provide an alternate zero-prompt binding by itself.

The native workflow accepts `capability_only=true` for a separate bounded Codex
0.160.0 ConPTY experiment. Exact `170884eb` passed all eight checks: a restricted
fresh profile and loopback model, wrong-token refusal on a dedicated authenticated
server, actual native TUI initialization before the observer, sole empty-thread
identity before any prompt, first queued delivery, preserved draft submitted
once, and unchanged private configuration. Full history proved two exact user
receipts. One observed empty-rollout materialization read was retried within the
fixture's explicit bound; input submission was never retried. All captured
fixture processes and scratch were retired, with no cleanup/reader errors.

This is provider capability evidence. It creates no AgentDocker binding and
uses no account, physical input or permission request. Product launch/bootstrap,
process-generation ownership, shared transport and restart/adoption remain. The
provider API's `vscode` source label is not process identity. Earlier unsupported
history and transient-read failures remain in the verification index; later
passes do not erase them or replace the strict product startup gate.

Installer review corrections clean update extractions, preserve inactive releases
on idempotent install, compare canonical daemon paths and retain unrecognized
retirement directories while collecting verified siblings. Uninstall refuses
before deactivation when a preserved directory cannot be accounted for.
A completed activation remains successful if extraction cleanup fails: the CLI
report and desktop identify the retained path/error. Failed operations preserve
their original error, and later attempts use fresh extraction directories.

Exact-head `f155249d` passed 322 native core/host, 83 portable, 43 installer,
11 portable-service and 13 installed-service checks. This includes an actual
update under a deliberately held directory handle: activation succeeds, the
cleanup diagnostic identifies sharing violation 32, and the fixture closes its
own handle and removes its extraction. Source/tree/binary hashes match; owned
tasks and scratch were removed with zero service cleanup errors. Full Mac/Linux
gates passed 1,465/1,453 Rust and 169 Python tests each (one skipped), zero retries.
The earlier fixture failures are retained in the verification index.
Integration with accepted growing-source accounting requires final checks and
follow-up review; hosted distribution, physical and actual-provider/logon/reboot
acceptance remain open.

The provider-only remote-TUI probe has an optional private command-approval
scenario. It installs one prompt rule in its disposable profile, requires the
native terminal to display and hold the print command, and supplies a single
synthetic Return before checking the result and retained draft. The observing
WebSocket client never answers an approval. Exact `9af3a42c` passed all twelve
checks, with two exact input receipts, unchanged configuration/rule and complete
owned process, reader and scratch cleanup. Cleanup explicitly cancels private
PTY I/O and shuts down pywinpty's forwarding socket before joining both readers;
the earlier `ceaff20a` cleanup failure remains in the verification index. This
is provider capability evidence, not AgentDocker receiver integration, a real
provider account or a human permission interaction.
