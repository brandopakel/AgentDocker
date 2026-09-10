# Build and install AgentDocker from source

One command builds the release binaries, packages the native app for this
machine and installs it for your user, so the next launch runs what you just
built:

```sh
make install
```

Then quit AgentDocker and open it again. The Dock, Spotlight, the `agentdocker`
and `agentd` commands in `~/.local/bin`, and the MCP entry your agent tools use
all follow one managed pointer, so they switch together.

## What `make install` does

1. `scripts/build_storage.py`: the disk-budget preflight from the testing standard.
2. `scripts/build_native.py`: `cargo build --release` of the three executables,
   bound to the exact source inputs in a `native-build.json` manifest.
3. `packaging/desktop/package.py`: the same app bundle (macOS) or desktop
   payload (Linux) the release pipeline produces, ad-hoc signed, with licenses,
   into `artifacts/local/desktop-<timestamp>/`. `artifacts/local/latest` points at
   the newest one.
4. `agentdocker desktop install --from <payload> --local-preview`: the built
   CLI installs its own package for the next launch. Versions are retained;
   `make rollback` activates the previous one; `make status` shows both.

Nothing here touches a running daemon or its agents. A daemon that is already
running keeps serving the old build until you end agent work and run
`make restart-daemon` (or `agentdocker daemon restart`), after which the next
client or app launch starts the installed one. Safe live replacement is still
open engineering; see [Remaining work](REMAINING-WORK.md).

## First install over a hand-copied app

Installations before this command copied files by hand (`install.sh`, or the
bundle script). The installer keeps unrelated files safe by refusing to
overwrite an app or command it did not install. If `make install-preview`
reports a collision on `~/Applications/AgentDocker.app` or `~/.local/bin/*`,
move those aside once, then install:

```sh
mkdir -p ~/AgentDocker-old && mv ~/Applications/AgentDocker.app ~/.local/bin/agentdocker ~/.local/bin/agentd ~/.local/bin/agentdocker-ui ~/AgentDocker-old/
make install
```

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
| `make clean-artifacts` | Remove `artifacts/local` (keeps the Cargo cache) |

Set `PREFIX=/tmp/agentdocker-trial` on any install target to keep launchers,
versions and activation metadata inside a disposable directory. Set
`CARGO_TARGET_DIR` to share a build cache; the Makefile defaults to `target/`
in this checkout.

Requirements: Rust (see `rust-version` in `crates/ui/Cargo.toml`), Python 3.11+,
and on macOS the Xcode command-line tools. End users of a packaged app need
none of these. Public, signed builds are described in
[Desktop distribution](DESKTOP-DISTRIBUTION.md).
