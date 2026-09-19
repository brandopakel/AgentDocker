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
ACL refusal, process identity and command descendant cancellation, and compiles
the UI binary. At main `16bf69a`, it does not build/test the complete daemon or
CLI; the native graphical and release workflows have no Windows target. A
successful cross-compile alone is not runtime acceptance. Unix CI remains required.

File observations on Windows track native read-only attributes and change
metadata; Windows has no Unix executable permission bits. Captured Windows
container build inputs normalize files to 644, or 444 when read-only. An image
recipe must set container executable bits explicitly. Reparse points are not
accepted as ordinary files. Engine workspace transport is explicitly unavailable
on Windows until a checked named-pipe/VM transport is implemented.

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
`windows-daemon-smoke` artifact; the same script runs on macOS and Linux,
so it is checked before the runner sees it — there it ends its private
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
records what a directory made there inherits. What the daemon creates is
owned by the user. The same held
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

- Managed sessions: `run`, `launch` and the desktop's launch answer that
  managed sessions are not available on Windows yet; `attach` says the same.
  Sessions started by the person and reached through hooks and MCP are the
  way in — the `hook claude-code` and `hook codex` adapters and the `mcp`
  server are stdio and the pipe, and run.
- The native Codex queue (`codex-queue`): its hook endpoint is a Unix socket
  checked by peer credentials. The Codex hook adapter sees no receiver and
  takes its ordinary path, so a Codex session on Windows reads its messages
  at its next prompt, never live.
- Live daemon reload and the descriptor handover (`daemon reload`): the
  daemon holds no descriptors a successor could inherit; stop and start it.
- Container workspace transport and grants: the endpoint is a Unix socket.
- The desktop installer (`desktop install`, updates, rollback) and the daemon
  and connector services: `daemon install` and `uninstall` say there is no
  service on Windows yet, a later slice. The subcommands that only speak to
  a daemon still work there: `daemon start` starts one on demand for the
  home, `stop` asks it to exit, `status` reports it (and that no service
  exists), `vacuum` compacts its store, and `reload` carries the daemon's
  own refusal.
- A validation command is ended on a timeout, but only the command itself:
  there is no process group and no Job Object around it yet, so whether its
  descendants survived is not reported.

Work still required before platform support can be claimed:

- ConPTY terminal input/output/resize and a session owner that survives the
  daemon, so managed sessions and `attach` exist; then restart recovery.
- The native Codex queue over the named pipe with the same peer checks.
- Windows provider configuration and desktop application inventory.
- Daemon service/session startup, per-user desktop installation, Start menu
  integration, updates/rollback and signed packages.
- The daemon and CLI test suites on the Windows runner (they still carry
  Unix-only fixtures), native graphical acceptance, then a fresh
  real-provider integration and sustained lifecycle trials.

The supported download/platform matrix remains unchanged until those acceptance
stages pass. See [native delivery](NATIVE-DELIVERY.md) for the macOS/Linux stack.

## Local connection boundary

The shared IPC layer uses Unix sockets on macOS/Linux and named pipes on
Windows. Windows pipe creation supplies a protected user/SYSTEM DACL, reserves
the first instance, and rejects remote clients. Both ends check the peer's user
without impersonation or privilege changes; clients also reject any untrusted
read grant on the pipe before sending application data. This is a same-user,
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
