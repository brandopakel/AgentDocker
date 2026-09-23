# Native Windows delivery work

Windows is an intended native platform. The Windows implementation is an incomplete port and
is not a downloadable Windows product. It does not use WSL, a browser server or
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
acceptance. The public tag workflow still has no Windows target. See
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

What the slice refuses on Windows, in words rather than with a hang or a
crash, and what that means for a person:

- The native Codex queue (`codex-queue`): its hook endpoint is a Unix socket
  checked by peer credentials. The Codex hook adapter sees no receiver and
  takes its ordinary path, so a Codex session on Windows reads its messages
  at its next prompt, never live.
- Live daemon reload and the descriptor handover (`daemon reload`): the
  daemon holds no descriptors a successor could inherit; stop and start it.
- Container workspace transport and grants: the endpoint is a Unix socket.
- The desktop installer (`desktop install`, updates, rollback) and connector
  service remain unavailable. Daemon login startup is now implemented through
  a limited per-user Task Scheduler task, with a private ownership receipt and
  a bounded crash supervisor. `daemon install`, `uninstall`, `start`, `stop`,
  `restart` and `status` handle that task; start/stop still operate on demand
  when no task is installed. Real product lifecycle/provider-survival
  acceptance remains separate from source tests and the isolated supervisor
  prototype. See the [user commands](GUIDE.md) and
  [service semantics](ARCHITECTURE.md#wire-protocol).
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
- Daemon service/session startup, per-user desktop installation, Start menu
  integration, updates/rollback and signed packages.
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
