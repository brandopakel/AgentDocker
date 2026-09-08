# Native desktop distribution

The product name is **agentdocker**. Its window, bundle display name and disk image say agentdocker. macOS uses a technical `.app` bundle extension; Finder normally hides it according to the user's display preferences. Packaging does not write FinderInfo to force hiding: that invalidates strict code-signature verification. Linux ships the same native window with a desktop launcher and icon. The window connects to the per-user daemon over a Unix socket. No browser or local HTTP server is involved.

## Build and verify a local preview

The build tools require Rust, Python 3.11+, and Xcode command-line tools on macOS. End users do not need Rust or Python to open the packaged app. These commands produce a **local preview** for the current Mac's architecture, not a signed public release:

```sh
native_build=$(python3 scripts/build_native.py)
native_binary_dir=$(printf '%s' "$native_build" | python3 -c 'import json,sys; print(json.load(sys.stdin)["binary_directory"])')
native_target=$(printf '%s' "$native_build" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target"])')
python3 packaging/desktop/package.py \
  --binary-dir "$native_binary_dir" --output artifacts/desktop \
  --version 0.1.0 --source "$(git rev-parse HEAD)" \
  --target "$native_target" --dmg
python3 scripts/desktop_smoke.py \
  --binary-dir artifacts/desktop/AgentDocker.app/Contents/MacOS \
  --output artifacts/desktop-smoke
```

Use a fresh output directory for every package or trial. `native-build.json` records the exact source input hash, commit, tree, dirty status, compiler, target, daemon state schema and executable checksums. Cargo-reported artifact paths respect custom target directories; use the reported `binary_directory` when packaging. Packaging refuses changed binaries, source/version mismatches, wrong architectures, mixed universal inputs and existing output directories. The final manifest hashes the signed executables and archives. A failed build or signer does not publish a completed artifact directory. A checksum detects corruption; it is not publisher authentication.

For Intel macOS, build with `--target x86_64-apple-darwin` and package from `target/x86_64-apple-darwin/release`. For a universal bundle, build both targets from unchanged source, then use `--target universal-apple-darwin --binary-dir target/release --second-binary-dir target/x86_64-apple-darwin/release`. A universal bundle still needs validation of both architectures; Rosetta execution is distinct from an Intel hardware trial.

Linux requires the system X11/Wayland client libraries, including `libxkbcommon-x11`, and a compatible Mesa or vendor graphics driver. Linux uses `--target x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`, built on a compatible Linux host. The archive contains three executables, a desktop entry, AppStream metadata and an SVG icon. Linux graphical acceptance runs the packaged executables under Xvfb with Mesa. The same acceptance driver uses a disposable home, socket, project and synthetic process; it verifies a real rendered PNG, connection, inventory and running process discovery. It samples the owned daemon and window for TCP sockets through readiness and screenshot capture until window exit, under the same bounded deadline. Reports include the sample count and duration. Short-lived sockets between samples can be missed. It does not prove provider message consumption or every GUI action. Local screenshots can include other discovered sessions: keep them private. CI captures only runner fixtures.

Build metadata comes from the compiled `agentd --build-info` command, which prints JSON and exits before opening state or a socket. Cross builds must be executable on the build host (for example, Intel macOS through Rosetta), or provide `scripts/build_native.py --schema-runner 'qemu-aarch64 -L /target/sysroot'` with a suitable target runner. The runner receives an argument vector without shell evaluation. Reported version, architecture, OS and schema are checked before the build manifest is written.

Linux launchers encode `Exec` and `Icon` separately according to the [Desktop Entry string rules](https://specifications.freedesktop.org/desktop-entry/latest/value-types.html) and [command quoting rules](https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html). Control characters and equals signs in executable paths are refused.

## Public macOS signing

Use a clean checkout, an installed **Developer ID Application** identity, and a local `notarytool` keychain profile:

```sh
python3 packaging/desktop/package.py \
  --binary-dir target/release --output artifacts/signed-desktop \
  --version 0.1.0 --source "$(git rev-parse HEAD)" \
  --target aarch64-apple-darwin --dmg \
  --identity 'Developer ID Application: YOUR NAME (TEAMID)' \
  --notary-profile agentdocker-notary
```

Keep private keys, passwords and API keys in Keychain or a separate private credential store. Never put them in this repository, command arguments, release artifacts or logs. The script references identity/profile names only. It signs nested executables explicitly and then the bundle, enabling hardened runtime and a secure timestamp for Developer ID. `--deep` is used only to verify. The app ZIP is submitted to Apple's service; only `Accepted` proceeds to stapling and Gatekeeper assessment. A requested DMG is separately signed, submitted and stapled before its final checksum is written. App signing metadata is fixed before signing; final notarization results belong to the external manifest and service reports.

This follows Apple's [distribution signing](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac), [notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution), and [custom notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow) guidance. Successful local ad-hoc verification is not Gatekeeper acceptance. Public signing remains unverified until the required developer identity is available and the actual signed artifact passes this flow.

## Install, update and roll back

Open **Installation** in the native window, choose **Use this application** or provide an extracted package path, and preview the installation. A disposable prefix keeps the app launcher, commands, retained versions and activation metadata inside that directory. Apply checks the reviewed package hash and prior active version again. Changed packages or installations require a new preview. The same operations are available from the CLI:

```sh
agentdocker desktop --prefix /tmp/agentdocker-trial install --from /path/to/AgentDocker.app --local-preview --preview
agentdocker desktop --prefix /tmp/agentdocker-trial install --from /path/to/AgentDocker.app --local-preview
agentdocker desktop --prefix /tmp/agentdocker-trial status
agentdocker desktop --prefix /tmp/agentdocker-trial rollback --local-preview --preview
agentdocker desktop --prefix /tmp/agentdocker-trial rollback --local-preview
```

Omit `--prefix` to install beneath your home. Mac launchers go into `~/Applications`, Linux launchers into `~/.local/share/applications`, and commands into `~/.local/bin`. Add that command directory to your PATH if necessary. Existing unrelated commands, apps and edited launchers are preserved: installation refuses a collision. Mac public installation requires signature verification and Gatekeeper acceptance; `--local-preview` explicitly allows an ad-hoc development build. Linux verifies package binary checksums, which establish consistency, not publisher identity. Extract the package first; this command does not fetch untrusted URLs or expand arbitrary archives.

Complete copied payloads are verified and synced before activation. Immutable version directories and activation records retain the previous release; one atomic pointer switches all managed launchers. A crash before that switch leaves the prior release active, although unused staging/generation files can remain. A single installer lock excludes concurrent updates. Preview and status do not create an installation. Newly configured provider connections use the stable managed CLI link. A stale running app must be reopened before setup; configurations written by earlier builds are not silently rewritten.

Updates affect the next app/CLI launch. They do not stop a live daemon or its agents. Daemon replacement remains an explicit lifecycle operation. Rollback verifies the retained payload and requires equal daemon state schemas; it does not restore or downgrade the database. Keep a matching state backup for any manual downgrade. Retained versions are not automatically pruned.

`scripts/desktop_install_smoke.py --source artifacts/desktop --output artifacts/install-smoke` tests this flow under a disposable prefix, including stale-preview rejection, tampered executables, private activation metadata, retained versions and a responsive daemon across activation/rollback. CI uses two package generations of the same binaries and labels that limitation. `--previous-source` accepts a separately built older package for a trial between source revisions. Graphical acceptance and real-provider round trips are separate checks.

## Remaining delivery requirements

The current installer handles verified local packages and explicit activation/rollback. A download/update feed, automatic update scheduling, uninstall/retention controls, Homebrew cask, Linux distribution packages, and Windows support remain. They also do not retroactively update the existing public v0.1.0 release. Guided setup with preview/undo/health and safe daemon upgrade boundaries are tracked in [NATIVE-DELIVERY.md](NATIVE-DELIVERY.md). New distribution artifacts must pass their checks and review before publication.
