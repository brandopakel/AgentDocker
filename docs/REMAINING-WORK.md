# Remaining engineering and release work

Reconciled September 9, 2026 against this checkout's implementation and project
documents. This is the current backlog summary; dated audits and verification
reports retain their original source-specific results. It is not a fresh audit
of remote PRs or other machines. A targeted read-only GitHub check verified the
Homebrew tap, publishing configuration names, and latest release metadata.

The three previously listed manual steps do **not** mean all engineering is
complete. The Iced migration exists, but daily use, safe upgrades, delivery and
platform support have separate completion conditions.

## Desktop cleanup in this change

- Default to **Current** sessions; retain completed runs under **History**.
  A read-only local diagnosis found ten exited records and two live coding
  agents, plus the human identity. Showing retained runs beside current work
  contributed directly to the apparent duplication. No registry data was deleted.
- Add project-scoped **Needs input**, prioritize unanswered questions, and keep
  search available across the filters. Finished askers' outstanding questions
  remain actionable until expiry.
- Replace large session cards with compact rows. Keep terminal/reply/stop actions
  in the selected session and move process, checkout and ID information to Details.
  Narrow windows open the selected session directly with Back to sessions.
- Put Coordination, Commands and project management under **More**. Show installed
  tools first in Connections, with paths, versions and capabilities in Details.
- Suppress known Codex Node launchers when their native child is present. This
  addresses a discovery path found in code, not a demonstrated second live
  registration on this Mac. Suppress transient discovery/registration overlap
  only with matching known PID and birth time; retain PID reuse and unknown cases.
- Extend unit and native workflow coverage for history separation, attention,
  process identity evidence and narrow-window navigation.

Source changes take effect in rebuilt binaries. They do not replace the installed
launcher or the daemon hosting existing sessions.

## Engineering still open

| Priority | Work | Completion condition | Supporting documents |
| --- | --- | --- | --- |
| Next | Legacy duplicate registry reconciliation | Provide a reviewed migration for proven duplicate identities, preserving inboxes, leases, channel membership, history and routing. Reject ambiguous provider sessions, process births and physical checkouts. Hiding history is not this migration. | [Activity and messaging](ACTIVITY-AND-MESSAGING.md), [delivery checkpoints](DELIVERY-PLAN.md), [identity acceptance](INTEGRATION-ACCEPTANCE.md) |
| Implemented; broader acceptance open | Codex incoming-message delivery | Prompt/tool/Stop context delivery and acknowledgement after output are implemented. A fresh Codex 0.153.4 trial passed all three boundaries with correlated replies and one identity. Additional provider versions, restart cycles and sustained use remain; hooks cannot wake an already idle provider. | [Activity and messaging](ACTIVITY-AND-MESSAGING.md), [integration acceptance](INTEGRATION-ACCEPTANCE.md) |
| Next | Safe live daemon replacement | Preserve child ownership, batch/PTY I/O, logs, identity, leases and schema compatibility; require the actual successor to be ready before retiring its predecessor, with failure recovery. `daemon reload` deliberately returns unavailable today. | [Architecture](ARCHITECTURE.md#sessions-and-persistence), [delivery plan](DELIVERY-PLAN.md) |
| Next | Sustained-use bounds and unresolved performance failures | Complete 1/10/100-agent resource trials, long-running growth/retention checks, large-checkout latency, crash/reboot and distinct-source upgrade/rollback trials. Diagnose the retained socket benchmark timeout; short passing repeats do not explain it. Fix defects these trials uncover. | [Testing standard](TESTING-AND-BENCHMARKS.md), [local trial](LOCAL-TRIAL.md), [delivery checkpoints](DELIVERY-PLAN.md) |
| Release | Download/update distribution | Deliver a verified download/update feed and scheduling policy; verify automatic formula updates in the existing Homebrew tap, publish the app cask, and deliver supported Linux packages. Registry Cargo publication is not an established supported route. Packaging/install/rollback already exist. | [Desktop distribution](DESKTOP-DISTRIBUTION.md), [distribution setup](DISTRIBUTION-SETUP.md), [product direction](PRODUCT-DIRECTION.md) |
| Platform | Linux delivery acceptance | Complete target-distribution desktop/service/package trials and ARM64 graphical execution. Existing x86-64 graphical CI does not establish that coverage. | [Product direction](PRODUCT-DIRECTION.md), [local trial](LOCAL-TRIAL.md) |
| Platform | Full native Windows product | Integrate the daemon and clients with named pipes; finish supervised lifecycle, ConPTY, identity-safe stopping/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and native graphical CI. Core/host/desktop adapter coverage is only a foundation. | [Windows port](WINDOWS-PORT.md), [architecture](ARCHITECTURE.md) |
| Follow-up | Terminal selection and richer interaction | Implement arbitrary terminal cell-range selection/copy; test physical keyboard focus, text editing and input methods with users, and repair observed usability/accessibility defects. Current copy takes the visible screen. | [Iced contracts](ICED-DESIGN.md), [desktop guide](DESKTOP-UX.md) |
| Follow-up | Concurrent provider configuration mutation | Coordinate changes to the same provider entry during delegated apply/undo. Existing exact-entry checks and receipts preserve ownership, but the provider CLI is not a compare-and-swap transaction. | [Guided setup](GUIDED-SETUP.md), [delivery checkpoints](DELIVERY-PLAN.md) |
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

## Distribution contradiction resolved

[Product direction](PRODUCT-DIRECTION.md) and the root README previously said no
Homebrew tap existed, while [Distribution setup](DISTRIBUTION-SETUP.md) marked it
done. A September 9 read-only GitHub check found the
[tap formula](https://github.com/brandopakel/homebrew-tap/blob/main/Formula/agentdocker.rb)
at v0.1.0, the tap repository variable and token secret name configured, and only
a README in `Casks/`. The latest published release metadata still named v0.1.0.
The source workflow already generates and attempts to publish the formula/cask.
Creating the tap is complete; publication of a newer verified release and its app
cask remains. Secret contents were not read and token validity was not tested.

## Reading the older documents

The root README, docs index, product direction, architecture, implementation
notes, delivery/native trackers, desktop/Iced guides, distribution/setup guides,
activity/integration acceptance, testing/local trial, Windows and container docs
were cross-checked for remaining work. The September 4/6 audits and September 7/8
review ledgers describe their own baselines. The runner notes and container test
README define validation procedures rather than additional product features.

Restore/private-state fixes, pre-exec launch gating, atomic native exit, bounded
queues, setup preview/apply/undo, profile routing, installation retention, joined
MCP/hooks identity and the Iced migration have implementations. Do not turn old
audit findings into new “missing features” without checking later corrections.
Conversely, an architecture row marked “done” or a historical green CI run does
not complete the release, platform, soak or human-acceptance work above.

The [local cleanup verification](verification/2026-09-09-desktop-simplification.json)
records 671 passing Rust tests (six skipped), 43 Python checks, strict lint,
packaging/release build, the final 83-test UI recheck, and 96 + 6 native workflow
steps against the packaged binaries. The preview is 24.3 MiB installed and
10.4 MiB zipped. Its source-input hash precedes the final guide/report-only edits.
Public release and hands-on acceptance remain separate gates.
