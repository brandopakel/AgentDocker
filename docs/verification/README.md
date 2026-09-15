# Verification records

One line per record, taken from the record's own status; regenerate with
`python3 scripts/docs_check.py --write-index` after adding one. The check
in the gate fails when this file and the records disagree. A record keeps
its original source, date and outcome; a later merge does not rewrite it.

| Record | Says |
| --- | --- |
| [2026-09-07-claude-profile-setup.json](2026-09-07-claude-profile-setup.json) | recorded undated; 34195804495 |
| [2026-09-07-desktop-maintenance.json](2026-09-07-desktop-maintenance.json) | owned native desktop fixtures; private paths and captures excluded |
| [2026-09-07-identity-lifecycle.json](2026-09-07-identity-lifecycle.json) | Packaged adapter lifecycle with a synthetic host; separate from actual model-provider trials |
| [2026-09-07-integration-benchmark-failure.json](2026-09-07-integration-benchmark-failure.json) | Original failed integrated benchmark; not a completed performance acceptance campaign |
| [2026-09-07-local.json](2026-09-07-local.json) | Selected exact-source local campaigns; this is not final release acceptance. |
| [2026-09-07-macos-capture-failure.json](2026-09-07-macos-capture-failure.json) | failed |
| [2026-09-07-native-resources.json](2026-09-07-native-resources.json) | sanitized exact-source native desktop integration and short resource observations; raw logs, process identities and captures remain private |
| [2026-09-07-provider-activity.json](2026-09-07-provider-activity.json) | Source-pinned native integration, actual provider callbacks and synthetic message trials; not complete delivery or release acceptance |
| [2026-09-07-state-timing-diagnostic.json](2026-09-07-state-timing-diagnostic.json) | Separate opt-in state timing diagnostic campaign, not a diagnosis of the original failed campaign |
| [2026-09-07-terminal-resources.json](2026-09-07-terminal-resources.json) | Terminal admission/lifecycle verification and short native Mac observation at the stated sources; not release acceptance |
| [2026-09-08-final-candidate-compile.json](2026-09-08-final-candidate-compile.json) | failed |
| [2026-09-08-hook-input-baseline.json](2026-09-08-hook-input-baseline.json) | reproduced Claude stdin wait with no timeout within the 1.5 s observation window |
| [2026-09-08-integrated-provider-trials.json](2026-09-08-integrated-provider-trials.json) | setup_trial: passed |
| [2026-09-09-desktop-simplification.json](2026-09-09-desktop-simplification.json) | passed |
| [2026-09-10-bulk-receipts.json](2026-09-10-bulk-receipts.json) | Source-bound bulk receipts and removed-checkout regression checkpoint; not provider idle-wake or public release acceptance. |
| [2026-09-10-button-interaction.json](2026-09-10-button-interaction.json) | Primary button text contrast during pointer interaction; follow-up to desktop delivery verification |
| [2026-09-10-claude-channel-probe.json](2026-09-10-claude-channel-probe.json) | Owned Claude provider capability probes; no production AgentDocker outbox implementation yet |
| [2026-09-10-codex-delivery.json](2026-09-10-codex-delivery.json) | passed |
| [2026-09-10-desktop-delivery.json](2026-09-10-desktop-delivery.json) | Iced usability, provider lifecycle delivery, durable coordination, package acceptance and bounded resource trials |
| [2026-09-10-desktop-design.json](2026-09-10-desktop-design.json) | Iced desktop design rounds of 2026-09-10: visual, interaction and idle-cost verification for commits 4f5b921, 17ca37a, 00c1c98, 4f0f579 on codex/desktop-delivery. |
| [2026-09-10-desktop-load.json](2026-09-10-desktop-load.json) | UI load benchmark and memory bisect for the Iced desktop on 2026-09-10, packaged baseline binaries and a release UI rebuild for the fix; see per-campaign contention limits. |
| [2026-09-10-durable-queue.json](2026-09-10-durable-queue.json) | standard_gate: passed |
| [2026-09-10-message-receipts.json](2026-09-10-message-receipts.json) | standard_gate: passed |
| [2026-09-10-notification-routing.json](2026-09-10-notification-routing.json) | release_workflow: passed |
| [2026-09-11-claude-channel-input.json](2026-09-11-claude-channel-input.json) | implemented_and_partial_acceptance |
| [2026-09-11-codex-appserver-input.json](2026-09-11-codex-appserver-input.json) | passed |
| [2026-09-11-codex-hook-discovery.json](2026-09-11-codex-hook-discovery.json) | passed |
| [2026-09-11-codex-input-bridge.json](2026-09-11-codex-input-bridge.json) | Experimental managed native Codex input, queue ownership and bounded crash recovery |
| [2026-09-11-codex-input-review.json](2026-09-11-codex-input-review.json) | Native identity-repair acceptance compares migrated schema to the same daemon fresh database, instead of stale schema 11. |
| [2026-09-11-codex-queue-recovery.json](2026-09-11-codex-queue-recovery.json) | passed |
| [2026-09-11-daily-updates.json](2026-09-11-daily-updates.json) | passed_for_listed_scope |
| [2026-09-11-desktop-identity.json](2026-09-11-desktop-identity.json) | Iced desktop identity round of 2026-09-11 on codex/desktop-delivery: per-project monogram tiles, the window-local unviewed-done badge, the Connections hooks copy and the per-launch Claude channel checkbox. |
| [2026-09-11-desktop-release.json](2026-09-11-desktop-release.json) | implemented_and_local_acceptance_passed |
| [2026-09-11-desktop-update.json](2026-09-11-desktop-update.json) | Update consumer ('agentdocker desktop update') verification on 2026-09-11: unit tests, lint and the offline update smoke against a locally packaged release under a disposable prefix. |
| [2026-09-11-followup-integration.json](2026-09-11-followup-integration.json) | local_gates_passed_final_ci_pending |
| [2026-09-11-hour-sustained-use.json](2026-09-11-hour-sustained-use.json) | passed_for_listed_scope |
| [2026-09-11-identity-repair.json](2026-09-11-identity-repair.json) | Explicit offline reconciliation of proven local external Claude/Codex duplicate records; durable exact aliases and preserved history. |
| [2026-09-11-macos-watcher-recovery.json](2026-09-11-macos-watcher-recovery.json) | passed |
| [2026-09-11-managed-claude-input.json](2026-09-11-managed-claude-input.json) | passed_bounded_managed_launch_acceptance |
| [2026-09-11-mcp-answer-receipts.json](2026-09-11-mcp-answer-receipts.json) | Codex MCP ask_human answer receipts and restart recovery |
| [2026-09-11-provider-configuration.json](2026-09-11-provider-configuration.json) | passed_for_listed_scope |
| [2026-09-11-provider-question-receipts.json](2026-09-11-provider-question-receipts.json) | separate_mcp_question_gap: failed |
| [2026-09-11-recovery-fixtures.json](2026-09-11-recovery-fixtures.json) | passed_for_listed_scope |
| [2026-09-11-retyped-drafts.json](2026-09-11-retyped-drafts.json) | passed_for_listed_scope |
| [2026-09-11-session-messages.json](2026-09-11-session-messages.json) | passed_for_listed_scope |
| [2026-09-11-structured-questions.json](2026-09-11-structured-questions.json) | Structured Iced command and choice questions; rendered callbacks and owned actual Codex trials |
| [2026-09-11-terminal-selection.json](2026-09-11-terminal-selection.json) | passed_for_listed_scope |
| [2026-09-12-claude-question-queue.json](2026-09-12-claude-question-queue.json) | One Claude channel question-answer path with durable shared-queue receipts and reply metadata |
| [2026-09-12-cli-sender-identity.json](2026-09-12-cli-sender-identity.json) | local acceptance and actual Claude shell sender trial passed; final CI/source review pending |
| [2026-09-12-compact-question-history.json](2026-09-12-compact-question-history.json) | Compact retained Inbox questions, explicit complete-text details, and notification navigation without draft submission or message dismissal. |
| [2026-09-12-event-continuation.json](2026-09-12-event-continuation.json) | Local review correction acceptance passed at 9dda825: 837 Rust tests, 65 Python checks and fresh real CLI/daemon restart/schema/exhaustion checks. |
| [2026-09-12-file-change-review.json](2026-09-12-file-change-review.json) | Bounded Codex file-change approval through the shared human answer queue and compact native full-diff review. |
| [2026-09-12-input-delivery-status.json](2026-09-12-input-delivery-status.json) | PR #108 merged as 1e2914270b59fcb6721d463999eed80229e15e5d after final 5778212 CI and actual source inspection. |
| [2026-09-12-integrated-desktop.json](2026-09-12-integrated-desktop.json) | Combined PR119: messenger Inbox, minimal home and Tools, permission validation, sender identity, launcher compatibility and persisted Applications destination |
| [2026-09-12-launcher-hook-repair.json](2026-09-12-launcher-hook-repair.json) | macOS launcher hook/MCP regression, real bundle discovery/opening, legacy paths across activation and rollback |
| [2026-09-12-output-drain.json](2026-09-12-output-drain.json) | Managed output ownership through pipe/terminal EOF and final log flush before publishing exit, releasing protection or restarting; not cross-process daemon handover. |
| [2026-09-12-permission-review.json](2026-09-12-permission-review.json) | local acceptance passed; final CI/source review pending |
| [2026-09-12-provider-event-reconnect.json](2026-09-12-provider-event-reconnect.json) | Local implementation and actual Codex event-only reconnect acceptance passed at 687e57f. |
| [2026-09-12-queue-read-reconnect.json](2026-09-12-queue-read-reconnect.json) | Bounded retry of retained Codex inbox reads with empty acknowledgements; uncertain writes retain existing pause behavior. |
| [2026-09-12-thirty-minute-codex-queue.json](2026-09-12-thirty-minute-codex-queue.json) | passed |
| [2026-09-12-ux-home.json](2026-09-12-ux-home.json) | functional acceptance passed; UI CPU increase under investigation; final CI/source review pending |
| [2026-09-15-native-codex-queue.json](2026-09-15-native-codex-queue.json) | Provider capability trials using actual installed Codex TUI and queue binaries with a private profile and loopback Responses fixture; not AgentDocker forwarding acceptance. |
