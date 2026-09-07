# Native desktop distribution

The product name is **agentdocker**. Its window, bundle display name and disk image say agentdocker. macOS uses a technical `.app` bundle extension; Finder normally hides it according to the user's display preferences. Packaging does not write FinderInfo to force hiding: that invalidates strict code-signature verification. Linux ships the same native window with a desktop launcher and icon. The window connects to the per-user daemon over a Unix socket. No browser or local HTTP server is involved.

## Build and verify a local preview

The build tools require Rust, Python 3.11+, and Xcode command-line tools on macOS. End users do not need Rust or Python to open the packaged app. These commands produce a **local preview**, not a signed public release:

```sh
python3 scripts/build_native.py
python3 packaging/desktop/package.py \
  --binary-dir target/release --output artifacts/desktop \
  --version 0.1.0 --source "$(git rev-parse HEAD)" \
  --target aarch64-apple-darwin --dmg
python3 scripts/desktop_smoke.py \
  --binary-dir artifacts/desktop/agentdocker.app/Contents/MacOS \
  --output artifacts/desktop-smoke
```

Use a fresh output directory for every package or trial. `native-build.json` records the exact source input hash, commit, tree, dirty status, compiler, target and executable checksums. Packaging refuses changed binaries, source/version mismatches, wrong architectures, mixed universal inputs and existing output directories. The final manifest hashes the signed executables and archives. A failed build or signer does not publish a completed artifact directory. A checksum detects corruption; it is not publisher authentication.

For Intel macOS, build with `--target x86_64-apple-darwin` and package from `target/x86_64-apple-darwin/release`. For a universal bundle, build both targets from unchanged source, then use `--target universal-apple-darwin --binary-dir target/release --second-binary-dir target/x86_64-apple-darwin/release`. A universal bundle still needs validation of both architectures; Rosetta execution is distinct from an Intel hardware trial.

Linux requires the system X11/Wayland client libraries, including `libxkbcommon-x11`, and a compatible Mesa or vendor graphics driver. Linux uses `--target x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`, built on a compatible Linux host. The archive contains three executables, a desktop entry, AppStream metadata and an SVG icon. Linux graphical acceptance runs the packaged executables under Xvfb with Mesa. The same acceptance driver uses a disposable home, socket, project and synthetic process; it verifies a real rendered PNG, connection, inventory and running process discovery. It checks for TCP sockets at startup. It does not prove provider message consumption or every GUI action. Local screenshots can include other discovered sessions: keep them private. CI captures only runner fixtures.

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

## Remaining delivery requirements

The archive/DMG builder and graphical trial do not yet provide an automatic installer, updater, rollback policy, Homebrew cask, Linux distribution packages, or Windows support. They also do not retroactively update the existing public v0.1.0 release. Guided setup with preview/undo/health and safe daemon upgrade boundaries are tracked in [NATIVE-DELIVERY.md](NATIVE-DELIVERY.md). New distribution artifacts must pass their checks and review before publication.
