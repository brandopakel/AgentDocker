# Setting up distribution

The release workflow generates the Homebrew formula and optional desktop cask.
Apple signing/notarization still needs a private developer identity and actual
release acceptance. Other engineering and platform work is tracked in
[Remaining work](REMAINING-WORK.md); distribution setup is one part of delivery.

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
require explicit preview acceptance. Prereleases leave the stable latest-release
endpoint and Homebrew tap unchanged; older maintenance releases cannot move
either backwards.

Assets are uploaded to a draft before publication. An upload failure leaves the
draft for a retry; an already published release is refused. Create releases
through the workflow, or leave a manually created release as a draft for it to
finish. A protected-tag run, hosted downloads, update/rollback and independent
machine acceptance remain necessary; generated assets alone do not establish them.

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
Before the first successful promotion, the channel URL returns 404.

The `channel-preview` tag remains at its first promotion's commit; subsequent
promotions replace only the feed and its record. Use the feed's versioned URLs
and source identities, not the channel tag's commit, to identify a build. The
feed has four fixed macOS/Linux targets. Adding Windows requires a feed and
client compatibility change first; its current portable ZIP is excluded.

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
gh workflow run preview-channel.yml --ref main -f release_tag=v0.2.0-beta.2
```

Use the actual already-published prerelease tag. This does not rebuild artifacts
or edit the versioned release. A new channel stays draft until its uploaded
feed verifies. Existing channel bodies/assets that do not match the publisher's
format are preserved and require investigation. Hosted feed and client
acceptance remain necessary after the first real promotion.

## The Homebrew tap

The tap and publishing configuration are present. The release generator builds
formula checksums from real archives and CI checks Ruby syntax.

A "tap" is just a GitHub repository named `homebrew-<something>` with a
`Formula/` directory in it. That is the whole mechanism.

**Status: done.** The tap exists at
[brandopakel/homebrew-tap](https://github.com/brandopakel/homebrew-tap),
carries the v0.1.0 formula, and both `HOMEBREW_TAP_REPOSITORY` and
`HOMEBREW_TAP_TOKEN` are set on this repository. The formula is available through:

```sh
brew tap brandopakel/tap
brew install agentdocker
```

Verified by installing it and running both binaries.

Every protected tagged release generates and validates the formula/cask,
publishes the download assets, then attempts to push the formula to
`Formula/agentdocker.rb` and the cask to `Casks/agentdocker-app.rb`. The tap job
depends on successful asset publication, preventing an upload failure from
advancing the tap to unavailable downloads. If the tap token is revoked the
job warns; the formula and cask remain attached to the run for manual recovery.
This ordering is implemented; the next tagged release must verify publication
end to end.

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
The older v0.1.0 Mac archives also carried the UI; current CLI archives contain
only the two commands. A **cask** installs an
application, which is what `AgentDocker.app` is: Homebrew puts it in
`/Applications` and knows how to take it away again.

`packaging/homebrew/agentdocker-app.rb.in` and the same generator
produce it, from the SHA-256 of the desktop archives the desktop
workflow builds. The release publishes both, and the cask is skipped —
loudly, not silently — when a release has no packaged app, because a
cask pointing at a download that is not there is worse than no cask.

The template exists, but as of the last recorded September 17 GitHub check the tap's `Casks/`
directory contains only a README. The install command below becomes available
only after a release publishes the app cask:

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
publishing a cask. Unsigned prereleases leave the stable tap unchanged. The
current cask template still contains a legacy unsigned-app caveat; reconcile it
with the final signed release before publication, rather than promising an
unsigned stable cask.

## Every route, one installation

| Route | Command | What it installs | Updated by |
| --- | --- | --- | --- |
| Desktop, direct download | `curl -fsSL …/install.sh \| sh` (default on macOS; `AGENTDOCKER_INSTALL=desktop` on Linux) | Downloads the verified desktop archive and runs the app's own installer from inside it: retained versions with rollback, launchers in `~/.local/bin`, the app in Applications | `agentdocker desktop update` |
| Commands, direct download | `AGENTDOCKER_INSTALL=cli` with the same script (default on Linux) | The two commands copied into `~/.local/bin`; refuses to write over links a managed installation owns | Run the script again |
| Homebrew cask | `brew install --cask brandopakel/tap/agentdocker-app` | The app in `/Applications` with its commands linked into Homebrew's bin | `brew upgrade --cask agentdocker-app` |
| Homebrew formula | `brew install brandopakel/tap/agentdocker` | The two commands and an optional `brew services` daemon | `brew upgrade agentdocker` |
| Local build | `make install` / `agentdocker desktop install --from` | The same managed installation as the desktop route | `agentdocker desktop update` or another local install |

A release before the desktop archives existed (v0.1.0) has no desktop zip;
the script says so and installs the commands instead, unless a version was
pinned, in which case it fails rather than guess.

## Apple Developer ID and notification acceptance

The local app has its icon and native notification-routing implementation.
The former AppleScript fallback, which could open Script Editor, has been
removed. A failed notification post retains its inbox message; it does not
prove the user saw a notification or that a click will route correctly.

An earlier local ad-hoc bundle returned this notification-post error:

```text
notifications are not permitted: Notifications are not allowed for this application (1)
```

That probe did not isolate signing from notification authorization and bundle
registration. Its result does not establish that paying for membership or
adding a signature alone fixes posting or navigation. The
notification audit (NOTIFICATION-ROUTING-AUDIT.md in git history) keeps those checks separate:
actual Notification Center clicks must open the right destination while the app
is active, backgrounded or closed, including retained drafts and expired targets.
Physical installed-app acceptance remains open.

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
on an independent Mac before publication. These acceptance steps remain
necessary after the credentials are configured.

The source-built app and CLI can continue local testing while release setup is
unfinished. The current published CLI/formula remains v0.1.0; the newer installed
local app is identified by its source commit, not that shared version string.

## Windows portable preview

The Windows packaging path targets `x86_64-pc-windows-msvc`. It builds a ZIP
with `agentdocker-ui.exe`, `agentdocker.exe` and `agentd.exe` together in the
`AgentDocker` folder, build metadata, licenses and opening instructions. The
packager checks native-build hashes again after copying and the PE x64 executable headers before
publishing the directory. The manifest and sidecar checksum identify the exact
archive; this preview is unsigned, without Authenticode or installer/update
support. Prerelease tags attach this separately tested Windows portable ZIP;
stable tags and the four-target update feeds remain macOS/Linux. Windows ARM64
is not claimed. The protected-tag publication path still needs its first live run.

On a native Windows build host, from the repository root in PowerShell:

```powershell
New-Item -ItemType Directory -Force artifacts | Out-Null
python scripts/build_native.py --target x86_64-pc-windows-msvc > artifacts/native-build.json
python scripts/windows_package_smoke.py --native-manifest artifacts/native-build.json --output artifacts/windows-desktop-package
```

The supplied build manifest must match the source-input and executable hashes
in the binary directory's manifest. The Windows workflow runs this trial before exposing its
`windows-desktop-preview-x86_64` artifact. It verifies the ZIP, extracts it
outside the checkout into a path containing spaces and Unicode, verifies every
executable against the manifest, removes the original staging payload, and runs
the daemon/CLI/terminal and fresh-home GUI trial on those extracted files.
The smoke's executable hashes must match the archive's hashes. A checksum,
runner trial and CI artifact do not establish publisher authentication or
clean-machine/provider acceptance. Those tests and the user installer, service,
update and rollback path remain in [Remaining work](REMAINING-WORK.md).
The Windows workflow ran this trial on `13e87591` (run 35666723079): 284 native tests and 51/51 steps on the extracted bytes; the line is in the [verification index](verification/INDEX.md).

Prerelease tags also build an **unsigned Windows x64 portable ZIP**. The Windows
job extracts that archive outside the build directory and runs the native
daemon/CLI/terminal/desktop trial before `release.py windows-preview` prepares
assets. Promotion checks the clean tag/build provenance, exact archived EXE
hashes, successful step results, GUI result and screenshot hash. It rechecks the
staged ZIP before exposing the output directory. A failed Windows build or trial
blocks the prerelease; stable tags skip this preview-only job.

Windows assets have separate `windows-preview-manifest.json` and
`windows-preview-acceptance.json` files and `WINDOWS-PREVIEW.txt` instructions.
They are not inputs to the four-target update feed. Release notes identify the
unsigned portable preview and its missing Windows installer, service and updater.
The public upload selects only release assets, excluding diagnostic artifacts.

Windows promotion refusal and exact-byte retention have fixture coverage. The
protected-tag Windows job, hosted ZIP download and independent-machine/provider
acceptance remain unverified until the candidate is released and tried.

## First coworker preview candidate

The source candidate is **0.2.0-beta.1**, with matching workspace packages,
internal dependency requirements and entries in both Cargo lockfiles (including
the excluded fuzz workspace). The legacy macOS bundle
helper reads this version from the workspace when no override is supplied.
The intended tag is `v0.2.0-beta.1`; changing the source version does not create a
tag, publish assets or install them. Publication remains open until the final
integrated source passes its gates and the protected-tag workflow finishes.

Before announcing the preview, download its actual hosted archives and sidecar
checksums, verify package provenance, and exercise the explicit-version install
route on macOS/Linux and ZIP extraction on Windows. Check that GitHub's stable
latest release, the stable update feed and Homebrew tap have not moved. Then
record first-run provider and restart acceptance on each declared platform in
the existing verification index. A Windows portable trial gives early feedback;
it does not complete the installer/service/update or native Codex input work.

## Order

1. Complete: the tap and v0.1.0 formula exist; publishing configuration names were
   present at the September 9 check. Secret values/token validity were not audited.
2. Open: configure Developer ID/notarization privately, reconcile the cask
   signing/installation contract, and validate final artifacts.
3. Open: run the protected-tag release, confirm formula/cask publication and
   exercise hosted update/rollback through the installed app.
4. Open: finish physical notification and independent-machine acceptance; retain
   results in the existing audit/verification records.
