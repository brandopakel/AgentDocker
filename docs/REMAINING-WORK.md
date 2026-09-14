# Remaining engineering and release work

Audited September 14, 2026 against merged `aaa1b61` (PR #119). This is the current
backlog for the requirements already in the project documents. The
[documentation index](README.md) records coverage of all 37 Markdown files;
the [delivery crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk) records
which test categories are complete or partial. Dated audits and
[verification reports](verification/) retain the implementation history,
source-specific results and failed trials.

## Delivered source and current desktop

PRs #98–#114 provide durable queues, notification routing, shared human/peer
submission, structured questions, update checking, identity-safe discovery and
offline duplicate repair. PR #119 consolidates the remaining #115–#118 changes
and Claude's messenger, minimal-home and Applications-folder work. It is merged,
reviewed and installed. [Final acceptance and installation](https://github.com/brandopakel/AgentDocker/pull/119#issuecomment-5649912103)
record 871 Rust tests, 65 Python checks, 218 native steps and 12 installation
scenarios at reviewed `d70668c`; the installed main build has matching source
inputs and pre-package executable hashes. All main CI workflows passed at
`aaa1b61`, including native desktop, Windows foundations and engines. Scheduled
fuzzing also passed September 13 and 14.

The earlier [combined checkpoint](verification/2026-09-12-integrated-desktop.json)
records 869 Rust tests and 265 native steps at `462c1b3`, actual Claude delivery
and a quiet 100-agent UI comparison. Those counts and sustained results remain
specific to that source. Older per-feature “CI/review pending” notes no longer
represent pending integration; their failed trials and acceptance limits remain.

| Delivered behavior | Evidence and limits |
| --- | --- |
| Simpler home, Inbox and Tools | Home groups agents by project with a short attention strip. Inbox has agent conversations, visible direct-message/question badges, folded text, draft-preserving replies and project-wide send. Setup details are available on demand. Native sends were verified against schema15 and schema16 daemons. |
| Permission review and correct sender identity | Turn-scoped concrete paths/network grants are shown and returned exactly; ambiguous lexical paths fail closed. Implicit CLI sends use the actual registered provider. Actual Claude shell and interactive channel trials verify sender identity, queue receipts and replies. Broader review types remain below. |
| Stable Mac launcher and Applications destination | The hook/MCP executable collision is repaired. A writable system Applications folder is preferred, destination selection persists, and the old per-user path remains compatible. Trial prefixes stay contained. Packaged activation/rollback/uninstall and Launch Services checks pass; actual activation evidence is recorded on PR #119. |
| Notification and question navigation | Destinations select the correct conversation and retain other drafts. Existing-window and cold-window native routes pass. Physical Notification Center and signed posting remain below. |

`make install` from the repository root updates the app and CLI without stopping
active agents. `agentdocker desktop status` reports the installed source and
application path. The running daemon keeps its existing version until an explicit
safe restart; a successful app installation does not complete live replacement
or activate newer daemon-only features. The earlier blocked Claude prompt was
repaired, installed and confirmed by the user.

The September 14 live check confirms `/Applications/AgentDocker.app` and the
open GUI use `aaa1b61` (schema16-capable). The serving daemon remains the older
schema15 release; no login service is installed. In particular, its discovery
still offers a Chrome native-messaging helper that current source already
excludes. Reinstalling the same UI does not activate that daemon correction.

No old PR remains open. The last merged `claude/applications-folder` branch and
its clean worktree were removed by the connected Claude session on September 14
after checking inclusion in main. The September 12 archive tags retain the three
superseded draft histories. Branches created for this audit or subsequent fixes
are new work, not leftovers from PR #119.

The [30-minute actual Codex trial](verification/2026-09-12-thirty-minute-codex-queue.json)
passed 60 ordered human/peer inputs and exact replies, seven read-response cuts,
one retained controller/conversation and normal cleanup. Profile and binary
hashes were unchanged. It does not establish overnight, sleep/reboot or live
replacement acceptance.

## Engineering and acceptance still open

| Priority | Work remaining | Completion condition and evidence |
| --- | --- | --- |
| Top | Provider limits and interrupted-session recovery | The user reported the connected Claude session reached its limit on September 14. Implement explicit provider availability and bounded recovery: preserve the shared human/peer FIFO queue, distinguish queued/received/completed work, reconcile uncertain turns without replay, retain drafts/questions, and prevent ping/retry storms. Show only supported limit/reset evidence; a live process is insufficient. Test before/after receipt, mid-tool and pending-answer limits, queue pressure, provider/daemon restart and recovery under the original or explicitly rebound identity. Existing leases and approval expiry remain enforced. This is open, not covered by ordinary idle-wake tests. See the [delivery requirement](DELIVERY-PLAN.md#provider-session-limits-and-interrupted-work-september-14) and [message acceptance cases](MESSAGE-DELIVERY-AUDIT.md#provider-limit-and-session-exhaustion-acceptance-september-14). |
| Top | Complete provider input and review handling | Local command review now includes concrete additional permissions and connection context, rejects remote/incomplete requests, and checks the offered one-time decisions; its final validation/integration are pending. Finish network-only/stdin review, broader permission forms, MCP elicitation and secret-input presentation; test further actual-provider interruptions, uncertain writes and sustained conversations. Preserve one queue/order and exact receipts for human and peer input. Managed Claude channels and the owned Codex bridge have bounded idle/busy/question acceptance; hooks alone cannot wake an idle model. See [message audit](MESSAGE-DELIVERY-AUDIT.md), [Codex input](CODEX-INPUT.md) and [Claude question evidence](verification/2026-09-12-claude-question-queue.json). |
| Acceptance | Verify setup and input readiness on the installed candidate | Source now separates configuration, generation-bound MCP/hook contact and fresh input receiver/receipt evidence for each session. Generic activity no longer produces Connected; stale receivers, pauses and old-generation receipts cannot produce verified delivery. Required setup guidance stays in Details. PR #125 merged as `4d00bec` after all final-head CI workflows and source review. Its reviewed `ab8d718` passed 884 Rust tests, 65 Python checks and 165 native steps. An actual Codex 0.154.0 managed conversation passed six FIFO human/peer inputs, matching replies, a dropped read response and an idle heartbeat over 90 seconds with one identity and unchanged configuration. Hook/MCP lifecycle and bounded channel receipt tests also pass at their recorded binaries. Installed-candidate checks remain pending. See [guided setup](GUIDED-SETUP.md) and [desktop contracts](DESKTOP-UX.md). |
| High | Verify notification clicks in the installed app | Source removes the AppleScript fallback and implements destinations plus native existing/cold-window routing. Complete actual Notification Center clicks, signed posting and installed-launcher acceptance, including expired targets and retained drafts. See [notification audit](NOTIFICATION-ROUTING-AUDIT.md). |
| Next | Safe live daemon replacement | Retain child/process-group ownership, exact exit status, PTY/pipe I/O, logs, identity and leases; fence all writers/autostart; establish actual successor readiness and recover from failed transfer. Reconcile uncertain writes without blind replay. `daemon reload` remains unavailable. Event continuation, provider event reconnect, output drain and read-only queue retry are merged prerequisites, not completed replacement. See [replacement design](LIVE-DAEMON-UPGRADES.md). |
| Next | Apply legacy reconciliation to production state | September 14 private-snapshot inventory found two safe historical previews (489 moved inbox rows, 70 duplicate message copies), a third proven pair blocked only by hook/MCP provenance, and distinct live-Claude session IDs that must remain separate. Source now permits only the known provenance difference while archiving both records; its final gate and review are pending. Apply verified pairs with fresh plans and backup/recovery after the daemon and required sessions can safely finish. Do not merge by display name or stop a live process to tidy the list. See [identity repair](IDENTITY-REPAIR.md). |
| Engineering | Storage maintenance delivered; sustained acceptance remains | `[journal] retention` in `agentd.toml` applied by the minute tick in bounded batches, `journal prune --before <seq\|duration>`, `checkpoints prune --older-than` (live authors, acceptors and handoff addressees protected; rows and event in one transaction) and `daemon vacuum` (refused while sessions are live unless forced) are implemented with failure-injection, clock-skew and live-daemon tests. Journal heads survive pruning so sequences and cursors stay monotonic. What remains is T11-style sustained use with retention enabled. See [architecture](ARCHITECTURE.md#the-journal). |
| Acceptance | Sustained use and unresolved failures | Complete overnight, sleep/reboot, broader queue/retention and checkout workloads. A [one-hour 100-agent/10,000-file daemon trial](verification/2026-09-11-hour-sustained-use.json) passed 1,392,836 cycles; it does not cover the remaining cases. Diagnose the retained incomplete Iced capture and historical socket timeout: later passing trials do not explain them. See [testing standard](TESTING-AND-BENCHMARKS.md) and [local trial](LOCAL-TRIAL.md). |
| Release | Publish verified downloads and updates | Consumer, opt-in daily scheduler and release archive/feed automation exist. Align the cask template with the stable signing and managed-installation contract. `install.sh` now refuses to write over a managed installation and stages a replacement bundle beside the old one, keeping `AgentDocker.app.previous`; it remains a hand-copy route separate from managed activation. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and actual update/rollback acceptance. Cargo registry publication is not an established supported route. See [distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md) and [setup](DISTRIBUTION-SETUP.md). |
| Platform | Linux and independent Mac acceptance | Complete target-distribution desktop/service/package trials, a second Mac and physical Intel acceptance. ARM64/x86-64 graphical/package CI and Rosetta execution do not establish those hardware results. See [local trial](LOCAL-TRIAL.md). |
| Platform | Full native Windows product | Integrate daemon/clients with named pipes; finish supervision, ConPTY, identity-safe stop/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and native graphical CI. Existing core/host/desktop adapters are foundations. See [Windows port](WINDOWS-PORT.md). |
| Input | Hands-on accessibility and input methods | Exercise VoiceOver and supported-platform screen readers, keyboard navigation/activation, visible focus, zoom, IME, Unicode and terminal copy/paste. Range selection has [actual macOS acceptance](verification/2026-09-11-terminal-selection.json); broader human trials remain. Repair observed defects. See [Iced contracts](ICED-DESIGN.md). |
| Installed acceptance | Removed-checkout watcher recovery | Source ignores vanished checkout roots and reports lost coverage; independent macOS checkout streams fix a reproduced surviving-file event loss. Both regressions passed 100 repetitions. Verify after the launcher/daemon switch; historical conflict channels remain. See [watcher evidence](verification/2026-09-11-macos-watcher-recovery.json). |
| Engineering | Architecture cleanup delivered | The environment defaults the [September 6 audit](AUDIT-2026-09-06.md) found in `core::paths` (`AGENTDOCKER_HOME`, `LOCALAPPDATA`, `AGENTDOCKER_SOCKET`) now live in `agentdocker_host::dirs`; core derives paths only from a given home. No further pure-core exception is known. |
| Later | Existing optional proposals | Federation/host namespaces and cross-host leases/routing are unbuilt. Additional adapters, container log following and proposed CLI conveniences remain deferred behind the single-host desktop. They are existing scope, not prerequisites invented by this audit. See [product direction](PRODUCT-DIRECTION.md), [architecture proposals](ARCHITECTURE.md#planned-protocol-and-event-additions) and [containers](CONTAINER-ENGINES.md). |

## Manual and operational steps

| Step | What remains |
| --- | --- |
| Apple signing/notarization | Supply a Developer ID Application identity and private notary profile; run signing, notarization, stapling and Gatekeeper checks on the final app/DMG before publication. Packaging automation exists; ad-hoc signing only verifies a local preview. The September 12 identity check found no valid signing identities. |
| Human accessibility/IME trials | Run the input trials above and record findings on the actual candidate. Automated control/accessibility checks do not replace them. |
| Switch the running daemon after sessions finish | App/CLI activation can preserve current sessions; restart the old daemon explicitly only when its work can safely end. Verify the successor source/schema and retained state. The old daemon lacks live transfer, so an app update does not complete this step. |
| Independent release acceptance | Run second-Mac, Intel and target-Linux trials and sustained actual-provider sessions against the final candidate. |

See [desktop distribution](DESKTOP-DISTRIBUTION.md) for signing and private
credential handling, and [local trial](LOCAL-TRIAL.md) for the operational sequence.

## Retained evidence and release configuration

The [delivery plan](DELIVERY-PLAN.md), [message audit](MESSAGE-DELIVERY-AUDIT.md)
and [verification directory](verification/) retain the full history. In
particular, [delivery-status evidence](verification/2026-09-12-input-delivery-status.json)
retains the incomplete restored-window capture, and the
[integration benchmark failure](verification/2026-09-07-integration-benchmark-failure.json)
retains the socket timeout. Historical passes do not complete a new candidate's
release or hands-on gates.

A September 14 read-only GitHub check still finds v0.1.0 as the latest published
release and only a README in the tap's `Casks/` directory. A cask generator or
template is not a published install route. The September 9 check found the
publishing variable and token secret name configured; it did not inspect secret
values or verify token validity.

The connected Claude and Codex sessions exchanged this audit's ownership and
findings through AgentDocker on September 14. The peer acknowledged the message
and returned its findings. This verifies active-session routing and model
consumption; it does not replace an idle-wake or sustained-load trial.
