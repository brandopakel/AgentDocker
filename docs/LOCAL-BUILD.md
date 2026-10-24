# Build and install AgentDocker from source

One command builds the release binaries, packages the native app for the
current platform and installs it for your user, so the next launch runs what you just
built:

```sh
make install
```

Run this from your AgentDocker source checkout. The installed Mac
app prefers `/Applications/AgentDocker.app` when that folder is writable and the
name is available or already managed by this installation. Otherwise it uses
`~/Applications/AgentDocker.app`. The chosen destination persists; when the app
lives in `/Applications`, the per-user path remains as a compatibility link for
existing hooks and MCP settings.

Then quit AgentDocker and open it again. The Dock, Spotlight, the `agentdocker`
and `agentd` commands in `~/.local/bin`, and the MCP entry your agent tools use
all follow one managed pointer, so they switch together.
The real launcher bundle retains `Contents/MacOS/agentdocker`, `agentd` and
`agentdocker-ui` executables for integrations that call them by those names;
`agentd` is `agentdocker` under the daemon's name, a hard link to the one
executable, which is the daemon when invoked as `agentd`
([one executable](ARCHITECTURE.md#one-executable)). The installer copies the
signed payload intact; each entry point redirects to the active immutable release
using installation metadata outside the app bundle. Its launch executable is
`agentdocker-ui`: an executable named only `AgentDocker` would collide with the
CLI path on case-insensitive filesystems and send hooks or MCP into the GUI.

## What `make install` does

1. `scripts/build_storage.py`: the disk-budget preflight from the testing standard.
2. `scripts/build_native.py`: `cargo build --release` of the two executables,
   `agentdocker` and `agentdocker-ui`, with `agentd` made a hard link to
   `agentdocker` beside them (`scripts/daemon_link.py`), bound to the exact
   source inputs in a `native-build.json` manifest.
3. `packaging/desktop/package.py`: the same app bundle (macOS) or desktop
   payload (Linux) the release pipeline produces, ad-hoc signed, with licenses,
   into `artifacts/local.noindex/desktop-<timestamp>/`, `agentd` in it a hard
   link to `agentdocker`. `artifacts/local.noindex/latest` points at the newest
   one. Its `manifest.json` reports the payload's size with the link counted
   once. Spotlight skips a `.noindex` folder, and the package is unregistered
   from Launch Services after packaging and after installing, so
   `open -a AgentDocker`, Spotlight and the Dock open the installed app, never
   a package in the checkout.
4. `agentdocker desktop install --from <payload> --local-preview`: the built
   CLI installs its own package for the next launch. Versions are retained;
   `make rollback` activates the previous one; `make status` shows both.

Installation preserves agent processes. Once the release is activated, the
installer asks a running daemon to reload to it and prints the
daemon's answer under `daemon` in its report. With the reload gate on
(`AGENTDOCKER_EXPERIMENTAL_RELOAD=1` in the daemon's environment) the daemon
hands over to a successor and retires the predecessor while agents keep
running; without it, the daemon
refuses, keeps serving the old build, and the report says so until you end
agent work and run `make restart-daemon` (or `agentdocker daemon restart`).
If the reload reply is lost or unexpected, the installer probes the serving
daemon again without replaying the reload. Its report sets `reloaded` to `null`
(unknown), preserves `before`, and shows the newly observed `serving` daemon
or `null` if none answers.

## First install over a hand-copied app

A hand-copied installation (`install.sh` or the bundle script) predates the
managed one. The installer keeps unrelated files safe by refusing to
overwrite an app or command it did not install, and `install.sh` refuses in
the other direction: it will not copy over the links a managed installation
owns, and when it does replace a hand-copied bundle it stages the new one
beside the old and keeps the old as `AgentDocker.app.previous`. If `make install-preview`
reports a collision on `~/Applications/AgentDocker.app` or `~/.local/bin/*`,
do this once.

First end active agent sessions and stop the daemon: the running daemon and
any `agentdocker mcp` servers your agent tools started execute from the old
bundle, and moving it out from under them cuts them off.

```sh
agentdocker daemon stop
```

Then move the hand-copied files into a fresh backup directory (the name
carries a timestamp so an existing backup is never overwritten) and install:

Run one command per line; a pasted line continuation breaks the sequence.

```sh
backup=~/AgentDocker-old-$(date +%Y%m%d-%H%M%S)
mkdir "$backup"
cp -p ~/.agentdocker/state.db "$backup"/
mv ~/Applications/AgentDocker.app ~/.local/bin/agentdocker ~/.local/bin/agentd ~/.local/bin/agentdocker-ui "$backup"/
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u "$backup/AgentDocker.app"
make install
agentdocker daemon restart
```

The `lsregister -u` line matters: Launch Services otherwise keeps the moved
bundle registered and the Dock or Spotlight reopens the old app from the
backup folder. Quit any old window that is still open, then restart agent
sessions (Claude Code, Codex) so their helper processes run the new build,
and run `agentdocker setup <tool>` for integrations the old build lacks.
The state database upgrades one way on the first start of the new daemon;
the copy in the backup folder is the rollback for it.

## Source builds versus published updates

`make install` is for people building from source. Everyone else installs a
published release (Homebrew tap or `install.sh`) and later runs
`agentdocker desktop update`, or presses **Check for updates** in Settings →
Installation, which verifies and previews the newer release before installing
it for the next launch. See [DISTRIBUTION-SETUP.md](DISTRIBUTION-SETUP.md).

## Other targets

| Target | Purpose |
| --- | --- |
| `make build` | Debug build of the workspace |
| `make test` | Tests (nextest when installed) |
| `make clippy`, `make fmt` | Strict lint, formatting |
| `make check` | The standard gate, `scripts/verify.sh check` |
| `make app` | Build and package without installing |
| `make install-preview` | Build, package and print what install would change |
| `make run` | Run the desktop app from the checkout against your real daemon |
| `make smoke` | Native workflow acceptance against release binaries |
| `make status`, `make rollback` | Inspect or roll back the managed installation |
| `make clean-artifacts` | Unregister and remove `artifacts/local.noindex` and the older `artifacts/local` (keeps the Cargo cache) |

Set `PREFIX=/tmp/agentdocker-trial` on any install target to keep launchers,
versions and activation metadata inside a disposable directory. The Makefile
never chooses a Cargo target directory itself: Cargo's own configuration and
`CARGO_TARGET_DIR` decide, so no extra build cache appears, and packaging
reads the artifact directory the build reports.

Requirements: Rust (see `rust-version` in `crates/ui/Cargo.toml`), Python 3.11+,
and on macOS the Xcode command-line tools. End users of a packaged app need
none of these. Public, signed builds are described in
[DISTRIBUTION-SETUP.md](DISTRIBUTION-SETUP.md).
