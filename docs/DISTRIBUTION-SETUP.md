# Setting up distribution

Two things stand between a green release build and somebody else being
able to install AgentDocker. Neither is code, both are one-time, and
this is exactly what each one buys.

## The Homebrew tap

**What is missing:** nothing in the software. `packaging/homebrew/generate.py`
already produces a complete formula at release time, from the real
SHA-256 of each of the four release archives, and CI validates it with
`ruby -c`. It is attached to every release run as an artifact. What it
does not have is anywhere to live — a formula nobody can reach installs
nothing.

A "tap" is just a GitHub repository named `homebrew-<something>` with a
`Formula/` directory in it. That is the whole mechanism.

**One-time setup:**

```sh
gh repo create brandopakel/homebrew-tap --public \
  --description "Homebrew formulae for AgentDocker"
```

Then tell the release workflow where it is:

```sh
gh variable set HOMEBREW_TAP_REPOSITORY --body brandopakel/homebrew-tap
gh secret   set HOMEBREW_TAP_TOKEN      --body "<a PAT with contents:write on that repo>"
```

A fine-grained personal access token scoped to that one repository with
**Contents: read and write** is enough. It does not need access to this
repository.

From then on every tagged release pushes the generated formula to
`Formula/agentdocker.rb`, and anybody can install with:

```sh
brew tap brandopakel/tap
brew install agentdocker
```

Until then the release job says so in its log and carries on; a missing
distribution channel is not a reason to fail a build.

**What the formula does and does not cover.** It installs `agentdocker`,
`agentd` and the `agentdocker-ui` executable, and registers the daemon
as a Homebrew service. It does **not** install `AgentDocker.app`, because
a formula is the wrong vehicle for a GUI application — that wants a
*cask*, which is a separate file in the same tap and is worth adding
once the app is signed (see below, because an unsigned cask is a cask
nobody can open).

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

1. Create the tap and set the two release settings. Costs nothing,
   makes `brew install agentdocker` real.
2. Ship a release; confirm the formula lands and installs.
3. Buy the Developer ID when the app is going to somebody who is not
   you, then set `--identity` and `--notary-profile` in the release
   workflow and add a cask beside the formula.
