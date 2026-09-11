# Release assets and updates

The protected-tag workflow in `.github/workflows/release.yml` builds the CLI
downloads and four native desktop packages: Apple Silicon, Intel Mac, Linux
x86_64 and Linux ARM64. Desktop packages come from `package.py`, including the
`build.json` metadata required by the managed installer. The tag version must
match `Cargo.toml` and the recorded build must have clean source.
Release tags exclude `+build` metadata to match the Homebrew generator. Validation
and required Mac signing configuration are checked before compiling.

`packaging/desktop/release.py` prepares the archives, their checksums and a
separate manifest for each target. The feed job requires all four targets from
the same source/version/schema and verifies each actual archive before writing
`updates.json`. A prerelease tag writes `updates-preview.json` and remains a
GitHub prerelease. Preview packages require explicit local-preview acceptance.

Publication uploads all assets to a draft, then publishes it. A failed upload
leaves the draft for a retry. The workflow refuses to replace an already
published release. The Homebrew formula and cask use the same download checksums.
Create releases through this workflow, or leave a manually created release as a
draft for it to finish; a release already published in GitHub's UI is refused.
An older maintenance release cannot move the latest-update endpoint or the
Homebrew tap backwards. Prereleases do not change either stable distribution path.
The separate CLI-only Linux tarballs use musl; graphical Linux packages use GNU libc.
No workflow change publishes a release by itself: a protected version tag is
the trigger.

## Mac signing setup

Stable Mac releases require a Developer ID Application certificate and successful
notarization. Add these repository or organization secrets before releasing:

| Secret | Content |
| --- | --- |
| `MACOS_CERTIFICATE_BASE64` | Base64 of the exported `.p12` certificate and private key |
| `MACOS_CERTIFICATE_PASSWORD` | Export password; an empty password is supported |
| `MACOS_SIGNING_IDENTITY` | Full `Developer ID Application: …` identity |
| `MACOS_NOTARY_KEY` | App Store Connect team API private key, including PEM delimiters |
| `MACOS_NOTARY_KEY_ID` | API key ID |
| `MACOS_NOTARY_ISSUER` | Team API issuer ID |

The packaging step creates a temporary private keychain and notarization profile,
imports the certificate, signs and notarizes through `package.py`, then restores
the original keychain settings and removes the temporary keychain even if
packaging fails. Credentials are never release artifacts. The workflow uses
ephemeral GitHub-hosted Mac runners. Signing setup follows
[GitHub's certificate workflow](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications)
and [Apple's notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

Unsigned prerelease builds can be prepared with no signing secrets. Partially
configured signing is an error. A stable tag fails before publication if Mac
credentials or notarization are unavailable; it does not advertise an unsigned
build through the stable feed.

## Verification still needed

Local tests cover exact archive metadata, complete feed generation, damaged
downloads, channel separation and signing cleanup with fixture credentials.
The native packages are also built and exercised by pull-request desktop CI.
An actual protected-tag run with Apple credentials, hosted downloads through
`agentdocker desktop update`, and Gatekeeper acceptance on a clean Mac remain
release acceptance steps. The updater previews and explicitly applies a release;
it does not replace a running daemon or terminate active agents.
