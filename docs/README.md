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
- [Verification records, one line each](verification/README.md)
- [Real-engine verification](../tests/containers/README.md)

Historical phase numbers are dependency sequence numbers, not GitHub PR numbers. The product-direction page defines upcoming priorities; command-specific `--help` describes the installed binary.

Native delivery follow-ups: [desktop packaging and installation](DESKTOP-DISTRIBUTION.md), [guided setup and health checks](GUIDED-SETUP.md), [bounded real-provider acceptance](INTEGRATION-ACCEPTANCE.md), and the [implementation/acceptance tracker](NATIVE-DELIVERY.md). The [macOS runner workaround](TEST-RUNNER-MACOS.md) preserves strict leak detection while avoiding captured cross-test descriptor inheritance.

- [Claude channel input](CLAUDE-CHANNEL-INPUT.md) — explicit local opt-in, retained offers/receipts and provider acceptance limits.

## Existing-document audit, September 14, 2026

The audit covers all **37 tracked Markdown files**: 34 here, the root README and
coding instructions, and the container test README. It also checks the **61
existing verification JSON reports**, source contracts, merged PRs and current
CI/build state. No additional plan or report is needed: current open work stays
in [Remaining work](REMAINING-WORK.md), test status stays in the
[delivery crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk), and this table
records coverage. The inventory baseline is `aaa1b61`, PR #119; current outcomes
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
| [Codex input](CODEX-INPUT.md) | Partial: managed bridge, question/MCP receipts and supported review forms implemented; broader elicitation/secret input and recovery remain. |
| [Claude channel input](CLAUDE-CHANNEL-INPUT.md) | Partial: explicit channel input and question queue implemented; authorization/version and longer recovery acceptance remain. |
| [Notification routing audit](NOTIFICATION-ROUTING-AUDIT.md) | Partial: AppleScript fallback removed and native routes implemented; physical installed notification clicks and signed posting remain. |
| [Identity repair](IDENTITY-REPAIR.md) | Partial: offline preview/apply implemented and tested; production reconciliation and live transfer not done. |
| [Live daemon upgrades](LIVE-DAEMON-UPGRADES.md) | PR #130 merged independent process/I/O ownership and durable exit recovery. Actual distinct-source crash/restart acceptance passed, alongside event continuation, reconnect and output drain. Graceful replacement, successor fencing/readiness and failed-transfer recovery remain unbuilt; explicit shutdown still stops managed agents. |
| [Native delivery](NATIVE-DELIVERY.md) | Partial: merged implementation record; installation complete on this Mac, broader release acceptance incomplete. |
| [Local build](LOCAL-BUILD.md) | Reference: build, Applications installation, compatibility paths, rollback and the two-way `install.sh`/managed-install protection implemented; running-daemon switch remains operational work. |
| [Desktop distribution](DESKTOP-DISTRIBUTION.md) | Partial: packaging, installer, update consumer and retention implemented; signing and final distribution acceptance remain. |
| [Distribution setup](DISTRIBUTION-SETUP.md) | Partial: tap/formula and automation exist; Developer ID, notarization and published app cask remain. |
| [Release automation](RELEASE-AUTOMATION.md) | Partial: archives/feed workflow implemented; protected-tag execution, hosted update and clean-Mac acceptance remain. |
| [Integration acceptance](INTEGRATION-ACCEPTANCE.md) | Historical bounded trials: later source/runtime evidence is in the input guides and verification reports; no universal-provider claim. |
| [Local trial](LOCAL-TRIAL.md) | Partial: isolated/native/provider/local installation trials exist; overnight, sleep/reboot and independent-machine stages incomplete. |
| [Testing and benchmarks](TESTING-AND-BENCHMARKS.md) | Partial: standard/CI/fuzz/benchmark tools exist; full workload, failure diagnosis and platform matrices incomplete. |
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
independent tasks to rerun or close; the [verification index](verification/README.md)
lists every record with a line from its own status and is regenerated by
`python3 scripts/docs_check.py --write-index`. A later reviewed merge resolves a report's
old integration status without changing its source, failed result or acceptance
scope. New evidence can extend an existing report or the relevant PR; do not
create another overlapping plan.
