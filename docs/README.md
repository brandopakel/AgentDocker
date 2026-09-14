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

## Existing-document audit, September 14, 2026

The audit covers all **37 tracked Markdown files**: 34 here, the root README and
coding instructions, and the container test README. It also checks the **61
existing verification JSON reports**, source contracts, merged PRs and current
CI/build state. No additional plan or report is needed: current open work stays
in [Remaining work](REMAINING-WORK.md), test status stays in the
[delivery crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk), and this table
records coverage. The source baseline is `aaa1b61`, PR #119.

**Reference** means the document describes an implemented workflow or engineering
rule; it is not a release certificate. **Partial** means named implementation or
acceptance remains. **Historical** means the audit/probe itself is complete and
its original evidence stays intact; unresolved findings are carried into the
current tracker. **Deferred** identifies existing optional proposals.

| Existing document | Audit outcome / remaining scope |
| --- | --- |
| [Root README and roadmap](../README.md) | Partial: phases 0–4 implemented; phase 5 acceptance/engineering, Windows and federation unfinished. Policy/quotas and restart/dependencies are already built. |
| [Coding instructions](../CLAUDE.md) | Reference: source layout, one build campaign, strict verification and source review. Pure-core exception remains tracked. |
| [This index](README.md) | Reference: complete file inventory and one current backlog/crosswalk. |
| [Remaining work](REMAINING-WORK.md) | Partial: current disposition of existing engineering, acceptance and release requirements. |
| [Delivery plan](DELIVERY-PLAN.md) | Partial: current sequence and T01–T12/L01–L15 status; old checkpoints are historical. |
| [Product direction](PRODUCT-DIRECTION.md) | Partial: single-host implementation exists; release/platform delivery remains, federation deferred. |
| [Architecture](ARCHITECTURE.md) | Partial: protocol/phase inventory and journal/checkpoint maintenance exist; pure-core cleanup, live replacement and Windows are incomplete. Optional protocol proposals remain deferred. |
| [Implementation notes](IMPLEMENTATION-NOTES.md) | Reference: implemented coordination/recovery contracts; distinguish command relaunch from conversation restoration. |
| [Guide](GUIDE.md) | Reference: command/tool inventory and current Tools/terminal navigation reconciled with source. |
| [Desktop UX](DESKTOP-UX.md) | Partial: simplified home/Inbox/Tools implemented; capability verification and human input acceptance remain. |
| [Iced design](ICED-DESIGN.md) | Partial: native migration and automated interactions implemented; VoiceOver/IME and other-platform hands-on acceptance remain. |
| [Guided setup](GUIDED-SETUP.md) | Partial: preview/apply/undo and configuration locks implemented; verified input readiness and required-trust guidance remain. |
| [Activity and messaging](ACTIVITY-AND-MESSAGING.md) | Partial: activity/hooks and opt-in input adapters exist; ordinary hooks alone cannot wake idle models. |
| [Message delivery audit](MESSAGE-DELIVERY-AUDIT.md) | Partial: durable shared queue, exact receipts and bounded actual-provider trials pass; broader reviews, interruption and sustained acceptance remain. |
| [Codex input](CODEX-INPUT.md) | Partial: managed bridge, question/MCP receipts and supported review forms implemented; broader elicitation/secret input and recovery remain. |
| [Claude channel input](CLAUDE-CHANNEL-INPUT.md) | Partial: explicit channel input and question queue implemented; authorization/version and longer recovery acceptance remain. |
| [Notification routing audit](NOTIFICATION-ROUTING-AUDIT.md) | Partial: AppleScript fallback removed and native routes implemented; physical installed notification clicks and signed posting remain. |
| [Identity repair](IDENTITY-REPAIR.md) | Partial: offline preview/apply implemented and tested; production reconciliation and live transfer not done. |
| [Live daemon upgrades](LIVE-DAEMON-UPGRADES.md) | Partial: event continuation, reconnect and output drain implemented; process/I/O transfer and successor fencing/readiness unbuilt. |
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
| [September 6 audit](AUDIT-2026-09-06.md) | Historical: restore/privacy fixes merged; pure-core exception and broader acceptance carried forward. |
| [September 7 review](REVIEW-2026-09-07.md) | Historical: merged review stacks closed; retained timeout/acceptance findings remain in the current tracker. |
| [September 8 review](REVIEW-2026-09-08.md) | Historical: implementation follow-ups merged; original failures remain source-specific evidence. |
| [September 11 landscape](LANDSCAPE-2026-09-11.md) | Historical research: adopted UI ideas implemented; remaining suggestions are optional, not a new release checklist. |
| [herdr bridge](HERDR-BRIDGE.md) | Deferred beyond implemented pane identity: focus/prompt bridge and reported-blocked mirror are proposals, not delivered features. |

Verification reports preserve the original trials, rather than representing
61 independent tasks to rerun or close. A later reviewed merge resolves a report's
old integration status without changing its source, failed result or acceptance
scope. New evidence can extend an existing report or the relevant PR; do not
create another overlapping plan.
