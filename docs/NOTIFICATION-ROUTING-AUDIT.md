# Notification click routing audit

Added September 10, 2026 from the user's live report and screenshot: clicking
notifications repeatedly opens a blank, untitled Script Editor window instead of
the relevant location in AgentDocker. Native posting and bounded installed click
routing now pass on this Mac after the approved notification-permission change.
Broader acceptance remains in the [active delivery plan](DELIVERY-PLAN.md) and
[remaining work](REMAINING-WORK.md).
The screenshot stays private; this document records only the reported behavior.

## Required behavior

Clicking an AgentDocker notification must activate the appropriate AgentDocker
window and navigate to the originating project, agent, message or pending
question. It must work while the app is foregrounded, backgrounded or closed.
Preserve drafts, use the canonical agent identity, and avoid opening another
window unnecessarily. An expired or unavailable destination must have a clear
in-app fallback without silently selecting another agent.

## Initial source findings

Read-only inspection at code checkpoint `5772736` found:

- [`host::notify::candidates`](../crates/host/src/notify.rs) first tries the
  installed app's `--notify` mode, then falls back to `osascript -e` with
  `display notification`. The fallback's Script Editor attribution is already
  described in the source. This matches the reported symptom, but the exact
  installed poster and failure causing this particular notification were not
  captured. Apple documents that an AppleScript notification's Show action opens
  the application that displayed it.
  [Apple notification guide](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/DisplayNotifications.html).
- [`Notice`](../crates/agentd/src/daemon/humans.rs) discards envelope routing IDs
  and retains only the sender's display name, kind and text. The host
  `Notification` and [`--notify`](../crates/ui/src/main.rs) carry only title/body.
- [`notify::post`](../crates/ui/src/notify.rs) creates native notification content
  without destination metadata. No notification-response delegate or navigation
  handler was found in the UI source. Apple's response-handler interface is a
  candidate for delivering the user's click into app navigation.
  [Apple notification response reference](https://developer.apple.com/documentation/usernotifications/handling-notifications-and-notification-related-actions).
- The host tries hard-coded installed app paths and discards poster stderr. The
  source attributes native posting failures to signing, but this inspection did
  not independently establish the failure reason on the user's installed build.

Signing, notification authorization and click routing require separate evidence.
Paying for developer membership or signing an app does not add the missing route
metadata and response handling. Signing/notarization remains a release gate;
correct notification navigation is an engineering requirement. No payment,
certificate change, installation switch or test notification was performed for
this read-only diagnosis.

## Implementation in the current change

- The daemon retains message, sender, project and channel IDs plus its exact home
  and socket. Native posting uses `--notify-json`; title/body remain display data.
  macOS no longer falls back to AppleScript. A posting failure leaves the message
  in Inbox and emits a bounded diagnostic without copying child output or content.
- Native notification `userInfo` carries the validated destination, and a retained
  `UNUserNotificationCenterDelegate` receives default clicks. Older notices with
  no metadata open Inbox; dismissal does not navigate. Authorization timeout now
  fails explicitly instead of falling through to posting.
- A private, bounded activation socket forwards another launch to the existing
  window for that daemon origin. A native click for another origin launches the
  same executable with child-only home/socket settings. The receiver validates
  origin and message IDs, refreshes daemon snapshots, and reveals the actual
  question or message. Missing destinations show an Inbox fallback; manual
  navigation cancels a pending route. No click submits or rewrites a draft.
- Ordinary Iced workflow fixtures set `AGENTDOCKER_NO_NOTIFICATIONS=1`. The new
  `scripts/notification_smoke.py` exercises real processes, private IPC, old
  messages, two projects and preserved drafts. It explicitly does not claim to
  simulate a Notification Center click.

The first two debug-build navigation trials reached the older direct message,
then exceeded their 30-second channel-transition gate. The second retained a
process sample showing the main thread spending its samples in software drawing.
The same 30-second gates passed against the release build: 21 existing-window
steps and two cold-launch steps in 9.92 seconds, with eight routing/draft checks,
unchanged executable hashes and no surviving fixture children. Captures show the
older direct and channel target in view. The combined standard gate passed 695
Rust tests (six skipped), 48 Python checks and 105 release workflow steps.
The packaged local preview also passed all 23 navigation steps in 10.31 seconds.
An explicit native post returned exit 1 and `Notifications are not allowed for
this application (1)`; the foreground window reported the same permission refusal.
The bundle is ad-hoc signed and this machine reports zero valid signing identities.
That evidence does not isolate membership, signing or bundle-registration causation.
No native click could be tested from the refused post. Signed builds and old/new
bundle registration remain open; the installed launcher and daemon are unchanged.
[Exact-source verification](verification/2026-09-10-notification-routing.json).

The September 14 installed-app check at `aaa1b61` again refused one explicitly
owned native post through `/Applications/AgentDocker.app`: exit 1,
`notifications are not permitted`, `UNErrorDomain error 1`. Accessibility
automation was available, but no notification appeared to click. The private
trial retained the installation identity and poster hash; no production message,
draft or provider state changed. This confirms the posting prerequisite is still
unmet, without identifying signing or membership as its sole cause.

The same September 14 check found that the installed real launcher fails
`codesign --verify --deep --strict` with an invalid Info.plist: it uses
`dev.agentdocker.launcher` metadata around a linked executable signed inside
`dev.agentdocker.desktop`. The immutable native payload passes signature
verification, but a direct post from that bundle also receives error 1. Both
bundle identifiers have Launch Services records. Therefore bundle integrity is
an established installed-bundle defect, while its contribution to the posting refusal
remains unresolved. Do not re-sign through the installed executable links:
that would modify an immutable release. A private alternative using a linked
`Contents` directory also failed strict verification with unsealed root contents;
that approach is not a validated fix. No installed files or settings changed.

The September 15 repair replaces the neutral wrapper with an intact payload copy and keeps ownership outside its signature. All three entry points redirect to the selected immutable release, retaining hook/MCP and GUI roles. Private linked-executable alternatives failed strict verification and were rejected. The full gate at clean `cf64ca3` passed 979 Rust tests (seven skipped) and 71 Python checks. Private installation passed 13 scenarios; an actual separately built pre-capability `b605f8e` → `cf64ca3` → `b605f8e` upgrade/rollback passed 12 scenarios with strict signature verification, hook/MCP compatibility, selected-release execution and live-daemon retention. Routing through the copied launcher passed 24 existing-window and two cold-start steps, preserving drafts and targets with no surviving fixture processes. The first older-release trial failed an alias-path assertion in the harness; its corrected rerun and the original failure are retained in the [launcher evidence](verification/2026-09-12-launcher-hook-repair.json). PR #153 is merged. The verified `cf64ca3` package, with production inputs identical to merged `4074275`, is now installed at `/Applications/AgentDocker.app`; strict signature verification and legacy CLI paths pass, the new app is open, and all four external provider identities survived the backed-up coordinator switch. These checks do not establish native posting authorization or physical Notification Center clicks.

The installed intact app still refused two valid native posts, including after
Launch Services activated its existing window: `Notifications are not allowed
for this application (1)`. Strict signature verification passed. An initial
malformed-destination probe was rejected before native posting and is retained
separately. These results are in the existing launcher evidence; no notification
settings or provider state changed.

The authorization follow-up moves the one-time permission request from app
construction to the first focused-window event. `agentdocker-ui
--notification-status`, run from the app bundle, reads its bundle identity and
macOS authorization/alert/sound/Notification Center settings without prompting or
posting. It uses Apple's [notification settings query](https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/getnotificationsettings(completionhandler:)).
This provides evidence to distinguish a denied setting from an unsupported
notification client. In the initial trial, the packaged diagnostic reported
`denied` authorization before and after 26 passing rendered navigation steps.
No physical focus event was established, and a subsequent console-state query
found the screen locked. That trial did not establish a signing/payment or
startup-timing cause. The full candidate gate passed 979 Rust tests and 71
Python checks. Permission was left unchanged pending the user's decision;
the approved follow-up below establishes posting and click behavior separately
from the first-focus request, which remains under acceptance.

## Installed Notification Center acceptance (September 15)

With the user's explicit approval, **Allow notifications** was enabled for
AgentDocker in macOS System Settings. The read-only diagnostic changed from
`denied` to `authorized`. The installed `cf64ca3` binaries and ad-hoc signature
were unchanged: native posts then succeeded. This verifies that the saved
permission blocked these local posts; no Developer ID identity or payment was
needed for this bounded local trial.

Actual Notification Center accessibility presses opened the exact Codex
message with the app backgrounded and foregrounded, preserved another
conversation's unsubmitted draft, opened a pending question and its answer
field, and showed the unavailable-message fallback after an owned message was
cleared. The end-to-end case sent an ordinary message through the production
daemon, which posted the notification automatically; the click opened that
message without a direct notification CLI call. The production GUI and daemon
kept their PIDs, with no Script Editor or unnecessary production window observed.

A separate click also opened a previously absent private daemon-origin window
on a UI binary whose hash matched the installed app. That window and private
daemon were retired, while the production window stayed open. This is narrower
than a cold OS launch with no AgentDocker GUI process. Test questions/messages
were cleaned up, the test draft returned to its original empty value, and strict
bundle signature verification still passed. Initial incomplete and harness-failed
attempts remain in the [existing launcher record](verification/2026-09-12-launcher-hook-repair.json).

Still open: installed-release explicit-Hide restoration (the fix has passed
only on a private candidate window), zero-process app launch, old AppleScript notifications,
expired-question variants, archived-history and broader project/account cases,
and Developer ID/notarized release acceptance. Notification Center accessibility
actions establish native activation; they do not establish physical mouse,
VoiceOver or IME usability. PR #158's first-focus request is separate from this
installed-release trial and is not credited as the permission fix.

## September 16 hidden-window follow-up

An actual Notification Center click on installed `1e90f83` reached its exact
private-origin message, but the explicitly hidden application remained hidden.
Window de-minimization and focus alone did not establish app visibility in that
trial. Source now requests macOS application unhide before Iced window focus.
Candidate `2de5994` passed a real Notification Center click with the target
application explicitly hidden: the exact message was selected and app visibility
became true. The installed poster forwarded to the private candidate window;
the production window and provider sessions were retained. The full gate passed
1,013 Rust tests (seven skipped), 77 Python checks, lint, packaging and release
build. Both failed visibility trials and the initial fixture-expression failure
remain in the [launcher evidence](verification/2026-09-12-launcher-hook-repair.json).
Zero-process OS launch and broader release acceptance remain open.

## Installed already-open-app click (September 17)

On installed `f5e298f4` (source `418fbc9`, carrying PR #175), with the
production app explicitly hidden, the daemon posted an ordinary message and
its owned Notification Center notification was pressed through accessibility:
the app became visible with the exact message row (`26beba01be15456f`)
revealed, and the GUI and both provider processes were unchanged. This closes
the person's finding that a click while the app was already open did nothing,
for that installed revision. Recorded in the
[integrated record](verification/2026-09-12-integrated-desktop.json) under
`installed_notification_click_2026_09_17`. Later the same day, with no app
process before the post or the press, the same kind of press launched the
installed app and revealed the message (`0691fa9fd985467f`) in 1.7 s with both
provider generations unchanged (`installed_cold_notification_click_2026_09_17`),
closing zero-process cold launch for that revision. Still open: a physical
mouse click, a click whose message lies outside the loaded pages (the bounded
page-back path), and the same on a signed release and on the next candidate.

## Work and acceptance

1. (Partial: installed app/daemon identity verified; original notification poster not captured.) Identify the actual daemon, app bundle, notification sender and source version
   used by the installed launcher. Capture bounded native-post failure reasons
   without logging private message text. Check authorization, bundle registration
   and signing independently; reproduce with an explicitly owned notification.
2. (Done.) Carry stable message/question, canonical agent and project identifiers from
   the envelope through posting. Define routing within the correct local daemon
   instance and resolve identifiers against current state, including reconciled
   legacy identities. Never interpret notification text as a command or path.
3. (Done in source: `crates/ui/src/notification_route.rs`.) Implement native click handling and app activation, including cold launch and
   forwarding to an existing window. Bind it to the app's normal navigation so a
   question opens its reply view and a message opens its conversation/context.
4. (Done: the fallback is removed, `crates/host/src/notify.rs`.) Replace or constrain the AppleScript fallback so the supported click workflow
   reaches AgentDocker. If a platform cannot provide actionable notifications,
   expose that limitation and retain the inbox entry; do not report working
   click routing from a successful notification post.
5. (Partial: installed foreground/background message, pending-question, stale-message and private-origin clicks pass; candidate explicit-Hide restoration also passes, while installed-release Hide acceptance remains open.) Complete real Notification Center clicks with the app hidden and fully closed;
   multiple projects/agents; pending and expired questions; retained history;
   stale notifications; old/new app installations; denied notification access;
   unsigned/local-preview and signed release candidates. Assert the correct
   destination, preserved draft and no duplicate window or Script Editor launch.
6. Keep ordinary workflow fixtures from posting unintended desktop notifications;
   reserve visible notices for explicit notification acceptance. Record sanitized
   source/version results and clean up only notifications belonging to the trial.

This work complements the [provider input queue audit](MESSAGE-DELIVERY-AUDIT.md).
A human clicking a desktop notification is distinct from an idle provider being
woken through its input queue. Neither behavior establishes the other.

The [schema-10 checkpoint](verification/2026-09-10-durable-queue.json) pins clean source `a9b54b5` and matching immutable binaries: 701 Rust tests, 48 Python checks, five actual queue scenarios, seven restart/upgrade checks, 105 native workflow steps and 23 notification-navigation steps passed. Provider input acceptance/idle wake and physical Notification Center clicks remain distinct open gates.
