# Remaining engineering and release work

Updated September 12, 2026. This is the current backlog. Dated audits and
[verification reports](verification/) preserve the source-specific history;
older statements that an implemented feature is still missing are superseded here.

PRs #98–#111 have merged after final CI and actual source review (latest merge
`c47b2ce`). Delivered work includes the simpler Current/Needs input/History
session view, compact rows and Details, identity-safe discovery and offline
legacy duplicate repair, native notification routing, durable queues, opt-in
Claude and Codex idle input, shared human/peer submission, question/answer
receipts, structured approval controls, and the update consumer and scheduler.
These source changes do not replace the installed launcher or running daemon.

The [delivery-status follow-up](verification/2026-09-12-input-delivery-status.json)
adds durable receipts, queue counts, pause reasons and a compact read-only review
panel. Paused sessions remain visible after exit, and drafts survive review.
It also fixes a reproduced macOS socket error that hid saved session logs.
Local validation at `c116d28` passed 826 Rust tests, 65 Python checks, 137 native workflow
steps and 25 delivery/restart steps. Actual Codex and Claude trials verify
ordered receipts and replies with one provider identity; the report retains
failures, corrected test assumptions and each trial's scope. A prior-source
ten-minute Codex trial also passed 18 ordered inputs/replies and six idle wakeups.
Review corrections reject conflicting equal-timestamp reports and distinguish
log-read failures. Final CI and actual follow-up source review passed at `5778212`;
PR #108 merged as `1e29142`. One restored-window capture omitted labels; a later
28-step diagnostic and both physical-window captures displayed them correctly.
The original incomplete capture is retained and its cause remains unresolved.

The [Inbox history cleanup](verification/2026-09-12-compact-question-history.json)
collapses earlier direct questions to a short preview with explicit full-text
details. Notification navigation expands its target and preserves other drafts.
At `9525145`, 848 Rust tests, 65 Python checks, 29 native history/review/restart
steps and 23 additional notification-navigation steps passed. Final CI and source
review remain required; this is not physical Notification Center acceptance.

## Engineering still open

| Priority | Work | Completion condition | Supporting documents |
| --- | --- | --- | --- |
| Top priority; Claude and Codex bounded acceptance | Unified user/agent input queue and idle wake | Managed Claude channels and the owned Codex bridge now have actual idle, busy/mixed-input, question-answer and correlated-receipt evidence under one provider identity. Codex also passes controlled receipt/crash recovery. Compact durable delivery status and read-only guided review are merged with local acceptance and final CI/source review. [Bounded file-change review](verification/2026-09-12-file-change-review.json) now passes 848 Rust tests, 65 Python checks, 19 native review/draft/restart steps and actual Codex Allow/Deny with three ordered peer/human inputs each; final CI/source review remain required. Complete permission/MCP elicitation/secret-input presentation, broader actual-provider interruptions/reconnect and sustained conversations. Hooks alone remain insufficient for idle wake. | [Message delivery audit](MESSAGE-DELIVERY-AUDIT.md), [Codex input](CODEX-INPUT.md), [Claude question evidence](verification/2026-09-12-claude-question-queue.json) |
| High priority; routing implemented, physical acceptance open | Notification clicks open blank Script Editor | Native posting, destination metadata and existing/cold-window navigation are implemented; the AppleScript fallback is removed in source. Complete actual Notification Center click trials, signed posting and installed-launcher acceptance while preserving drafts and handling expired targets. The running old installation still needs the safe switch. | [Notification routing audit](NOTIFICATION-ROUTING-AUDIT.md), [active delivery plan](DELIVERY-PLAN.md) |
| Next | Safe live daemon replacement | Pending questions now retain answer routing across restart, with atomic message fanout and closure. Checked, bounded event continuation now has [local 837-test and actual CLI/daemon restart/schema-upgrade evidence](verification/2026-09-12-event-continuation.json); PR #109 passed final CI/source review and merged as `ebaba5be`. A [Codex event-only reconnect follow-up](verification/2026-09-12-provider-event-reconnect.json) also passes 839 Rust tests and an actual pending-approval socket cut; PR #110 passed final CI/source inspection and merged as `d89a85c`. Supervision now retains output tasks through final log flush before publishing exit; a [reproduced premature-exit log race](verification/2026-09-12-output-drain.json) is corrected with 842-test and actual pipe/PTY shutdown acceptance; PR #111 passed final CI/source review and merged as `c47b2ce`. Other RPC interruptions and full replacement remain open. See the [replacement design](LIVE-DAEMON-UPGRADES.md). Full replacement still must preserve child ownership, batch/PTY I/O, logs, identity, leases and schema compatibility; require the actual successor to be ready before retiring its predecessor, with failure recovery. `daemon reload` deliberately returns unavailable today. | [Architecture](ARCHITECTURE.md#sessions-and-persistence), [delivery plan](DELIVERY-PLAN.md) |
| Partial acceptance | Sustained-use bounds and unresolved performance failures | A stable schema-9 checkpoint passed ten minutes each at 1/10/100 agents (255,891 cycles); the final package passed actual crash/schema-upgrade and distinct-source installation/rollback trials. The [immutable schema-11 daemon](verification/2026-09-11-hour-sustained-use.json) also passed one hour at 100 agents and 10,000 files: 1,392,836 cycles, unchanged hashes and clean child cleanup. Overnight, actual-provider queues, reboot/sleep, broader growth/retention and checkout workloads remain. Diagnose the retained incomplete Iced capture; a later correct physical-window/capture trial does not explain it. Diagnose the retained socket timeout; a fresh passing diagnostic campaign does not explain it. | [Current verification](verification/2026-09-10-desktop-delivery.json), [testing standard](TESTING-AND-BENCHMARKS.md), [local trial](LOCAL-TRIAL.md) |
| Release; consumer, producer and scheduler implemented | Download/update distribution | CLI and Settings update check/download/preview/apply exist with local fixture evidence. The opt-in daily scheduler persists its attempt before checking, preserves install previews and throttles failures across restart. Release automation prepares package.py archives and verified stable/preview feeds. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and target Linux acceptance. Registry Cargo publication is not an established supported route. | [Daily checks](verification/2026-09-11-daily-updates.json), [desktop distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md), [distribution setup](DISTRIBUTION-SETUP.md) |
| Platform | Linux delivery acceptance | ARM64/x86-64 Linux and Mac graphical/package CI, including update-consumer scenarios, passed checkpoint `d630d9f`. Target-distribution desktop/service/package trials and independent hardware acceptance remain gates. | [Current verification](verification/2026-09-10-desktop-delivery.json), [product direction](PRODUCT-DIRECTION.md), [local trial](LOCAL-TRIAL.md) |
| Platform | Full native Windows product | Integrate the daemon and clients with named pipes; finish supervised lifecycle, ConPTY, identity-safe stopping/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and native graphical CI. Core/host/desktop adapter coverage is only a foundation. | [Windows port](WINDOWS-PORT.md), [architecture](ARCHITECTURE.md) |
| Range selection accepted on this Mac; broader input trials open | Terminal selection and richer interaction | Range selection/copy retains a bounded visible-grid snapshot, preserves Unicode and wrapped text, and releases it after copy or resumed input. The [selection checkpoint](verification/2026-09-11-terminal-selection.json) passed 762 Rust tests, 58 Python checks, 114 native workflow steps and an actual macOS drag/copy/changed-output trial with clipboard restoration. Complete human accessibility/IME and other-platform input trials, and repair observed defects. | [Iced contracts](ICED-DESIGN.md), [desktop guide](DESKTOP-UX.md) |
| Acceptance | Removed-checkout conflict fix in installed app | Source ignores events for vanished checkout roots and reports lost coverage. The [macOS follow-up](verification/2026-09-11-macos-watcher-recovery.json) reproduces a separate surviving-file event loss during watch reconciliation and fixes it with independent checkout streams; both macOS regressions passed 100 repetitions without retries. Verify after the safe launcher/daemon switch; historical conflict channels remain. | [Bulk receipts and watcher evidence](verification/2026-09-10-bulk-receipts.json) |
| Later | Optional expansion | Authenticated federation/host namespaces and cross-host lease/routing semantics; additional provider/desktop adapters and engine capabilities such as image-declared volumes. Keep these behind a dependable single-host desktop. | [Product direction](PRODUCT-DIRECTION.md), [architecture](ARCHITECTURE.md), [containers](CONTAINER-ENGINES.md) |

## Manual and operational steps

| Step | What remains | Engineering dependency |
| --- | --- | --- |
| Apple signing/notarization | Supply a Developer ID Application identity and private notary profile; run the existing signing, notarization, stapling and Gatekeeper flow on the final app/DMG, then publish verified artifacts. | Packaging automation exists. Credentials, actual service acceptance and release publication remain; local ad-hoc signing is only preview evidence. |
| Hands-on accessibility/input methods | Trial VoiceOver on macOS and the corresponding screen reader on supported Linux/Windows builds; exercise Tab/Shift-Tab, activation, visible focus, zoom, IME composition, Unicode and terminal copy/paste. | Native accessibility adapters and automated control tests exist. Human findings can create further engineering work. |
| Switch the old launcher after sessions finish | Verify the built package and installation preview, account for provider/service paths, then activate it and verify the app/CLI/daemon versions. End active work normally before any daemon replacement; keep rollback available. | Updating a launcher affects future launches. It does not upgrade a running daemon. Safe live replacement remains the separate engineering item above. |
| Independent release acceptance | Run a second-Mac trial, Intel hardware acceptance, target Linux trials and sustained actual-provider sessions against the final candidate. | Historical single-machine/provider evidence and Rosetta execution do not cover these stages. |

The signing/installation commands and private credential handling are in
[Desktop distribution](DESKTOP-DISTRIBUTION.md). The sequence and pass conditions
for human and machine trials are in [Local trial](LOCAL-TRIAL.md).

## Historical evidence and release configuration

Use the [delivery plan](DELIVERY-PLAN.md), [message audit](MESSAGE-DELIVERY-AUDIT.md)
and [verification directory](verification/) for the implementation and review
history, including failed trials. Historical passing tests do not complete a
new candidate's release or hands-on acceptance gates.

A September 9 read-only GitHub check found the Homebrew tap formula at v0.1.0,
the publishing variable and token secret name configured, and only a README in
`Casks/`. Creating the tap is complete; publishing a newer verified release and
its app cask remains. That dated check did not inspect secret values or verify
token validity. See [Distribution setup](DISTRIBUTION-SETUP.md).
