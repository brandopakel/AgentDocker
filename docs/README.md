# Documentation

AgentDocker runs native agents directly on the user's computer. Docker and Podman
are optional execution adapters. The current full host implementation supports
macOS and Linux; Windows has tested foundations and remains incomplete.

- [Current remaining engineering and manual release steps](REMAINING-WORK.md)
- [Getting started, adapters and working sets](../README.md)
- [Build and install the current desktop locally](LOCAL-BUILD.md)
- [Using AgentDocker: the app, the console, every command, tutorials](GUIDE.md)
- [The desktop app, screen by screen: what every control does and what it will not claim](DESKTOP-UX.md)
- [Setting up distribution: the Homebrew tap, and what a Developer ID is actually for](DISTRIBUTION-SETUP.md)
- [Release archives, update feeds and signing automation](RELEASE-AUTOMATION.md)
- [Product direction and current delivery order](PRODUCT-DIRECTION.md)
- [Dax and herdr research notes, September 11](LANDSCAPE-2026-09-11.md)
- [Active delivery plan, commit/PR review and complete testing crosswalk](DELIVERY-PLAN.md)
- [September 7 review scope, evidence and gap ledger](REVIEW-2026-09-07.md)
- [Engineering audit, feature coverage and known blockers](AUDIT-2026-09-06.md)
- [Native delivery progress and regression coverage](NATIVE-DELIVERY.md)
- [Local native trial and acceptance plan](LOCAL-TRIAL.md)
- [Architecture and wire protocol](ARCHITECTURE.md)
- [Implementation and recovery contracts](IMPLEMENTATION-NOTES.md)
- [Optional Docker and Podman execution](CONTAINER-ENGINES.md)
- [Testing and benchmarks](TESTING-AND-BENCHMARKS.md)
- [Real-engine verification](../tests/containers/README.md)

Historical phase numbers are dependency sequence numbers, not GitHub PR numbers. The product-direction page defines upcoming priorities; command-specific `--help` describes the installed binary.

Native delivery follow-ups: [desktop packaging and installation](DESKTOP-DISTRIBUTION.md), [guided setup and health checks](GUIDED-SETUP.md), [bounded real-provider acceptance](INTEGRATION-ACCEPTANCE.md), and the [implementation/acceptance tracker](NATIVE-DELIVERY.md). The [macOS runner workaround](TEST-RUNNER-MACOS.md) preserves strict leak detection while avoiding captured cross-test descriptor inheritance.

- [Claude channel input](CLAUDE-CHANNEL-INPUT.md) — explicit local opt-in, retained offers/receipts and provider acceptance limits.

- [Portable AgentDocker coordination skill](../crates/cli/skills/agentdocker/SKILL.md) — requested September 15; one source for provider setup and MCP onboarding. Private Codex/Claude loader discovery passed; full integration and other-provider acceptance remain.

## Existing-document audit, September 14, 2026

The September 14 audit covered **37 tracked Markdown files**: 34 here, the root README and
coding instructions, and the container test README. It also checks the **61
existing verification JSON reports**, source contracts, merged PRs and current
CI/build state. No additional plan or report is needed: current open work stays
in [Remaining work](REMAINING-WORK.md), test status stays in the
[delivery crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk), and this table
records coverage. The September 15 portable skill adds one requested instruction
asset, bringing the tracked inventory to 38; it is not another project plan. The inventory baseline is `aaa1b61`, PR #119; current outcomes
below include merged PRs #125–#128 and #130, and the reviewed follow-ups in
PRs #129 and #131. Remaining acceptance is not an open implementation claim.

**Reference** means the document describes an implemented workflow or engineering
rule; it is not a release certificate. **Partial** means named implementation or
acceptance remains. **Historical** means the audit/probe itself is complete and
its original evidence stays intact; unresolved findings are carried into the
current tracker. **Deferred** identifies existing optional proposals.

| Existing document | Audit outcome / remaining scope |
| --- | --- |
| [Root README](../README.md) | Reference: implemented single-host features and usage. The duplicated roadmap is removed in PR #131; unfinished engineering remains in the existing Remaining work document, including Windows and deferred federation. |
| [Coding instructions](../CLAUDE.md) | Reference: source layout, one build campaign, strict verification and source review. The pure-core rule holds: environment reads live in the host crate. |
| [This index](README.md) | Reference: complete file inventory and one current backlog/crosswalk. |
| [Remaining work](REMAINING-WORK.md) | Partial: current disposition of existing engineering, acceptance and release requirements. |
| [Delivery plan](DELIVERY-PLAN.md) | Partial: current sequence and T01–T12/L01–L15 status; old checkpoints are historical. |
| [Product direction](PRODUCT-DIRECTION.md) | Partial: single-host implementation exists; release/platform delivery remains, federation deferred. |
| [Architecture](ARCHITECTURE.md) | Partial: protocol/phase inventory, journal/checkpoint maintenance and an environment-free core exist; live replacement and Windows are incomplete. Optional protocol proposals remain deferred. |
| [Implementation notes](IMPLEMENTATION-NOTES.md) | Reference: implemented coordination/recovery contracts; distinguish command relaunch from conversation restoration. |
| [Guide](GUIDE.md) | Reference: command/tool inventory and current Tools/terminal navigation reconciled with source. |
| [Desktop UX](DESKTOP-UX.md) | Implemented home/Inbox/Tools simplification and capability evidence. PR #129 adds readable default launch names, narrow conversation navigation and cancellation of delayed answer navigation; the follow-up gate passed 916 Rust tests and 70 Python checks. Physical input and installed-candidate acceptance remain. |
| [Iced design](ICED-DESIGN.md) | Partial: native migration and automated interactions implemented; VoiceOver/IME and other-platform hands-on acceptance remain. |
| [Guided setup](GUIDED-SETUP.md) | Implemented preview/apply/undo, configuration locks and per-session contact/input evidence. PR #125 is merged after review and CI; native and bounded actual-provider readiness checks passed. Installed-candidate acceptance remains. |
| [Activity and messaging](ACTIVITY-AND-MESSAGING.md) | Partial: activity/hooks and opt-in input adapters exist; ordinary hooks alone cannot wake idle models. |
| [Message delivery audit](MESSAGE-DELIVERY-AUDIT.md) | Implemented durable shared queue and exact receipts. PR #131 adds provider availability, quota isolation, queue gating and recovery, with actual isolated Claude/Codex trials, 135 runtime/interruption cases and [184 native steps at `66c4247`](https://github.com/brandopakel/AgentDocker/pull/131#issuecomment-5673268646). Combined `ec45cea` also passed mid-tool, pending-denial and same-identity controller replacement cases. Broader reviews, provider/account-reset and sustained acceptance remain. |
| [Codex input](CODEX-INPUT.md) | Partial: managed bridge and existing-terminal native queue implemented; six actual Codex 0.154.0 release trials and the 962-Rust/70-Python gate passed at `7133023`. PR #148 is merged; one installed existing-session auto-bootstrap, peer wake and exact receipt passed. Zero-prompt startup/reopen, broader elicitation/secret input and historical reconciliation remain. |
| [Claude channel input](CLAUDE-CHANNEL-INPUT.md) | Partial: explicit channel input and question queue implemented; authorization/version and longer recovery acceptance remain. |
| [Notification routing audit](NOTIFICATION-ROUTING-AUDIT.md) | Partial: fallback removed; signed-copy launcher installation, actual older-release rollback and 26 copied-launcher navigation steps passed. Production activation and strict signature verification passed; physical clicks and signed posting remain. |
| [Identity repair](IDENTITY-REPAIR.md) | Partial: offline preview/apply implemented and tested; production reconciliation and live transfer not done. |
| [Live daemon upgrades](LIVE-DAEMON-UPGRADES.md) | PR #130 merged independent process/I/O ownership and durable exit recovery. Actual distinct-source crash/restart acceptance passed, alongside event continuation, reconnect and output drain. PR #155 implements graceful replacement, successor fencing/readiness and failed-transfer recovery behind the experimental reload gate; review and acceptance remain open. Explicit shutdown still stops managed agents. |
| [Native delivery](NATIVE-DELIVERY.md) | Partial: merged implementation record; installation complete on this Mac, broader release acceptance incomplete. |
| [Local build](LOCAL-BUILD.md) | Reference: build, Applications installation, compatibility paths, rollback and the two-way `install.sh`/managed-install protection implemented; the backed-up coordinator switch to verified `cf64ca3` is complete on this Mac with all four external provider identities preserved. |
| [Desktop distribution](DESKTOP-DISTRIBUTION.md) | Partial: packaging, installer, update consumer and retention implemented; signed-copy launcher and actual older-release rollback passed private acceptance. Production activation passed; public signing and final distribution acceptance remain. |
| [Distribution setup](DISTRIBUTION-SETUP.md) | Partial: tap/formula and automation exist; Developer ID, notarization and published app cask remain. |
| [Release automation](RELEASE-AUTOMATION.md) | Partial: archives/feed workflow implemented; protected-tag execution, hosted update and clean-Mac acceptance remain. |
| [Integration acceptance](INTEGRATION-ACCEPTANCE.md) | Historical bounded trials: later source/runtime evidence is in the input guides and verification reports; no universal-provider claim. |
| [Local trial](LOCAL-TRIAL.md) | Partial: isolated/native/provider/local installation trials exist; the overnight stage has a [7.5-hour record](verification/2026-09-14-overnight-sustained-use.json) and retention a [20-minute record](verification/2026-09-15-retention-sustained-use.json); sleep/reboot and independent-machine stages incomplete. |
| [Testing and benchmarks](TESTING-AND-BENCHMARKS.md) | Partial: standard/CI/fuzz/benchmark tools and the sustained-use and retention workload scripts exist with their records; failure diagnosis and platform matrices incomplete. |
| [macOS test runner](TEST-RUNNER-MACOS.md) | Reference: reproduced descriptor inheritance and validated strict serial workaround; remove only after an upstream fix passes its controls. |
| [Windows port](WINDOWS-PORT.md) | Partial: native core/host/named-pipe foundations; full daemon/GUI, ConPTY, service and installer remain. |
| [Container engines](CONTAINER-ENGINES.md) | Partial: optional engines/workspaces implemented; Mac engine acceptance and documented unsupported capabilities remain. |
| [Container test README](../tests/containers/README.md) | Reference with partial acceptance: separate engine/lifecycle/relay fixtures; Linux CI and earlier Mac Podman evidence do not establish current Docker Desktop acceptance. |
| [September 4 audit](AUDIT-2026-09-04.md) | Historical: retain original baseline; current fixes and open work supersede its status. |
| [September 6 audit](AUDIT-2026-09-06.md) | Historical: restore/privacy fixes and pure-core environment cleanup are complete; broader acceptance stays in the current tracker. |
| [September 7 review](REVIEW-2026-09-07.md) | Historical: merged review stacks closed; retained timeout/acceptance findings remain in the current tracker. |
| [September 8 review](REVIEW-2026-09-08.md) | Historical: implementation follow-ups merged; original failures remain source-specific evidence. |
| [September 11 landscape](LANDSCAPE-2026-09-11.md) | Historical research: adopted UI ideas implemented; remaining suggestions are optional, not a new release checklist. |
| [herdr bridge](HERDR-BRIDGE.md) | Deferred beyond implemented pane identity: focus/prompt bridge and reported-blocked mirror are proposals, not delivered features. |

Verification reports preserve the original trials, rather than representing
independent tasks to rerun or close. A later reviewed merge resolves a report's
old integration status without changing its source, failed result or acceptance
scope. New evidence can extend an existing report or the relevant PR; do not
create another overlapping plan.

## Verification records

One line per record under `docs/verification/`, taken from the record's own
status. The section between the markers is generated: run
`python3 scripts/docs_check.py --write-index` after adding a record; the gate
fails when it and the records disagree.

<!-- verification-index:start -->

| Record | Says |
| --- | --- |
| [2026-09-07-claude-profile-setup.json](verification/2026-09-07-claude-profile-setup.json) | portable_coordination_skill_2026_09_15: Combined full release gate, independent source review, actual setup/discovery and native-TUI queue acceptance passed; final CI and installation pending. |
| [2026-09-07-desktop-maintenance.json](verification/2026-09-07-desktop-maintenance.json) | owned native desktop fixtures; private paths and captures excluded |
| [2026-09-07-identity-lifecycle.json](verification/2026-09-07-identity-lifecycle.json) | Packaged adapter lifecycle with a synthetic host; separate from actual model-provider trials |
| [2026-09-07-integration-benchmark-failure.json](verification/2026-09-07-integration-benchmark-failure.json) | Original failed integrated benchmark; not a completed performance acceptance campaign |
| [2026-09-07-local.json](verification/2026-09-07-local.json) | Selected exact-source local campaigns; this is not final release acceptance. |
| [2026-09-07-macos-capture-failure.json](verification/2026-09-07-macos-capture-failure.json) | failed |
| [2026-09-07-native-resources.json](verification/2026-09-07-native-resources.json) | sanitized exact-source native desktop integration and short resource observations; raw logs, process identities and captures remain private |
| [2026-09-07-provider-activity.json](verification/2026-09-07-provider-activity.json) | Source-pinned native integration, actual provider callbacks and synthetic message trials; not complete delivery or release acceptance |
| [2026-09-07-state-timing-diagnostic.json](verification/2026-09-07-state-timing-diagnostic.json) | Separate opt-in state timing diagnostic campaign, not a diagnosis of the original failed campaign |
| [2026-09-07-terminal-resources.json](verification/2026-09-07-terminal-resources.json) | Terminal admission/lifecycle verification and short native Mac observation at the stated sources; not release acceptance |
| [2026-09-08-final-candidate-compile.json](verification/2026-09-08-final-candidate-compile.json) | failed |
| [2026-09-08-hook-input-baseline.json](verification/2026-09-08-hook-input-baseline.json) | reproduced Claude stdin wait with no timeout within the 1.5 s observation window |
| [2026-09-08-integrated-provider-trials.json](verification/2026-09-08-integrated-provider-trials.json) | setup_trial: passed |
| [2026-09-09-desktop-simplification.json](verification/2026-09-09-desktop-simplification.json) | passed |
| [2026-09-10-bulk-receipts.json](verification/2026-09-10-bulk-receipts.json) | Source-bound bulk receipts and removed-checkout regression checkpoint; not provider idle-wake or public release acceptance. |
| [2026-09-10-button-interaction.json](verification/2026-09-10-button-interaction.json) | Primary button text contrast during pointer interaction; follow-up to desktop delivery verification |
| [2026-09-10-claude-channel-probe.json](verification/2026-09-10-claude-channel-probe.json) | Owned Claude provider capability probes; no production AgentDocker outbox implementation yet |
| [2026-09-10-codex-delivery.json](verification/2026-09-10-codex-delivery.json) | passed |
| [2026-09-10-desktop-delivery.json](verification/2026-09-10-desktop-delivery.json) | Iced usability, provider lifecycle delivery, durable coordination, package acceptance and bounded resource trials |
| [2026-09-10-desktop-design.json](verification/2026-09-10-desktop-design.json) | Iced desktop design rounds of 2026-09-10: visual, interaction and idle-cost verification for commits 4f5b921, 17ca37a, 00c1c98, 4f0f579 on codex/desktop-delivery. |
| [2026-09-10-desktop-load.json](verification/2026-09-10-desktop-load.json) | UI load benchmark and memory bisect for the Iced desktop on 2026-09-10, packaged baseline binaries and a release UI rebuild for the fix; see per-campaign contention limits. |
| [2026-09-10-durable-queue.json](verification/2026-09-10-durable-queue.json) | standard_gate: passed |
| [2026-09-10-message-receipts.json](verification/2026-09-10-message-receipts.json) | standard_gate: passed |
| [2026-09-10-notification-routing.json](verification/2026-09-10-notification-routing.json) | release_workflow: passed |
| [2026-09-11-claude-channel-input.json](verification/2026-09-11-claude-channel-input.json) | implemented_and_partial_acceptance |
| [2026-09-11-codex-appserver-input.json](verification/2026-09-11-codex-appserver-input.json) | passed |
| [2026-09-11-codex-hook-discovery.json](verification/2026-09-11-codex-hook-discovery.json) | passed |
| [2026-09-11-codex-input-bridge.json](verification/2026-09-11-codex-input-bridge.json) | Experimental managed native Codex input, queue ownership and bounded crash recovery |
| [2026-09-11-codex-input-review.json](verification/2026-09-11-codex-input-review.json) | network_review_2026_09_15: Full release gate passed; actual managed-network callback acceptance remains open after a private configuration denied the connection before emitting a callback. |
| [2026-09-11-codex-queue-recovery.json](verification/2026-09-11-codex-queue-recovery.json) | passed |
| [2026-09-11-daily-updates.json](verification/2026-09-11-daily-updates.json) | passed_for_listed_scope |
| [2026-09-11-desktop-identity.json](verification/2026-09-11-desktop-identity.json) | Iced desktop identity round of 2026-09-11 on codex/desktop-delivery: per-project monogram tiles, the window-local unviewed-done badge, the Connections hooks copy and the per-launch Claude channel checkbox. |
| [2026-09-11-desktop-release.json](verification/2026-09-11-desktop-release.json) | implemented_and_local_acceptance_passed |
| [2026-09-11-desktop-update.json](verification/2026-09-11-desktop-update.json) | Update consumer ('agentdocker desktop update') verification on 2026-09-11: unit tests, lint and the offline update smoke against a locally packaged release under a disposable prefix. |
| [2026-09-11-followup-integration.json](verification/2026-09-11-followup-integration.json) | local_gates_passed_final_ci_pending |
| [2026-09-11-hour-sustained-use.json](verification/2026-09-11-hour-sustained-use.json) | passed_for_listed_scope |
| [2026-09-11-identity-repair.json](verification/2026-09-11-identity-repair.json) | Explicit offline reconciliation of proven local external Claude/Codex duplicate records; durable exact aliases and preserved history. |
| [2026-09-11-macos-watcher-recovery.json](verification/2026-09-11-macos-watcher-recovery.json) | passed |
| [2026-09-11-managed-claude-input.json](verification/2026-09-11-managed-claude-input.json) | passed_bounded_managed_launch_acceptance |
| [2026-09-11-mcp-answer-receipts.json](verification/2026-09-11-mcp-answer-receipts.json) | Codex MCP ask_human answer receipts and restart recovery |
| [2026-09-11-provider-configuration.json](verification/2026-09-11-provider-configuration.json) | passed_for_listed_scope |
| [2026-09-11-provider-question-receipts.json](verification/2026-09-11-provider-question-receipts.json) | separate_mcp_question_gap: failed |
| [2026-09-11-recovery-fixtures.json](verification/2026-09-11-recovery-fixtures.json) | passed_for_listed_scope |
| [2026-09-11-retyped-drafts.json](verification/2026-09-11-retyped-drafts.json) | passed_for_listed_scope |
| [2026-09-11-session-messages.json](verification/2026-09-11-session-messages.json) | passed_for_listed_scope |
| [2026-09-11-structured-questions.json](verification/2026-09-11-structured-questions.json) | Structured Iced command and choice questions; rendered callbacks and owned actual Codex trials |
| [2026-09-11-terminal-selection.json](verification/2026-09-11-terminal-selection.json) | passed_for_listed_scope |
| [2026-09-12-claude-question-queue.json](verification/2026-09-12-claude-question-queue.json) | One Claude channel question-answer path with durable shared-queue receipts and reply metadata |
| [2026-09-12-cli-sender-identity.json](verification/2026-09-12-cli-sender-identity.json) | local acceptance and actual Claude shell sender trial passed; final CI/source review pending |
| [2026-09-12-compact-question-history.json](verification/2026-09-12-compact-question-history.json) | Compact retained Inbox questions, explicit complete-text details, and notification navigation without draft submission or message dismissal. |
| [2026-09-12-event-continuation.json](verification/2026-09-12-event-continuation.json) | Local review correction acceptance passed at 9dda825: 837 Rust tests, 65 Python checks and fresh real CLI/daemon restart/schema/exhaustion checks. |
| [2026-09-12-file-change-review.json](verification/2026-09-12-file-change-review.json) | Bounded Codex file-change approval through the shared human answer queue and compact native full-diff review. |
| [2026-09-12-input-delivery-status.json](verification/2026-09-12-input-delivery-status.json) | PR #108 merged as 1e2914270b59fcb6721d463999eed80229e15e5d after final 5778212 CI and actual source inspection. |
| [2026-09-12-integrated-desktop.json](verification/2026-09-12-integrated-desktop.json) | Combined PR119: messenger Inbox, minimal home and Tools, permission validation, sender identity, launcher compatibility and persisted Applications destination |
| [2026-09-12-launcher-hook-repair.json](verification/2026-09-12-launcher-hook-repair.json) | Historical hook repair and September15 intact launcher: private install/legacy rollback/routes and production activation passed; an existing Codex session auto-bound, then a peer message started its next ordinary turn... |
| [2026-09-12-output-drain.json](verification/2026-09-12-output-drain.json) | Managed output ownership through pipe/terminal EOF and final log flush before publishing exit, releasing protection or restarting; not cross-process daemon handover. |
| [2026-09-12-permission-review.json](verification/2026-09-12-permission-review.json) | local acceptance passed; final CI/source review pending |
| [2026-09-12-provider-event-reconnect.json](verification/2026-09-12-provider-event-reconnect.json) | Local implementation and actual Codex event-only reconnect acceptance passed at 687e57f. |
| [2026-09-12-queue-read-reconnect.json](verification/2026-09-12-queue-read-reconnect.json) | Bounded retry of retained Codex inbox reads with empty acknowledgements; uncertain writes retain existing pause behavior. |
| [2026-09-12-thirty-minute-codex-queue.json](verification/2026-09-12-thirty-minute-codex-queue.json) | passed |
| [2026-09-12-ux-home.json](verification/2026-09-12-ux-home.json) | functional acceptance passed; UI CPU increase under investigation; final CI/source review pending |
| [2026-09-14-overnight-sustained-use.json](verification/2026-09-14-overnight-sustained-use.json) | passed_for_listed_scope |
| [2026-09-15-native-codex-queue.json](verification/2026-09-15-native-codex-queue.json) | Native Codex queue, supervised recovery, canonical resume with a prompt and schema20 historical-answer migration passed at recorded sources. |
| [2026-09-15-retention-sustained-use.json](verification/2026-09-15-retention-sustained-use.json) | passed: 20-minute retention trial rerun with every claimed assertion (source 33f8117 of the retention branch on main 51a1a9f, hashed private daemon copy): ten registered agents, journal retention 120s applied by the d... |

<!-- verification-index:end -->
