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

- Private named-pipe listener and clients, peer verification, bounded streams.
- Native supervised processes, ConPTY terminal input/output/resize, same-user
  identity checks for stopping adopted processes, and restart recovery.
- Windows provider configuration and desktop application inventory.
- Daemon service/session startup, per-user desktop installation, Start menu
  integration, updates/rollback and signed packages.
- Full daemon/CLI tests and native graphical acceptance, followed by a fresh
  real-provider integration and sustained lifecycle trials.

The supported download/platform matrix remains unchanged until those acceptance
stages pass. See [native delivery](NATIVE-DELIVERY.md) for the macOS/Linux stack.
