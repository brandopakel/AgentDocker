# Notification click routing audit

Added September 10, 2026 from the user's live report and screenshot: clicking
notifications repeatedly opens a blank, untitled Script Editor window instead of
the relevant location in AgentDocker. This is an open usability defect in the
[active delivery plan](DELIVERY-PLAN.md) and [remaining work](REMAINING-WORK.md).
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

## Work and acceptance

1. Identify the actual daemon, app bundle, notification sender and source version
   used by the installed launcher. Capture bounded native-post failure reasons
   without logging private message text. Check authorization, bundle registration
   and signing independently; reproduce with an explicitly owned notification.
2. Carry stable message/question, canonical agent and project identifiers from
   the envelope through posting. Define routing within the correct local daemon
   instance and resolve identifiers against current state, including reconciled
   legacy identities. Never interpret notification text as a command or path.
3. Implement native click handling and app activation, including cold launch and
   forwarding to an existing window. Bind it to the app's normal navigation so a
   question opens its reply view and a message opens its conversation/context.
4. Replace or constrain the AppleScript fallback so the supported click workflow
   reaches AgentDocker. If a platform cannot provide actionable notifications,
   expose that limitation and retain the inbox entry; do not report working
   click routing from a successful notification post.
5. Test real Notification Center clicks with the app active, hidden and closed;
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
