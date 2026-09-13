# Native desktop distribution

The product name is **AgentDocker**. Its window, bundle name, Launchpad entry and disk image say AgentDocker; only the command-line tools are lowercase (`agentdocker`, `agentd`, `agentdocker-ui`). macOS uses a technical `.app` bundle extension; Finder normally hides it according to the user's display preferences. Packaging does not write FinderInfo to force hiding: that invalidates strict code-signature verification. Linux ships the same Iced native window with a desktop launcher and icon. The window connects to the per-user daemon over a Unix socket. No browser or local HTTP server is involved.

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

Linux requires the system X11/Wayland client libraries, including `libxkbcommon-x11`. The production tiny-skia renderer does not require Vulkan or a discrete GPU. Linux uses `--target x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`, built on a compatible Linux host. The archive contains three executables, a desktop entry, AppStream metadata and an SVG icon. Linux graphical acceptance runs the packaged executables under Xvfb with Mesa. The same acceptance driver uses a disposable home, socket, project and synthetic process; it verifies a real rendered PNG, connection, inventory and running process discovery. It samples the owned daemon and window for TCP sockets through readiness and screenshot capture until window exit, under the same bounded deadline. Reports include the sample count and duration. Short-lived sockets between samples can be missed. A second driver, `scripts/iced_workflow_smoke.py`, exercises the actual rendered controls through project pinning/restoration, question delivery, channel messages, setup apply/undo, launch/stop, terminal rendering, commands and focus reveal. It does not prove provider consumption or physical keyboard/screen-reader behavior. Local screenshots can include other discovered sessions: keep them private. CI captures only runner fixtures.

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

Open **Settings → Manage installation and retained versions** in the native window, choose **Use this application** or provide an extracted package path, and preview the installation. A disposable prefix keeps the app launcher, commands, retained versions and activation metadata inside that directory. Apply checks the reviewed package hash and prior active version again. Changed packages or installations require a new preview. The same operations are available from the CLI:

```sh
agentdocker desktop --prefix /tmp/agentdocker-trial install --from /path/to/AgentDocker.app --local-preview --preview
agentdocker desktop --prefix /tmp/agentdocker-trial install --from /path/to/AgentDocker.app --local-preview
agentdocker desktop --prefix /tmp/agentdocker-trial status
agentdocker desktop --prefix /tmp/agentdocker-trial rollback --local-preview --preview
agentdocker desktop --prefix /tmp/agentdocker-trial rollback --local-preview
```

Omit `--prefix` to install beneath your home. On a Mac the launcher `AgentDocker.app` goes into `/Applications` when that folder is writable and the name is free or already ours, which is where Finder and Launchpad look; otherwise into `~/Applications`. The choice is recorded in the managed store (`launcher.json`) so it does not move later, and when the launcher lives in `/Applications` a link stays at `~/Applications/AgentDocker.app` so absolute hook and MCP command paths written by earlier releases keep running the active release. The launcher is a real bundle (Launchpad ignores symlinked bundles) whose executables are stable links into the managed `current` pointer, so updates and rollbacks never rewrite it; a home-prefix install also registers it with Launch Services. Linux launchers go into `~/.local/share/applications`, and commands into `~/.local/bin`.

Complete copied payloads are verified and synced before activation. Immutable version directories and activation records retain the previous release; one atomic pointer switches all managed launchers. A crash before that switch leaves the prior release active, although unused staging/generation files can remain. A single installer lock excludes concurrent updates. Preview and status do not create an installation. Newly configured provider connections use the stable managed CLI link. A stale running app must be reopened before setup; configurations written by earlier builds are not silently rewritten.

### Updating from the published feed

`agentdocker desktop update` reads the download feed (`updates.json`, by default
the asset of the latest GitHub release), picks this machine's target, and says
whether a newer release exists than the managed installation (or, with none,
than the running command):

```sh
agentdocker desktop update --check            # report only; downloads nothing
agentdocker desktop update                    # download, verify, extract, preview
agentdocker desktop update --apply            # the same, then install for the next launch
```

The archive is fetched with the system `curl`, HTTPS only including redirects,
bounded in size and time, into `~/.local/share/agentdocker/desktop/downloads/<version>/`
(private), then compared byte for byte and by SHA-256 with what the feed
advertised; a mismatch deletes the download and installs nothing. The extracted
payload goes through the same inspection as `install --from` (links, targets,
checksums, signature and Gatekeeper unless `--local-preview`), must be the very
release the feed described (version, source, state schema, target), and must be
newer than what is installed; `--apply` pins the reviewed release and current
IDs exactly as the desktop screen's Apply does. The report includes how many
agents the running daemon says are live, so the person can choose when to
restart it; the command never restarts anything. The desktop screen offers
**Check for updates** and **Download and preview** on Settings → Installation,
and the footer says when a newer version is known. A preview feed or a
`file://` feed is accepted only with `--local-preview`; `AGENTDOCKER_UPDATE_FEED`
overrides the feed URL. `scripts/desktop_update_smoke.py --source <package dir>
--output <dir>` exercises the whole path offline against a locally packaged
release. Publication of `updates.json` beside the release archives is part of
the release workflow.

Settings also offers **Daily update checks**, off by default. When enabled, the
open app checks at most once per 24 hours and checks on its next launch if due.
The timestamp is saved before the request, so restarting after an offline or
failed check does not retry immediately. A full worker queue waits for capacity.
The check has a separate worker and a 45-second deadline; it preserves any
installation preview and its Apply pin. A clickable footer opens the available
release in Installation. Downloads and activation still require explicit actions.
The scheduler does not run while the app is closed or an alternate installation
prefix is selected. Disabling it stops future checks; an in-flight read-only
request may finish. `scripts/daily_update_smoke.py` exercises these controls and
restart behavior with a controlled CLI reply; the update-consumer driver above
provides separate feed/archive validation.

Updates affect the next app/CLI launch. They do not stop a live daemon or its agents. Daemon replacement remains an explicit lifecycle operation. Rollback verifies the retained payload and requires equal daemon state schemas; it does not restore or downgrade the database. Keep a matching state backup for any manual downgrade. Retained versions are not automatically pruned.

`scripts/desktop_install_smoke.py --source artifacts/desktop --output artifacts/install-smoke` tests this flow under a disposable prefix, including stale-preview rejection, tampered executables, private activation metadata, retained versions and a responsive daemon across activation/rollback. CI uses two package generations of the same binaries and labels that limitation. `--previous-source` accepts a separately built older package for a trial between source revisions. Graphical acceptance and real-provider round trips are separate checks.

## Remove launchers and clean up retained versions

**Settings → Manage installation and retained versions** offers **Preview removal** and **Preview cleanup**.
It lists the exact removals and retention reasons; **Apply reviewed cleanup**
refuses a changed plan. CLI equivalents are:

```sh
agentdocker desktop --prefix /tmp/agentdocker-trial uninstall --preview
agentdocker desktop --prefix /tmp/agentdocker-trial uninstall --expect-plan PLAN_ID
agentdocker desktop --prefix /tmp/agentdocker-trial prune --keep 2 --preview
agentdocker desktop --prefix /tmp/agentdocker-trial prune --keep 2 --expect-plan PLAN_ID
```

Use the `plan_id` returned by the matching preview. Uninstall removes only owned
launchers and the active installation pointer. It preserves running sessions,
daemon state, provider configuration and retained payloads. Provider connections
that used the removed CLI link require reinstallation or an explicit setup
change. Uninstall is resumable if interrupted between launcher removals.

Prune keeps the active and rollback versions, plus `--keep` additional inactive
versions, newest first. It also keeps every running release and legacy releases
that lack lifetime locking. New CLI, daemon and window processes hold shared
version locks; cleanup requires an exclusive lock and rechecks payload hashes
before deletion. On macOS, the kernel's loaded executable path selects the pin,
so switching the managed pointer cannot move a running process to another release's
lock. Matching CLI/daemon/window siblings use that same loaded path, and
`agentdocker ui` prefers its matching sibling over a separately installed app.
A process losing the startup/removal race exits before normal
operation. Pin files and activation records remain; retention does not delete
unknown or modified payloads. Cleanup is explicit, not scheduled.

An installed user service blocks launcher removal and protects all retained
versions because its configuration may reference an older binary directly.
Review `agentdocker daemon uninstall --dry-run` and explicitly remove that
service first. Desktop cleanup never changes the service registration itself.
Public signing requirements and daemon replacement boundaries still apply.

## Remaining delivery requirements

The current installer handles verified local packages and explicit activation/rollback.
`packaging/desktop/feed.py` generates download metadata from the package manifests
and the actual archive bytes:

```sh
python3 packaging/desktop/feed.py artifacts/desktop/manifest.json \
  --preview --output artifacts/updates-preview.json
```

Multiple manifests must have the same source, version and daemon schema, with
one entry per target. The generator checks archive hashes, sizes and download
budgets. Public feeds require clean source and notarized Developer ID packages
for macOS. A preview feed cannot establish public signing or download availability.
Generation does not publish the feed, fetch updates or schedule checks. The feed
records the intended policy: at most one daily check, manual download, explicit
activation and deferred daemon replacement until sessions finish. The consumer
and opt-in native scheduler are implemented with local fixture acceptance.
Hosted-release download and signed-distribution acceptance remain required.

Native desktop CI now includes Linux x86-64/ARM64 and macOS ARM64/Intel runners,
each building and graphically exercising its packaged binaries. Adding the jobs
does not establish a passing run or target-distribution acceptance.

Public feed hosting, Homebrew cask publication, Linux distribution packages and
Windows support remain. These changes do not update the existing public v0.1.0
release. Guided setup and daemon upgrade boundaries are tracked in
[NATIVE-DELIVERY.md](NATIVE-DELIVERY.md). New artifacts must pass their checks and
review before publication. Homebrew publication now follows successful release
asset upload, so a failed upload cannot advance the tap to missing downloads.
