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

Every tagged release attempts to push the generated formula to
`Formula/agentdocker.rb` on its own. If the token is ever revoked the
release job warns and carries on — the formula and the cask are attached
to the run either way and can be copied across by hand, because a
missing distribution channel is not a reason to fail a build, and
`publish` needs that job to finish before it can upload the release
assets.

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

The template exists, but as of the September 9 GitHub check the tap's `Casks/`
directory contains only a README. The install command below becomes available
only after a release publishes the app cask:

```sh
brew install --cask brandopakel/tap/agentdocker-app
```

It `depends_on` the formula, so installing the app brings the commands
with it.

**It carries a caveat, and will until there is a Developer ID.** The app
is ad-hoc signed, so Gatekeeper refuses a downloaded copy. Homebrew's
supported way to say "I fetched this deliberately" is:

```sh
brew install --cask --no-quarantine brandopakel/tap/agentdocker-app
```

The cask says so in its own caveats. When the release is signed and
notarised, both the caveat and the flag go away.

## Apple Developer ID: what it is actually for

**It is not for the app icon.** That is fixed, in software, and works on
any Mac: the app sets its own at runtime, and the bundle carries an
`.icns`. Nothing about signing was ever involved in that, and this
document exists partly so nobody concludes otherwise again.

**It *is* for the notification icon, and that one is measured.**
A notification wears the icon of the bundle that posted it, and every
way of overriding that is closed — the `UserNotifications` framework
refuses a spoofed sender, which is why `terminal-notifier` withdrew
`-sender`, and an `osascript` notification belongs to Script Editor. So
AgentDocker posts its own, from `AgentDocker.app`, and the daemon runs
it in a one-shot `--notify` mode.

That path is written and it does not work yet, for one reason:

```
$ AgentDocker.app/Contents/MacOS/agentdocker-ui --notify AgentDocker test
notifications are not permitted: Notifications are not allowed for this application (1)
```

`UNErrorCodeNotificationsNotAllowed`. `UNUserNotificationCenter` will
not register a bundle without a stable signing identity, and an ad-hoc
signature has none:

| | signature | notifications |
|---|---|---|
| an app whose notifications work | `TeamIdentifier=Q6L2SF6YDW` | register, prompt, deliver |
| `AgentDocker.app` today | `Signature=adhoc`, `TeamIdentifier=not set` | refused, no prompt |

The daemon therefore falls back to `osascript`, which delivers with the
wrong icon. The ordering is deliberate: the right icon arrives the day
the signature does, with nothing to change here.

**It is for other people being able to open the app.** `scripts/bundle-macos.sh`
signs ad-hoc (`codesign --sign -`). That is a real signature and it is
enough for the machine that built the app. It is not enough for anyone
else: macOS refuses to open a downloaded application whose signature has
no Developer ID behind it, and the message the user gets is that the app
is damaged or cannot be verified. `spctl -a -t exec AgentDocker.app`
answers `rejected` today and `accepted` after notarisation.

So the $99/year Apple Developer Program membership buys exactly one
thing here: **strangers can run the download.** Concretely it enables

- signing the bundle with a Developer ID Application certificate,
- submitting it to Apple's notary service and stapling the ticket,
- therefore a `.dmg` or a Homebrew cask that opens on a first
  double-click rather than through right-click → Open or
  `xattr -d com.apple.quarantine`,
- **and notifications that carry the AgentDocker mark**, which is the
  more visible of the two if the only user is you.

**What works without it, today:**

- the CLI and the daemon, installed by `install.sh`, `cargo install`, or
  a Homebrew formula — command-line tools are not gatekept this way;
- the desktop app built from source on the user's own machine;
- the desktop app on your machine, and on any machine where the person
  is willing to right-click → Open once.

**The honest recommendation:** ship the CLI and daemon through the tap
now, and treat the signed, notarised `.app` as a separate step taken
when there is somebody to ship it *to*. The packaging pipeline already
accepts an identity and a notary profile — `packaging/desktop/package.py`
takes `--identity` and `--notary-profile` and verifies the result — so
when the certificate exists it is configuration, not work.

## Order

1. ~~Create the tap and point the release at it.~~ Done, and verified by
   installing from it.
2. ~~Set `HOMEBREW_TAP_TOKEN` so releases publish on their own.~~ Done.
3. Ship a release; confirm the formula updates and the cask appears.
4. Buy the Developer ID when the app is going to somebody who is not
   you. Then `--identity` and `--notary-profile` in the release
   workflow, and drop the cask's caveat. Everything up to here works
   without it.
