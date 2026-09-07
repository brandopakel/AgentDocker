# Native Windows delivery work

Windows is an intended native platform. This branch is an incomplete port and
is not a downloadable Windows product. It does not use WSL, a browser server or
a required container engine to substitute for native execution.

The first boundary is core and host I/O: full-resolution process identities,
same-user process inventory without reading environments or requesting extra
privileges, nonblocking exclusive file locks, explicit protected user/SYSTEM
ACLs, local named-pipe names, bounded file observations, and Job Object command
ownership. State creation checks existing ancestry without changing its ACLs,
refuses foreign write/delete access and reparse points, and retains directory
handles while creating state. Existing owned broad-read state can be narrowed;
foreign-writable state is refused. Administrators and SYSTEM remain machine
administrators, as root does on Unix.

The Windows workflow runs these crates on a real Windows runner, including
ACL refusal, process identity and command descendant cancellation. A successful
cross-compile alone is not runtime acceptance. Unix CI remains required.

File observations on Windows track native read-only attributes and change
metadata; Windows has no Unix executable permission bits. Captured Windows
container build inputs normalize files to 644, or 444 when read-only. An image
recipe must set container executable bits explicitly. Reparse points are not
accepted as ordinary files. Engine workspace transport is explicitly unavailable
on Windows until a checked named-pipe/VM transport is implemented.

Work still required before platform support can be claimed:

- Integrate the shared named-pipe listener/clients into a full native daemon.
  Native core/host CI has exercised peer verification, bounded streams,
  admission and desktop cancellation; it does not constitute a running Windows product.
- Native supervised processes, ConPTY terminal input/output/resize, same-user
  identity checks for stopping adopted processes, and restart recovery.
- Windows provider configuration and desktop application inventory.
- Daemon service/session startup, per-user desktop installation, Start menu
  integration, updates/rollback and signed packages.
- Full daemon/CLI tests and native graphical acceptance, followed by a fresh
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
