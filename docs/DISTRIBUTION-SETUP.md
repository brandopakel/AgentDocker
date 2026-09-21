# Setting up distribution

The release workflow generates the Homebrew formula and optional desktop cask.
Apple signing/notarization still needs a private developer identity and actual
release acceptance. Other engineering and platform work is tracked in
[Remaining work](REMAINING-WORK.md); distribution setup is one part of delivery.

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
notification audit (in git history) keeps those checks separate:
actual Notification Center clicks must open the right destination while the app
is active, backgrounded or closed, including retained drafts and expired targets.
Physical installed-app acceptance remains open.

The packaging pipeline accepts `--identity` and `--notary-profile` through
`packaging/desktop/package.py`. Local previews can be ad-hoc signed; the stable
protected-tag workflow requires a Developer ID Application identity and
successful notarization. Follow release automation (in git history) for
private credential configuration. Verify signing, notarization, stapling and
Gatekeeper against the final app/DMG on an independent Mac before publication.
These acceptance steps remain necessary after the credentials are configured.

The source-built app and CLI can continue local testing while release setup is
unfinished. The current published CLI/formula remains v0.1.0; the newer installed
local app is identified by its source commit, not that shared version string.

## Order

1. Complete: the tap and v0.1.0 formula exist; publishing configuration names were
   present at the September 9 check. Secret values/token validity were not audited.
2. Open: configure Developer ID/notarization privately, reconcile the cask
   signing/installation contract, and validate final artifacts.
3. Open: run the protected-tag release, confirm formula/cask publication and
   exercise hosted update/rollback through the installed app.
4. Open: finish physical notification and independent-machine acceptance; retain
   results in the existing audit/verification records.
