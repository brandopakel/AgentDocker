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
- [Landscape research: Dax, herdr and Paprika](LANDSCAPE-2026-09-11.md)
- [Active delivery plan, commit/PR review and complete testing crosswalk](DELIVERY-PLAN.md)
- [September 7 review scope, evidence and gap ledger](REVIEW-2026-09-07.md)
- [Engineering audit, feature coverage and known blockers](AUDIT-2026-09-06.md)
- [Native delivery progress and regression coverage](NATIVE-DELIVERY.md)
- [Local native trial and acceptance plan](LOCAL-TRIAL.md)
- [Architecture and wire protocol](ARCHITECTURE.md)
- [The remote connector: agents that work inside a browser](REMOTE-CONNECTOR.md)
- [Implementation and recovery contracts](IMPLEMENTATION-NOTES.md)
- [Optional Docker and Podman execution](CONTAINER-ENGINES.md)
- [Testing and benchmarks](TESTING-AND-BENCHMARKS.md)
- [Real-engine verification](../tests/containers/README.md)

Historical phase numbers are dependency sequence numbers, not GitHub PR numbers. The product-direction page defines upcoming priorities; command-specific `--help` describes the installed binary.

Native delivery follow-ups: [desktop packaging and installation](DESKTOP-DISTRIBUTION.md), [guided setup and health checks](GUIDED-SETUP.md), [bounded real-provider acceptance](INTEGRATION-ACCEPTANCE.md), and the [implementation/acceptance tracker](NATIVE-DELIVERY.md). The [macOS runner workaround](TEST-RUNNER-MACOS.md) preserves strict leak detection while avoiding captured cross-test descriptor inheritance.

- [Claude channel input](CLAUDE-CHANNEL-INPUT.md) — explicit local opt-in, retained offers/receipts and provider acceptance limits.

- [Portable AgentDocker coordination skill](../crates/cli/skills/agentdocker/SKILL.md) — requested September 15; one source for provider setup and MCP onboarding. Merged in #149 and included in the recorded installed source; private Codex/Claude loader discovery passed, while fresh-session implicit activation and other-provider acceptance remain.

<a id="existing-document-audit-september-14-2026"></a>

## Existing-document audit, September 14, 2026 (refreshed September 19)

The September 14 audit covered **37 tracked Markdown files**: 34 here, the root README and
coding instructions, and the container test README. It also checks the **61
existing verification JSON reports**, source contracts, merged PRs and current
CI/build state. No additional plan or report is needed: current open work stays
in [Remaining work](REMAINING-WORK.md), test status stays in the
[delivery crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk), and this table
records coverage. The September 15 portable skill adds one requested instruction
asset, bringing the tracked inventory to 38; it is not another project plan. The inventory baseline is `aaa1b61`, PR #119; current outcomes
below include merged messaging PR #150 and portable-skill PR #149, merged
UI PRs #152/#160/#170, experimental reload #155, active Codex delivery #169/#171
and session reconnect #164. The September 17 refresh adds merged #172 (landscape
reconciliation), #173 (project pause, schema 23), #174, #167 (usage-log reader),
#175 (notification reveal) and #177 (channel-capable Claude setup); installed
`d14610b7` carries them, plus the Board (#176), persisted message drafts (#178),
preserved resume state (#179) and startup ordering (#180), all merged after
final review/CI. The September 17 20:55 UTC activation preserved both provider
sessions and all 128 retained receipts. Actual installed conversation-draft
restoration passed without submitting the text. Remaining acceptance is not an
open implementation claim.

Later on September 18 the September landscape items landed — typed links #182,
webhooks #183, the Board layout #189, and through the combined #191 roles #186,
reply from the notification #190, delivery readiness #188 and question drafts
#185 — and [Remaining work](REMAINING-WORK.md) gained the road to v1, the short
list between main and a release other people can install.

The September 18 reconciliation checked GitHub merge metadata and ancestry for
older records that still said final review/CI or installation was pending. Their
original trial fields remain intact; a dated disposition now supplies the index
summary. PRs #181/#182 are also merged and included in the current desktop preview. The
current inventory is 40 Markdown files (including the connector document and
trial issue template) and 67 verification records. The September 19 audit
rechecked every document's disposition, merged source through `14f1c519`,
installed component identities, actual release assets and release policy.
Current UI preview source is `ce82d06`; the serving coordinator stays at
`705f924` to preserve its managed sessions. See the [component table](REMAINING-WORK.md#delivered-source-and-current-desktop).

**Reference** means the document describes an implemented workflow or engineering
rule; it is not a release certificate. **Partial** means named implementation or
acceptance remains. **Historical** means the audit/probe itself is complete and
its original evidence stays intact; unresolved findings are carried into the
current tracker. **Deferred** identifies existing optional proposals.

| Existing document | Audit outcome / remaining scope |
| --- | --- |
| [Root README](../README.md) | Reference: implemented single-host features and usage. The duplicated roadmap is removed in PR #131; unfinished engineering remains in the existing Remaining work document, including Windows and deferred federation. |
| [Trial issue template](../.github/ISSUE_TEMPLATE/trial-report.md) | Complete reporting route: actual versions, reproduction steps and expected/observed behavior; private paths and content should be redacted. |
| [Coding instructions](../CLAUDE.md) | Reference: source layout, one build campaign, strict verification and source review. The pure-core rule holds: environment reads live in the host crate. |
| [This index](README.md) | Reference: complete file inventory and one current backlog/crosswalk. |
| [Remaining work](REMAINING-WORK.md) | Current backlog: completed implementation is separated from open engineering, release setup and acceptance. Shared-chat integration (#200), build-campaign ownership (#204), a current downloadable desktop and independent-machine acceptance remain. |
| [Delivery plan](DELIVERY-PLAN.md) | Historical checkpoints retained; T01–T12/L01–L15 dispositions refreshed. Current component identities and bounded Claude idle/receipt acceptance are reconciled; old pending notes do not reopen completed implementation. |
| [Remote connector](REMOTE-CONNECTOR.md) | Implemented: inventory/helpers, local OAuth/MCP, both real vendors, login service, stable Tailscale host, project routing and allowlisting. Real-account CIMD, desktop service start/install and periodic egress-feed refresh remain. Browser models poll; no idle-wake claim. |
| [Product direction](PRODUCT-DIRECTION.md) | Partial delivery: single-host implementation includes Messages, Board/drafts, project pause, reconnect and initial usage collection. Signed downloads, wider provider acceptance, native Windows and independent Linux/Mac trials remain; federation is deferred. |
| [Architecture](ARCHITECTURE.md) | Implemented protocol/phase inventory reconciled with schema 23 and merged #191/#194/#199. Usage activation/resources, experimental reload acceptance, Windows and deferred federation remain explicitly incomplete. |
| [Implementation notes](IMPLEMENTATION-NOTES.md) | Reference: implemented coordination/recovery contracts; distinguish command relaunch from conversation restoration. |
| [Guide](GUIDE.md) | Reference: command/tool inventory and current Tools/terminal navigation reconciled with source. |
| [Desktop UX](DESKTOP-UX.md) | Implemented controls and current capabilities, with shared-chat presentation (#200) installed as a local preview and awaiting integration; session presentation (#203) is merged. Physical keyboard, IME, screen-reader and coworker usability acceptance remain. |
| [Iced design](ICED-DESIGN.md) | Partial: native migration and automated interactions implemented; VoiceOver/IME and other-platform hands-on acceptance remain. |
| [Guided setup](GUIDED-SETUP.md) | Implemented preview/apply/undo and ordinary CLI setup receipts (local validation passed; final review/CI pending), health, channel-capable Claude setup, shell opt-in and same-session reconnect. The person's managed-Claude consent/idle trial passed; empty-profile configuration preview/apply/undo passed on `ce82d06`; actual fresh-account and additional-provider acceptance remain. |
| [Activity and messaging](ACTIVITY-AND-MESSAGING.md) | Implemented coordination/activity contracts and supported input adapters. Hooks/MCP contact alone is not idle wake; plain sessions and other-provider parity remain explicit limits. |
| [Message delivery audit](MESSAGE-DELIVERY-AUDIT.md) | Partial acceptance: shared durable queue/exact receipts, limit gating and recovery are implemented. #196/#199 Claude managed reconnect/idle and #202 Codex active peer-answer delivery have actual bounded evidence. Other runtimes, startup/reopen, latency and sustained/provider-reset cases remain. |
| [Codex input](CODEX-INPUT.md) | Implemented managed bridge and existing-terminal native queue. Installed active peer-answer stall is fixed in merged #202; idle and active trials retain exact source/receipt evidence. Zero-prompt startup/reopen, wider permissions/elicitation, throughput and historical reconciliation remain. |
| [Claude channel input](CLAUDE-CHANNEL-INPUT.md) | Implemented channel queue, same-session resumption and app-guided reconnect. The person accepted provider consent and an idle project input received a reply without keyboard input; broader provider/version/recovery cases remain. |
| [Notification routing audit](NOTIFICATION-ROUTING-AUDIT.md) | Bounded installed acceptance passed: approved OS permission, automatic daemon posting, real foreground/background message/question/stale-target clicks, preserved draft and cold private-origin routing. The person's September 17 finding — a click while the conversation was already open did nothing — is fixed in #175 (installed): the archived row is marked and scrolled to, paging back a bounded number of pages, and actual Notification Center presses on the installed revision — with the app hidden, and with no GUI process (launched and revealed in 1.7 s) — revealed the exact message (September 17). A physical mouse click, the page-back path on a real click and signed-release acceptance remain. |
| [Identity repair](IDENTITY-REPAIR.md) | Partial: offline preview/apply implemented and tested; production reconciliation not done (it needs a quiet window with every recorded session ended). Same-session reconnect is separate (#164); #179 (merged) preserves eligible observations, open channel references and task ownership across the fold. |
| [Live daemon upgrades](LIVE-DAEMON-UPGRADES.md) | Boundaries 2 to 6 are in source: session owners (merged), the coordinator fence, the successor handover with acceptance as the readiness commit, CLI clients that follow a replaced daemon, and installations that reload the running daemon to the activated release; the desktop app retries `transferring`. Records: [successor readiness](verification/2026-09-15-successor-readiness.json) and [20 reloads under pressure](verification/2026-09-15-reload-acceptance.json). Bounded Codex native-client acceptance now covers idle/draft/busy/question handovers with a loopback model fixture and exact answer receipt. Still gated behind `AGENTDOCKER_EXPERIMENTAL_RELOAD`: actual model services, Claude, distinct-source provider handovers, attached-terminal drafts, replay retention limits and Windows remain; explicit shutdown still stops managed agents. |
| [Native delivery](NATIVE-DELIVERY.md) | Implemented native foundation, installer, retention and gated reload, with historical fixes complete. Installed app and serving coordinator intentionally differ; public release and broad acceptance remain. |
| [Local build](LOCAL-BUILD.md) | Reference: build/install, stable launcher compatibility, update/rollback and retention. Current component table identifies the app, daemon and receiver separately; gate-off reload preserves running agents. |
| [Desktop distribution](DESKTOP-DISTRIBUTION.md) | Partial: packaging, installer, update consumer and retention implemented; signed-copy launcher and actual older-release rollback passed private acceptance. Production activation passed; public signing and final distribution acceptance remain. |
| [Distribution setup](DISTRIBUTION-SETUP.md) | Partial: tap/formula and automation exist; Developer ID, notarization and published app cask remain. |
| [Release automation](RELEASE-AUTOMATION.md) | Partial: archives/feed workflow implemented; protected-tag execution, hosted update and clean-Mac acceptance remain. |
| [Integration acceptance](INTEGRATION-ACCEPTANCE.md) | Historical bounded trials: later source/runtime evidence is in the input guides and verification reports; no universal-provider claim. |
| [Local trial](LOCAL-TRIAL.md) | Partial: isolated/native/provider/local installation trials exist; the overnight stage has a [7.5-hour record](verification/2026-09-14-overnight-sustained-use.json) and retention a [20-minute record](verification/2026-09-15-retention-sustained-use.json); sleep/reboot and independent-machine stages incomplete. The first coworker rollout requires macOS, Linux and native Windows acceptance. |
| [Testing and benchmarks](TESTING-AND-BENCHMARKS.md) | Partial: standard/CI/fuzz/benchmark tools and the sustained-use and retention workload scripts exist with their records; failure diagnosis and platform matrices incomplete. |
| [macOS test runner](TEST-RUNNER-MACOS.md) | Reference: reproduced descriptor inheritance and validated strict serial workaround; remove only after an upstream fix passes its controls. |
| [Windows port](WINDOWS-PORT.md) | Partial: native core/host/named-pipe foundations; full daemon/GUI, ConPTY, service and installer remain; required for the first coworker rollout. |
| [Container engines](CONTAINER-ENGINES.md) | Partial: optional engines/workspaces implemented; Mac engine acceptance and documented unsupported capabilities remain. |
| [Container test README](../tests/containers/README.md) | Reference with partial acceptance: separate engine/lifecycle/relay fixtures; Linux CI and earlier Mac Podman evidence do not establish current Docker Desktop acceptance. |
| [September 4 audit](AUDIT-2026-09-04.md) | Historical: retain original baseline; current fixes and open work supersede its status. |
| [September 6 audit](AUDIT-2026-09-06.md) | Historical: restore/privacy fixes and pure-core environment cleanup are complete; broader acceptance stays in the current tracker. |
| [September 7 review](REVIEW-2026-09-07.md) | Historical: merged review stacks closed; retained timeout/acceptance findings remain in the current tracker. |
| [September 8 review](REVIEW-2026-09-08.md) | Historical: implementation follow-ups merged; original failures remain source-specific evidence. |
| [September 11 landscape](LANDSCAPE-2026-09-11.md) | Historical research complete. Requested local Board, typed links, webhooks, roles, notification Reply, readiness and CLI exit statuses are merged; optional herdr/Paprika bridges remain deferred. Acceptance limits live in the current tracker. |
| [herdr bridge](HERDR-BRIDGE.md) | Deferred by measurement beyond implemented pane identity (row 25): the focus/prompt bridge and reported-blocked mirror are designed, costed and not built; nothing in current delivery depends on them. |

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
| [2026-09-07-claude-profile-setup.json](verification/2026-09-07-claude-profile-setup.json) | Portable coordination skill merged in #149 and is included in the recorded installed d14610b7 candidate built from source 3785e810; fresh-session acceptance remains open. |
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
| [2026-09-11-followup-integration.json](verification/2026-09-11-followup-integration.json) | PRs #99–#102 merged; their original local acceptance records and failures remain source-specific. |
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
| [2026-09-12-cli-sender-identity.json](verification/2026-09-12-cli-sender-identity.json) | CLI sender identity is integrated through #119; the original sender trials remain source-specific. |
| [2026-09-12-compact-question-history.json](verification/2026-09-12-compact-question-history.json) | Compact retained Inbox questions, explicit complete-text details, and notification navigation without draft submission or message dismissal. |
| [2026-09-12-event-continuation.json](verification/2026-09-12-event-continuation.json) | Checked event continuation and its provider-worker integration are merged; live replacement remains experimentally gated. |
| [2026-09-12-file-change-review.json](verification/2026-09-12-file-change-review.json) | Bounded Codex file-change approval through the shared human answer queue and compact native full-diff review. |
| [2026-09-12-input-delivery-status.json](verification/2026-09-12-input-delivery-status.json) | PR #108 merged as 1e2914270b59fcb6721d463999eed80229e15e5d after final 5778212 CI and actual source inspection. |
| [2026-09-12-integrated-desktop.json](verification/2026-09-12-integrated-desktop.json) | Combined PR119: messenger Inbox, minimal home and Tools, permission validation, sender identity, launcher compatibility and persisted Applications destination |
| [2026-09-12-launcher-hook-repair.json](verification/2026-09-12-launcher-hook-repair.json) | Historical hook repair and September 15 intact launcher: private install/rollback/routes, production activation and native Codex peer wake passed. |
| [2026-09-12-output-drain.json](verification/2026-09-12-output-drain.json) | Managed output ownership through pipe/terminal EOF and final log flush before publishing exit, releasing protection or restarting; not cross-process daemon handover. |
| [2026-09-12-permission-review.json](verification/2026-09-12-permission-review.json) | Concrete permission review merged through #115/#119; broader provider review surfaces remain open. |
| [2026-09-12-provider-event-reconnect.json](verification/2026-09-12-provider-event-reconnect.json) | Local implementation and actual Codex event-only reconnect acceptance passed at 687e57f. |
| [2026-09-12-queue-read-reconnect.json](verification/2026-09-12-queue-read-reconnect.json) | Bounded retry of retained Codex inbox reads with empty acknowledgements; uncertain writes retain existing pause behavior. |
| [2026-09-12-thirty-minute-codex-queue.json](verification/2026-09-12-thirty-minute-codex-queue.json) | passed |
| [2026-09-12-ux-home.json](verification/2026-09-12-ux-home.json) | Home simplification is integrated through #119; the recorded CPU investigation remains historical evidence. |
| [2026-09-14-overnight-sustained-use.json](verification/2026-09-14-overnight-sustained-use.json) | passed_for_listed_scope |
| [2026-09-15-native-codex-queue.json](verification/2026-09-15-native-codex-queue.json) | Existing Codex input is merged and has installed idle/active receipt evidence; universal-provider and startup acceptance remain open. |
| [2026-09-15-reload-acceptance.json](verification/2026-09-15-reload-acceptance.json) | Passed at bffc599 on release binaries built from the committed source (state schema 18): 20 successive gated reloads of one private daemon, each predecessor retired within 14.0 s of the reload being asked for, while t... |
| [2026-09-15-retention-sustained-use.json](verification/2026-09-15-retention-sustained-use.json) | passed: 20-minute retention trial rerun with every claimed assertion (source 33f8117 of the retention branch on main 51a1a9f, hashed private daemon copy): ten registered agents, journal retention 120s applied by the d... |
| [2026-09-15-successor-readiness.json](verification/2026-09-15-successor-readiness.json) | Passed at fb92879 on release binaries built from the committed source (state schema 18): two successive gated reloads kept a batch and a PTY agent's processes, logs and exact exits 7/3 under the third daemon with the... |
| [2026-09-16-reload-controller-episode.json](verification/2026-09-16-reload-controller-episode.json) | passed |

<!-- verification-index:end -->
