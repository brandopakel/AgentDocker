# Native Windows

Windows is a native platform alongside macOS and Linux. It is distributed as an
unsigned x64 portable preview, and the preview's own CLI installs it per user.
It does not use WSL, a browser server or a required container engine to
substitute for native execution. Windows ARM64 is not supported. Acceptance
follows the same [first-run acceptance](LOCAL-TRIAL.md#stage-5--other-machines-and-systems)
as the other platforms, and a cross-compile alone is not runtime acceptance.

The base is core and host I/O: full-resolution process identities, same-user
process inventory without reading environments or requesting extra privileges,
nonblocking exclusive file locks, explicit protected user/SYSTEM ACLs, local
named-pipe names, bounded file observations, and Job Object command ownership.
State creation checks existing ancestry without changing its ACLs, refuses
foreign write/delete access and reparse points, and retains directory handles
while creating state. Existing owned broad-read state can be narrowed;
foreign-writable state is refused. Administrators and SYSTEM remain machine
administrators, as root does on Unix.

File observations on Windows track native read-only attributes and change
metadata; Windows has no Unix executable permission bits. Captured Windows
container build inputs normalize files to 644, or 444 when read-only. An image
recipe must set container executable bits explicitly. Reparse points are not
accepted as ordinary files. Engine workspace transport is unavailable on
Windows.

## Not supported on Windows

- Live daemon reload and the descriptor handover (`daemon reload`): the daemon
  holds no descriptors a successor could inherit; stop and start it. The command
  refuses in words rather than hanging.
- Container workspace transport and grants: the endpoint is a Unix socket.
- Opening an external project terminal or focusing an external agent terminal.
- Start menu and `PATH` integration, and signed packages.
- Startup registration of a Codex session started directly, without a first
  prompt: the provider queues `SessionStart` on session creation and runs it
  during turn processing, and its MCP initialization carries no root thread
  identity. The product launcher (`codex-native`) supplies birth and binding.
- Native Codex launch and managed secret input are experimental and stay behind
  their gates.
- A validation command that times out is ended, but only the command itself: no
  process group or Job Object surrounds it, so whether its descendants survived
  is not reported.
- A batch launcher whose session checkout is on a network share (a UNC path) is
  refused before anything is created (see
  [provider launchers](#provider-launchers)).

## Portable desktop archive

Windows x64 desktop packaging produces an unsigned ZIP containing the three
`.exe` files together, exact source/build and executable hashes, licenses and
opening instructions. The Windows workflow builds release binaries, checks PE
architecture, and runs its full daemon/CLI/terminal/desktop trial only after
extracting that ZIP outside the checkout into a path containing spaces and
Unicode. Prerelease tags publish the accepted portable Windows target
separately from the macOS/Linux update feeds. See
[distribution setup](DISTRIBUTION-SETUP.md#windows-portable-preview).

## The daemon and the CLI

`agentdocker` builds natively for Windows, and is the daemon when invoked as
`agentd.exe` (a byte-identical copy beside it in the portable folder and
installed versions, since a ZIP holds no links) or as
`agentdocker.exe daemon run`. The daemon serves the
shared named-pipe transport (`agentdocker_host::ipc`, the same listener and
stream the Unix socket uses), and the CLI registers, lists, sends, reads, claims
and stops through it. A client with nothing to talk to starts the daemon and
waits for it to listen: ten seconds on Windows, three elsewhere. A plain `ping`
on a home no daemon has made creates the home and starts a daemon.

What the shared code does on every platform:

- Ending a process goes through `agentdocker_host::procinfo::end(pid,
  started_at, force)`: the recorded birth is compared before anything is
  sent. On Unix that is `SIGTERM`/`SIGKILL`; on Windows the birth is read from
  the very handle that is then terminated, so the check and the act cannot
  straddle a pid recycling. Liveness is `procinfo::alive` on both (an access
  refusal is a yes, as with `kill(pid, 0)`). The daemon's stop path keeps its
  record checks.
- A file's identity across renames is `agentdocker_host::files::identity`:
  device and inode on Unix, volume serial number and file index on Windows,
  read from the open handle. The channel-receipt cursor stores it.
- The hook adapter's bounded stdin read and deadline-bound stdout write use a
  helper thread on Windows, where a console or pipe handle cannot be polled; the
  deadline is the same, and nothing partial is acknowledged.
- Private state reads (`read_private_file`, `check_private_dir`,
  `open_private`) open read-only with the kind and the ACL checked and nothing
  narrowed: a read is never a write.
- A daemon the CLI starts on demand does not hold the client's standard handles.
  On Windows a child inherits every inheritable handle of its parent, and a
  script's or shell's capture of `agentdocker` output is such a handle, so the
  daemon would keep the capture open for as long as it ran. `command::detach`
  makes the client's standard handles non-inheritable before the daemon is
  started; the daemon gets its own stdio from the command.
- Both binaries do their work on a thread with a 32 MiB stack. A Windows main
  thread has 1 MiB (Unix has 8), and clap's derived parser for this many
  commands and the one future behind every command overflow it in a debug build.
  The reservation is address space until touched.
- A process's loaded executable is read with `QueryFullProcessImageNameW`
  through an owned process handle. Liveness is checked before and after, UTF-16
  paths are preserved and a failed or truncated query is refused. Provider birth
  and ancestry checks remain required.

### State ownership

The daemon creates its home itself, as it does on a person's first run, and
what it creates is owned by the user. Objects an administrator creates on
Windows belong to the Administrators group, not the user. A home made from an
elevated shell is foreign-owned state, and the daemon refuses it by design
(`state or ancestor belongs to an untrusted Windows principal`); a home the
daemon makes under such a directory is refused as writable by another
principal, so nothing the daemon owns sits under one. The refusal names the
path and the principal, and the fix is to let the daemon make the home. A client
starting the daemon on demand makes the home the way the daemon does.

CPython's `os.mkdir(mode=0o700)` gives a directory an explicit DACL of SYSTEM,
Administrators and `OWNER RIGHTS` (S-1-3-4). The check resolves that entry to
the object's already validated owner. It does not trust that SID globally or
change ownership checks: foreign-owned state and other principals' write grants
remain refused.

### Provider launchers

An npm-installed provider is a `.cmd` shim on `PATH` (`claude.cmd`, `codex.cmd`
beside `node.exe`). The launch gate starts one the way a shell and the standard
library do:

- One resolver serves the runtime inventory and the launch
  (`command::find_program`): a bare name is resolved by launcher extension in
  `PATHEXT`'s order, never to a data file, never in the working directory. The
  lookup uses the child's own `PATH` and `PATHEXT`, the command's overrides over
  this process's, matched without case.
- A batch launcher runs under `cmd.exe` from the system directory with the
  standard library's own batch command line (`cmd.exe /e:ON /v:OFF /d /c
  ""script" args…"`) and argument rules: a line-breaking argument is refused
  before anything runs, `%` is neutralised, and arguments are quoted unless made
  of characters cmd leaves alone.
- A canonical `\\?\` path is given as the plain path cmd understands only when
  the plain path names the same file; a verbatim path the plain rules would
  rewrite is refused, never run as another file.
- A relative script is made absolute where it was checked, so `cmd.exe` running
  from the child's directory runs that file and not a namesake there.
- The session's checkout is given to `cmd.exe` as a plain path: a canonical
  Windows path is verbatim, and `cmd.exe` started in one falls back to the
  Windows directory ("UNC paths are not supported"). A checkout on a network
  share is therefore refused for a batch launcher before anything is created.

### Codex queue

The native Codex queue shares the receiver, hook and explicit recovery code with
Unix. Its Windows endpoints are private named pipes, authenticated by the kernel
peer PID and same-user token before protocol input. Process birth, provider
generation, hook ancestry and exact transcript receipts remain required. Windows
uses a host-wide monotonic deadline, file-handle identity and write-through
private ledger publication. Ledgers are published through the atomic snapshot
API, which permits verified concurrent readback.

## Managed sessions on Windows

A managed session on Windows is the same session owner, the same wire and the
same daemon-side controller as on macOS and Linux (`agentdocker_core::session`:
`Activate`, keystrokes, window sizes, output from an offset, the exit
acknowledgement; the owner outliving any daemon), with the platform pieces below
answering what the Unix pieces do. `run`, `run --tty`, `logs`, `stop` and
`attach` work on Windows. The table is what each Unix piece is and what answers
it.

| Unix piece | Where | Windows answer |
| --- | --- | --- |
| The terminal pair: `posix_openpt`, the child gets the slave as its standard streams and controlling terminal | `host::pty::Pty` | A pseudo console: `CreatePseudoConsole` over two pipes; the daemon side reads the output pipe and writes the input pipe; the child is created with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`. There is no slave to hand over: the attribute is what binds the child. |
| The launch gate: fork, hold before exec until the record is durable, `exec denied` on refusal, the pid and birth readable meanwhile | `host::launch::{prepare, Pending, OwnedChild}` | `CreateProcessW` with `CREATE_SUSPENDED` (plus `EXTENDED_STARTUPINFO_PRESENT` for the console attribute): the pid and the birth time are readable from the handle while the first thread has never run; `activate` is `ResumeThread`, refusal is `TerminateProcess` of a process that never executed an instruction. Rust's `Command` cannot carry the attribute list on stable (`raw_attribute` is unstable), so this is a direct `CreateProcessW` with the standard command-line quoting and an environment block built from the launch. |
| The process group the daemon signals: `setsid` in the child, `kill(-pid)` | `take_controlling_terminal`, `OwnedChild::drop`, `group_exists` | A Job Object the owner holds, the child assigned to it before it resumes (`CREATE_SUSPENDED` makes that a certainty, not a race). Stop is `TerminateJobObject`; "does the group still exist" is the job's active process count, which trails an exit by a moment, so the owner asks again rather than concluding from one answer. The owner holds a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: a daemon crash leaves the owner and child intact, while an owner crash ends every descendant even without destructors. Explicit `disown` clears that flag before surrendering ownership and returns an error if the clear fails. A Ctrl-C is queued through the bounded keyboard writer (`\x03`) without blocking the supervisor; a full queue leaves Ctrl-C unqueued but preserves the two-second stop grace; a missing or closed input ends the job immediately. When written into the pseudo console input, the console turns Ctrl-C into the child's `CTRL_C_EVENT`; there is no `SIGTERM`, so the graceful stop is that, then the job after the grace period. The child is not created in a new process group of its own: that flag makes a process ignore Ctrl-C and a child inherits the ignoring, so the owner clears its own inherited ignore before creating the child. A piped child has nothing to be asked with; its polite stop is the end. The daemon's own liveness question about a group (`group_exists`) is answered by the leader's liveness, since only the owner holds the job. |
| Window size: `TIOCSWINSZ` and `SIGWINCH` | `Pty::resize` | `ResizePseudoConsole`; the console tells the child. A mutex protects the handle through resize; close takes it once under the same mutex, then releases the mutex before native teardown. Later resizes fail with a closed-console error. |
| The owner's socket: `<home>/sessions/<agent>.sock`, `0600`, a lock beside it | `owner::serve`, `supervisor::Controller::connect` | The shared named pipe (`agentdocker_host::ipc`, protected DACL, same-user peer check) under a per-agent name derived from the home and the id; the lock stays a file in `sessions/`. |
| The owner survives the daemon: its own session, `SIGHUP` ignored | `owner::main` | Started with `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP` and, if the daemon is ever inside a job, `CREATE_BREAKAWAY_FROM_JOB`; the client's standard handles are non-inheritable before the start (as `command::detach` does for the daemon), so the owner holds none of the daemon's pipes. |
| Liveness and identity of the owner and the child | `procinfo::{alive, start_time, end}` | The handle's creation time, `end` from the very handle it terminates. |
| `attach`: raw mode, the window size, cancellable readiness-driven stdin, `Ctrl-]` | `pty::{RawMode, window_size, nonblocking_input}`, `cli::attach` | `SetConsoleMode`: input without line, echo and processed input, with virtual-terminal input; output with virtual-terminal processing; both restored on drop. The size from `GetConsoleScreenBufferInfo`'s window; a resize seen by polling it, since there is no signal. A console input handle is waitable, so readiness-driven reading is `WaitForMultipleObjects` on it and a cancel event. |
| The log: every byte of output appended | `owner::write_log` | Unchanged: the output pipe's bytes. |

Where it lives: `crates/host/src/pty/windows.rs` (the pseudo console,
`window_size`, `RawMode` over console modes) and
`crates/host/src/launch/windows.rs` (the suspended creation, the attribute list,
the job, the standard command-line quoting and the environment block);
`crates/agentd/src/owner.rs` and `supervisor.rs` are one source for every
platform, over `agentdocker_host::ipc` (the owner's endpoint is
`session::endpoint`: the socket on Unix, a private pipe named for the home and
the agent on Windows, the lock and the exit file still files in the sessions
directory); `crates/cli/src/attach.rs` with `attach/input_windows.rs` (a console
reader thread, cancelled on drop, handing UTF-16 keystrokes on as UTF-8; the
window size looked at four times a second in place of `SIGWINCH`).

A session outlives a daemon that is ended abruptly, as a crash would end it; the
next daemon finds it running under its owner and stops it through that owner. A
deliberate `daemon stop` stops managed sessions first, by design. `attach` needs
a real Windows console for its console modes, reader thread and size polling.

### Terminal lifetime

ConPTY is closed explicitly after the child and its descendants exit, on a
blocking worker while the output pump continues to drain. The pump can hold the
shared terminal until EOF without keeping its own write end alive. This follows
the [native close contract](https://learn.microsoft.com/en-us/windows/console/closepseudoconsole),
including older Windows versions where close waits for output drainage. The exit
report is flushed before a same-directory write-through move; a Windows
directory is not opened as a plain file for `sync_all`. The session owner's async
runtime has one worker, and wire writes are bounded. All fallible process and job
handle clones are acquired before the suspended child is resumed.

Without `STARTF_USESTDHANDLES`, Windows copies the detached owner's redirected
handles into its ConPTY child, causing immediate input EOF and invisible output.
The launch therefore supplies explicit null standard handles for ConPTY,
following the [Microsoft Terminal implementation](https://github.com/microsoft/terminal/discussions/15814).
The console's startup pipe ends remain open until child creation completes, as
required by the [ConPTY startup sequence](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session).
Before a suspended child is activated, its token's default object owner is set
to its own user SID, so files it creates (provider transcripts among them) are
user-owned even under an elevated launcher.

### Storage

SQLite's win32 VFS gets a permanent `CreateFileW` override before any
connections or worker threads start. New database, WAL, journal and
shared-memory files receive an explicit current-user owner and a protected
user/SYSTEM DACL, including when SQLite recreates them. The override preserves
access, sharing, creation disposition, inheritance and Win32 error results;
existing owners and ACLs are not adopted or rewritten. The initializer refuses
an unsupported VFS and caches failure. Both binaries, all store entry points and
raw test fixtures use the same initialization gate; library embedders must
initialize before using raw SQLite connections. Initial schema creation runs in
one transaction with `FULL` durability.

### Desktop

The desktop terminal pane uses the shared blocking IPC transport, including
Windows named pipes. Desktop startup secures the state home before creating its
autostart lock, and CLI/GUI sibling lookup uses the platform executable suffix.
A window started with `--desktop` from an absent home starts its own private
daemon, connects and renders. Screenshot capture uses private scratch outside the
fresh home, waits 400 ms after readiness so a snapshot's navigation text is
redrawn, and exports after the owned window exits; reusing an old capture
destination is refused.

### Startup diagnostics

`AGENTDOCKER_STARTUP_TRACE=1` adds fixed startup-stage labels, the process ID
and elapsed milliseconds to stderr, including stages before the normal logger
starts. The stages cover home security, database-file protection, SQLite
connection, compatibility checks, WAL setup, schema creation, migrations,
search indexes and state restoration. It emits no command arguments,
environment values or state contents, does not extend startup deadlines, and is
silent by default.

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
before their first write without granting impersonation privileges, because
Windows' default context is unavailable before input. No debug privilege is
enabled. This is a same-user, per-host boundary across that user's local logon
sessions. Other machine administrators remain privileged.

Active Windows connections are limited to 254 while one instance waits for the
next connection; admission waits instead of exhausting the OS's 255-instance
limit. Desktop workers use overlapped I/O with read/write deadlines and shared
cancellation for terminal clones. Named pipes do not provide stream half-close:
explicit shutdown closes the whole Windows connection.

The implementation follows [Microsoft's pipe security model](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
and the [Tokio named-pipe API](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/struct.ServerOptions.html).

Policy reload uses the volume serial number and 128-bit native file identifier,
plus file change metadata, before reusing a previous policy, because last-write
time and length alone miss an equal-size replacement. Reads open regular files
without following a final reparse point and compare the handle's stamp before
and after the bounded read.

## Provider setup and message delivery

Setup publishes flushed receipts and configuration files through the host's
native helper (write-through moves on Windows; rename and directory sync on
Unix). Undo uses native deletion without a directory-file open. Receipt staging
uses exclusive current-user-owned private-state creation, including in elevated
runs. Executable recognition accepts the native `.exe` suffix; bundled skill
frontmatter accepts LF and CRLF.

Managed Claude sessions use a real `SessionStart` that binds the provider
session without changing its agent ID, PID or owner. Native Windows ancestry
inspection and root-only verified binding let lifecycle hooks recover automatic
channel receipts; child hooks cannot bind or acknowledge the root session.

The product Codex launcher (`codex-native`) starts the provider in ConPTY with a
private shared-server receiver. It records birth and binding before input, owns
its exact child, delivers queued input with exact receipts, and replaces a
receiver without replay. While the TUI stays alive, an unsent draft is preserved
across delivery and receiver replacement. On native exit it revokes its
capability token and leaves provider configuration unchanged. It reopens an
explicit persisted UUID with the canonical identity and prior receipts; the
submitted history survives reopen, and input queued while the provider is down is
delivered after reopen. Launch and resume preserve the queue and receipts, not
unsent editor text. A deliberately invalid executable is refused before the first
launch and its capability file is removed without a model request. The launcher
ends its owned server by native process termination, not graceful
server-protocol shutdown.

## Diagnostic recipes

The Windows workflow (`.github/workflows/windows.yml`) is dispatched manually
with these inputs; each one narrows or extends what the native run exercises:

- `startup_samples` (`0`, `5`, `10` or `20`; default `0`) adds that many fresh
  homes per ancestry type (ordinary and OWNER RIGHTS) on the same extracted
  package. A series stops at the first failed assertion and keeps each home's
  bounded startup log. The startup deadline is unchanged, and ordinary runs take
  no additional samples. A run with samples has a longer job budget than an
  ordinary one.
- `service_acceptance` exercises the extracted daemon and connector Task Scheduler
  lifecycles (install, stop, start, restart, crash recovery, uninstall) on one
  random private home and its owned task, with client autostart disabled and
  cleanup required. It installs `psutil` and passes `--service` to the package
  smoke.
- `hosted_tag` and `hosted_source`, with optional `baseline_tag` and
  `baseline_source`, test a published preview (see
  [distribution setup](DISTRIBUTION-SETUP.md#windows-portable-preview)).
- `console_input_probe` diagnoses only raw Ctrl-] and advertised Win32 console
  input.
- `capability_only` runs a provider-only authenticated remote ConPTY probe with
  a restricted fresh profile and loopback model; it creates no AgentDocker
  binding. `capability_approval` adds one private command approval through the
  native UI. `receiver_acceptance` additionally builds the candidate CLI and
  daemon and exercises the shared-server receiver after one explicit fixture
  turn. `witnessed_birth` initializes the observer before the sole native TUI,
  requires an empty server and that thread-start notification, and tests first
  queued input without the warmup turn. `automatic_launcher` runs the
  product-owned `codex-native` launcher instead of fixture binding; it needs
  `capability_only` and `receiver_acceptance`.
- `codex_version` selects among the workflow's pinned provider downloads, each
  verified against a recorded SHA-256.

`scripts/windows_package_smoke.py` runs against the extracted archive. It takes
`--native-manifest` and `--output` (both required) and:

- `--codex <exe>` enables private ConPTY/loopback acceptance with the actual
  native Codex executable. `--codex-scenario` selects one of three:
  `automatic` tests the product-owned launcher; `startup` (the default) is the
  strict direct-Codex diagnostic that begins with no prompt; `established` begins
  with one fixture prompt before testing delivery, draft preservation, busy
  FIFO input, exact receipts and receiver replacement. A narrower scenario never
  changes the strict `startup` outcome.
- `--queue-recovery` (requires `--codex` with `--codex-scenario automatic`) runs
  three private ConPTY scenarios, selected for the launcher driver as
  `--queued-recovery normal`, `holds` and `client-reply-loss`:
  - `normal`: explicit queued-entry recovery after a deliberate interruption.
    Recovery requires distinct confirmation, action and order, starts one
    existing entry and treats a same-intent retry as the same entry.
  - `holds`: refusal during provider-rate and project holds, before any intent,
    with the preview still readable.
  - `client-reply-loss`: the local client sends the start request over the
    private resolve named pipe and is closed without reading the reply, once the
    durable intent exists. The intent is retained and the entry is started once;
    a repeat start is reported as already attempted or already delivered and
    nothing is started twice.
  Each scenario retains the next entry for 30 seconds.
- `--startup-samples N` (0 to 20) and `--service` are the script's forms of the
  inputs above.
- `--mcp-forms`, `--mcp-urls`, `--mcp-idle` and `--managed-secret` (each
  requires `--codex`) exercise managed Codex form decisions, website decisions
  without opening a browser, those decisions after the model turn ends, and the
  experimental masked input with synthetic ConPTY boundaries.

Failed CI packages are kept separately from the accepted preview artifact.

## Desktop installation

`desktop install`, `status` and `rollback` work on a per-user store at
`%LOCALAPPDATA%/AgentDocker/desktop` (or a private `--prefix`), with explicit
`--local-preview`, payload/schema and preview guards. A private atomic activation
record and a receipt-checked launcher resolver reject malformed records,
modified launchers, missing activation and escaped payloads. Bootstrap
entrypoints select and pin the immutable executable, inherit arguments and
stdio, and wait for its exit; Windows advertises launcher contract 2 to
distinguish this from the Mac-only contract 1. The process-parent lookup unwraps
only a live bootstrap whose kernel image, receipt, private store and process
births agree; caller-supplied environment variables do not select an identity.
Stable bootstrap bytes and their receipt survive activation; provider and
service registrations use those stable paths. Registration compares canonical
filesystem paths, so the DOS and verbatim spellings of one path agree, and still
checks the active release and the verified bootstrap receipt.

Private snapshot records are published by a dedicated snapshot publisher that
uses POSIX rename semantics so concurrent readers holding the target open do not
block publication. A separate read-only snapshot API keeps the owner, ACL, type
and hard-link checks while accepting a handle whose record was unlinked by
replacement.

The updater accepts unsigned Windows preview feeds only, verifies archive bytes
and the payload source/schema, and extracts only the seven exact portable files,
with bounded expansion and no links. A wrong checksum or the stable policy is
refused. Update extractions are cleaned up, inactive releases are preserved on an
idempotent install, and an extraction cleanup failure after a completed
activation leaves the activation successful: the CLI report and desktop identify
the retained path and error. Failed operations preserve their original error,
and later attempts use fresh extraction directories.

Maintenance plans and guards removal of verified inactive versions, retaining
the active, rollback and held versions and stopped-service references. Uninstall
reserves bounded private retirement storage (eight directories, 256 MiB),
publishes an inactive record, then renames only receipt-verified launchers out of
the public directory; a hash receipt precedes each retirement. Loaded images
finish without deletion, and a later activation, prune or uninstall collects
closed images. Unexpected or changed content is preserved, and an uninstall
refuses before deactivation when a preserved directory cannot be accounted for.
Unrecognized retirement directories are retained while verified siblings are
collected. Settings and running immutable releases are retained. An interrupted
cleanup is resumable from the portable CLI.

## Services

Daemon login startup is a limited per-user Task Scheduler task with a private
ownership receipt and a bounded crash supervisor. `daemon install`, `uninstall`,
`start`, `stop`, `restart` and `status` handle that task; `start` and `stop`
operate on demand when no task is installed. See the [user commands](GUIDE.md)
and [service semantics](ARCHITECTURE.md#starting-the-daemon). The package
instructions require finishing managed work, quitting the app, uninstalling an
enabled task with `daemon uninstall` and stopping the daemon before moving or
replacing its folder; extraction alone installs no startup task.

- Lookup selects the exact task from a successful enumeration and propagates
  query errors instead of treating every error as absence. Names are compared
  without regard to case before ownership is checked, so a foreign task under a
  case-variant name makes install and uninstall refuse, with its XML unchanged.
- The crash supervisor restarts a failed native exit after two seconds, at most
  three times; ten stable minutes reset the budget. A clean exit stays stopped,
  and Scheduler-level retries are disabled.
- A failed cleanup retires verified process generations, supervisor first,
  while retaining the failed result and any cleanup errors.
- Scheduler operations stage at most 512 KiB of UTF-8 script in a fresh private
  temporary file directly under the user home, with protected creation and
  ancestry checks. This does not depend on shared Temp permissions or a
  caller-only `TMP` override, and does not create the daemon state home for a
  cold read-only query. A short encoded loader reads the file after its writer is
  closed, avoiding Windows' command-line limit while retaining exact ownership
  checks; the file is removed after the bounded command exits.
- Each daemon or connector Scheduler script allows 60 seconds, including
  PowerShell startup. The command runs once, preserves ownership checks and
  private staging, terminates its owned job on timeout, and never retries a
  possibly committed mutation. Daemon readiness and stop deadlines are separate.
- With `AGENTDOCKER_STARTUP_TRACE=1`, service operations keep a private
  `windows-service-trace.log` with Rust/PowerShell entry/return and task
  lookup/ownership phases. It resets above 64 KiB before another operation,
  excludes arguments and credentials, and creates no missing cold state home.

### Browser connector

`connector install`, `enable` and `uninstall` use a distinct limited per-user
login task. The owned daemon login service is a prerequisite, started through
its own task; the connector task never launches an on-demand daemon.
Registration checks the exact action, ownership marker and current-user
principal before mutation. A private run record pins PID, kernel birth and a new
nonce; uninstall requests that generation's graceful exit and tunnel teardown
before removing its task. The task captures the selected daemon endpoint and
absolute configured paths while stripping session identity. Differently
configured or foreign tasks and browser grants are preserved. A stopped
connector registration protects installed launchers without retaining unrelated
inactive payloads.

The owned task retries failed native exits after two seconds, at most three
times; ten stable minutes reset the budget. A clean exit stays stopped, and
Scheduler-level retries are disabled. Strict setup errors remain fatal, and the
native-command boundary captures ordinary stderr and its actual exit code.
