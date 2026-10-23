# Setting up distribution

The release workflow generates the Homebrew formula and optional desktop cask.
Apple signing and notarization need a private developer identity.

## Release workflow and retry policy

A protected `v*` tag triggers `.github/workflows/release.yml`; editing the
workflow does not publish anything. The tag version must match `Cargo.toml`
and the recorded build must have clean source. Tags exclude `+build` metadata.
The workflow builds CLI archives and four native desktop targets: Apple
Silicon, Intel Mac, Linux x86_64 and Linux ARM64. The graphical Linux packages
use GNU libc; the separate CLI-only Linux archives use musl.

`packaging/desktop/release.py` prepares archives, checksums and target manifests.
The desktop feed requires all four targets from the same source, version and
schema, and verifies the archive bytes. Stable tags produce `updates.json`;
prerelease tags produce `updates-preview.json`, remain GitHub prereleases and
require explicit preview acceptance. GitHub's `latest` never names a prerelease,
so the preview feed also has one fixed address, the `channel-preview` release
(`releases/download/channel-preview/updates-preview.json`), which only moves
forward. `agentdocker desktop update` without `--feed` reads the stable feed,
plus that one on a prerelease installation or with `--local-preview`, and takes
the newer. A prerelease installation takes the next preview without new
consent; a stable one downloads a preview build only with `--local-preview`,
and `file://` sources always need that flag. Staying on the preview channel
skips Gatekeeper's policy assessment only for a build the feed marks
`local-preview` (ad-hoc, which Gatekeeper would refuse); a Developer-ID-signed
preview is still assessed. Only `--local-preview` itself skips it for any
build. A channel with nothing published (404) is reported as `published: false`, not
as a failure. Prereleases leave the stable latest-release endpoint and Homebrew
tap unchanged; older maintenance releases cannot move either backwards.

Assets are uploaded to a draft before publication. An upload failure leaves the
draft for a retry; an already published release is refused. Create releases
through the workflow, or leave a manually created release as a draft for it to
finish.

### Preview update channel

After a versioned prerelease publishes, the workflow copies its verified
`updates-preview.json` to this fixed URL:

```text
https://github.com/brandopakel/AgentDocker/releases/download/channel-preview/updates-preview.json
```

The `channel-preview` release is itself a prerelease with `latest=false` and
contains only the feed. Download URLs still name immutable versioned releases;
the stable latest-release endpoint and Homebrew tap remain unchanged. Clients
keep downloads manual and activation explicit. A stable installation must opt
into previews; an existing preview installation has already chosen that channel.
Until the first promotion, the channel URL returns 404.

The `channel-preview` tag stays at its first promotion's commit; later
promotions replace only the feed and its record. Use the feed's versioned URLs
and source identities, not the channel tag's commit, to identify a build. The
macOS/Linux feed keeps its four fixed targets. Windows installation uses the
separate `channel-preview-windows` release and `updates-preview-windows.json`
asset; a release that carries only the portable Windows ZIP cannot serve as an
installed baseline. Neither Windows feed is added to the four-target feed or the
stable update channel.

The publisher checks the actual versioned release, source commit, four target
assets, sizes and available GitHub digests before copying the feed. Promotions
and repairs share the publication lock and compare semantic versions, so a
late older run cannot replace a newer preview. The release body records the
highest promoted version before upload; if clobber removes the old feed and the
upload fails, an older retry still cannot take over. A same-version retry must
use identical canonical feed bytes.
If the record names version N but the asset still contains N-1 (or is absent),
retry N: an N-1 retry deliberately reports `preserved_newer`. An empty feed asset
left in GitHub's `starter` state is repairable only by that exact recorded
version; unrelated or nonempty unfinished assets are refused.

If the versioned release is published but channel promotion fails, repair only
the channel from reviewed `main`:

```sh
gh workflow run preview-channel.yml --ref main -f release_tag="v<version>"
```

Use the tag of the already-published prerelease. This does not rebuild artifacts
or edit the versioned release. A new channel stays draft until its uploaded
feed verifies. Existing channel bodies/assets that do not match the publisher's
format are preserved and require investigation.

GitHub's release-by-tag endpoint only returns published releases. On a channel
404, the publisher also checks the authenticated release inventory for the
draft, across at most ten pages of 100 releases. Ambiguous matches, malformed
responses (including entries without a string tag name), lookup failures or a
larger inventory fail without changing any
release. This lets initial publication and interrupted-publication repair
verify uploaded draft assets before exposing the feed.

## The Homebrew tap

A "tap" is just a GitHub repository named `homebrew-<something>` with a
`Formula/` directory in it. That is the whole mechanism. The release generator
builds formula checksums from real archives and CI checks Ruby syntax.

The tap is [brandopakel/homebrew-tap](https://github.com/brandopakel/homebrew-tap).
Publishing needs the `HOMEBREW_TAP_REPOSITORY` repository variable
(`owner/homebrew-tap`) and the `HOMEBREW_TAP_TOKEN` secret on this repository.
The formula is available through:

```sh
brew tap brandopakel/tap
brew install agentdocker
```

Every protected tagged release generates and validates the formula/cask,
publishes the download assets, then attempts to push the formula to
`Formula/agentdocker.rb` and the cask to `Casks/agentdocker-app.rb`. The tap job
depends on successful asset publication, preventing an upload failure from
advancing the tap to unavailable downloads. If the tap token is revoked the
job warns; the formula and cask remain attached to the run for manual recovery.

Replacing the token, when it expires: make a *fine-grained* personal
access token at
<https://github.com/settings/personal-access-tokens/new>, scoped to
**only** `brandopakel/homebrew-tap`, with **Contents: read and write** —
it needs nothing from the AgentDocker repository — and then

```sh
gh secret set HOMEBREW_TAP_TOKEN
```

Typed at the prompt, or piped. Not `--body "<the token>"`: an argument
is visible to every process on the machine while the command runs, and
it stays in the shell history afterwards.

## The cask

Homebrew has two kinds of thing and we need both. A **formula** installs
commands — `agentdocker` and `agentd`, plus the daemon as a Homebrew service.
CLI archives contain only the two commands, which are one executable: `agentd`
is a hard link to `agentdocker`, the daemon when invoked by that name
([one executable](ARCHITECTURE.md#one-executable)), and the archive holds it
once. A **cask** installs an
application, which is what `AgentDocker.app` is: Homebrew puts it in
`/Applications` and knows how to take it away again.

`packaging/homebrew/agentdocker-app.rb.in` and the same generator
produce it, from the SHA-256 of the desktop archives the desktop
workflow builds. The release publishes both, and the cask is skipped —
loudly, not silently — when a release has no packaged app, because a
cask pointing at a download that is not there is worse than no cask. The
install command is available once a release publishes the app cask:

```sh
brew install --cask brandopakel/tap/agentdocker-app
```

The cask keeps the same one-copy contract as the app's own installer: the
application carries `agentdocker` and `agentd` inside its bundle, and the cask
links those into Homebrew's bin as `binary` stanzas. It therefore
`conflicts_with` the formula rather than depending on it — the formula is the
commands-only route, the cask is the app route, and nobody ends up with two
copies of the commands. Uninstalling the cask stops the `dev.agentdocker.agentd`
login service first; `zap` also removes `~/.agentdocker`. Homebrew owns that
copy: `agentdocker desktop update` reports a Homebrew installation and points
at `brew upgrade --cask agentdocker-app` rather than installing a second copy
beside it.

Stable tagged releases require Developer ID signing and notarization before
publishing a cask. Unsigned prereleases leave the stable tap unchanged.

## Every route, one installation

| Route | Command | What it installs | Updated by |
| --- | --- | --- | --- |
| Desktop, direct download | `curl -fsSL …/install.sh \| sh` (default on macOS; `AGENTDOCKER_INSTALL=desktop` on Linux) | Downloads the verified desktop archive and runs the app's own installer from inside it: retained versions with rollback, launchers in `~/.local/bin`, the app in Applications | `agentdocker desktop update` |
| Commands, direct download | `AGENTDOCKER_INSTALL=cli` with the same script (default on Linux) | The two commands into `~/.local/bin`, `agentd` a hard link to `agentdocker`; refuses to write over links a managed installation owns | Run the script again |
| Homebrew cask | `brew install --cask brandopakel/tap/agentdocker-app` | The app in `/Applications` with its commands linked into Homebrew's bin | `brew upgrade --cask agentdocker-app` |
| Homebrew formula | `brew install brandopakel/tap/agentdocker` | The two commands and an optional `brew services` daemon | `brew upgrade agentdocker` |
| Local build | `make install` / `agentdocker desktop install --from` | The same managed installation as the desktop route | `agentdocker desktop update` or another local install |

A release without a desktop archive has no desktop zip; the script says so and
installs the commands instead, unless a version was pinned, in which case it
fails rather than guess.

After an install, update or rollback changes the active desktop release, the
installer automatically prunes unused owned builds under its installation lock.
It keeps the active and rollback builds, every running build with a lifetime pin,
legacy builds without the pin contract, unrecognized directories and the build
providing the visible Mac launcher. An installed user service preserves all
builds because it may reference their binaries. Cleanup checks content hashes
and holds removal pins through deletion. Preview and unchanged installs do not
prune. A cleanup failure appears in the install report's `retention` field and
does not undo or fail a successful activation; `desktop prune --preview` can
inspect what remains. `desktop prune --keep N` retains additional builds for
that manual operation; a later activation again keeps only protected builds.

## Apple Developer ID and notification acceptance

The app has its icon and native notification-routing implementation. A failed
notification post retains its inbox message; it does not prove the user saw a
notification or that a click will route correctly. Notification authorization
and bundle registration are separate from signing, so a signature alone does
not establish posting or navigation. Actual Notification Center clicks must open
the right destination while the app is active, backgrounded or closed, including
retained drafts and expired targets.

The packaging pipeline accepts `--identity` and `--notary-profile` through
`packaging/desktop/package.py`. Stable Mac releases require a Developer ID
Application identity and successful notarization. Configure these repository or
organization secrets for the protected-tag workflow:

| Secret | Content |
| --- | --- |
| `MACOS_CERTIFICATE_BASE64` | Base64 of the exported `.p12` certificate and private key |
| `MACOS_CERTIFICATE_PASSWORD` | Certificate export password; an empty password is supported |
| `MACOS_SIGNING_IDENTITY` | Full `Developer ID Application: …` identity |
| `MACOS_NOTARY_KEY` | App Store Connect team API private key, including PEM delimiters |
| `MACOS_NOTARY_KEY_ID` | API key ID |
| `MACOS_NOTARY_ISSUER` | Team API issuer ID |

The Mac job checks this configuration before its native build. Unsigned
prereleases can use ad-hoc signing with no signing secrets; partial signing
configuration is an error. Stable publication requires complete credentials
and successful notarization. An empty certificate password is valid, but its
environment variable must still be supplied.

Packaging creates a temporary private keychain and notarization profile,
imports the certificate, then signs and notarizes the package. It restores the
original keychain settings and removes the temporary keychain even if packaging
fails. Credential files are private and are never release artifacts. The jobs
use ephemeral GitHub-hosted Mac runners.

Verify signing, notarization, stapling and Gatekeeper against the final app/DMG
on an independent Mac before publication.

Disk-image creation allows at most three attempts when `hdiutil` exits 1 with
the exact `create failed - Resource busy` diagnostic, waiting two then four
seconds. Each creation attempt and final image verification has a 180-second
timeout. Other failures stop packaging. A completed image must pass
`hdiutil verify` before publication.

The packager retains attempt status, duration and the last 64 KiB of each output
stream in `OUTPUT-dmg-create.json`, outside the temporary package directory.
Use a fresh output prefix; existing diagnostics are never overwritten. The
optional `--dmg-diagnostics` selects another fresh path. Desktop CI retains this
report even on failure; the manifest records the attempt count and whether
recovery was needed.

## Windows portable preview

The Windows packaging path targets `x86_64-pc-windows-msvc`. It builds a ZIP
with `agentdocker-ui.exe`, `agentdocker.exe` and `agentd.exe` together in the
`AgentDocker` folder, build metadata, licenses and opening instructions.
`agentd.exe` is a byte-identical copy of `agentdocker.exe`, the daemon when
invoked by that name: a ZIP holds no links, and Windows installs keep it a
copy. The
packager checks native-build hashes again after copying and the PE x64 executable
headers before publishing the directory. The manifest and sidecar checksum
identify the exact archive; the preview is unsigned, without Authenticode.
Prerelease tags attach this Windows portable ZIP; stable tags and the
four-target update feeds are macOS/Linux only. After installer lifecycle
acceptance the release also publishes the installed-package manifest and the
separate Windows update feed. Windows ARM64 is not supported.

On a native Windows build host, from the repository root in PowerShell:

```powershell
New-Item -ItemType Directory -Force artifacts | Out-Null
python scripts/build_native.py --target x86_64-pc-windows-msvc > artifacts/native-build.json
python scripts/windows_package_smoke.py --native-manifest artifacts/native-build.json --output artifacts/windows-desktop-package
```

The supplied build manifest must match the source-input and executable hashes
in the binary directory's manifest. The Windows workflow runs this trial before
exposing its `windows-desktop-preview-x86_64` artifact. It verifies the ZIP,
extracts it outside the checkout into a path containing spaces and Unicode,
verifies every executable against the manifest, removes the original staging
payload, and runs the daemon/CLI/terminal and fresh-home GUI trial on those
extracted files. The smoke's executable hashes must match the archive's hashes.
A checksum, runner trial and CI artifact do not establish publisher
authentication or clean-machine or provider acceptance.

Prerelease tags build the archive the same way. The Windows job extracts it
outside the build directory and runs the native daemon/CLI/terminal/desktop
trial before `release.py windows-preview` prepares assets. Promotion checks the
clean tag/build provenance, exact archived EXE hashes, successful step results,
GUI result and screenshot hash, and rechecks the staged ZIP before exposing the
output directory. A failed Windows build or trial blocks the prerelease; stable
tags skip this preview-only job. The Windows installed-package preview channel
is published only when installation and installed-service acceptance pass on
the exact archive.

Windows assets have separate `windows-preview-manifest.json` and
`windows-preview-acceptance.json` files and `WINDOWS-PREVIEW.txt` instructions.
They are not inputs to the four-target update feed. Release notes identify the
unsigned distribution. Optional per-user Task Scheduler startup is described in
[the Windows port](WINDOWS-PORT.md#services). The package instructions require
finishing managed work, quitting the app, uninstalling an enabled task with
`daemon uninstall`, and stopping the daemon before moving or replacing its
folder. Extraction alone does not install a startup task. The public upload
selects only release assets, excluding diagnostic artifacts.

The native Windows workflow also accepts `hosted_tag` and `hosted_source` for an
immutable published preview. `scripts/windows_hosted_smoke.py` verifies the tag,
GitHub asset digests, checksum, archive layout and executable provenance before
running a fresh private-prefix installation, the default installed preview
feed, and installed daemon and connector service lifecycles. It confirms stable
and the Homebrew tap are unchanged and preserves failed scratch. With no
baseline this covers first installation and current-version feed discovery. An
optional older `baseline_tag`/`baseline_source` must support native
installation contract 2; the trial then requires an actual hosted update,
rollback and reapply. A portable-only older release is refused before
execution.

## Preview releases

Versioned published releases are immutable.

Releases carry a state schema version and a delivery ledger version. A release
cannot replace one with a newer schema, and package rollback to an older schema
refuses before changing the selected binaries. Never restore an older
delivery ledger over acknowledged input; use isolated state for an
older-version trial. Matching schema versions alone do not establish
compatibility for another release.

Native Codex launch, managed secret input and live replacement are
experimental.

Before announcing a preview, download its actual hosted archives and sidecar
checksums, verify package provenance, and exercise the explicit-version install
route on macOS/Linux and ZIP extraction on Windows. Check that GitHub's stable
latest release, the stable update feed and the Homebrew tap have not moved. Then
record first-run provider and restart acceptance on each declared platform. A
Windows portable trial gives early feedback; it does not complete the
installer, service, update or native Codex input path.
