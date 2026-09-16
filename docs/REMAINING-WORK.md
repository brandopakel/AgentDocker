# Remaining engineering and release work

Audited September 14 against `aaa1b61` and updated through September 15 merged
`4074275` (PR #153). This is the current
backlog for the requirements already in the project documents. The
[documentation index](README.md) records coverage of all 38 Markdown files;
the [delivery crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk) records
which test categories are complete or partial. Dated audits and
[verification reports](verification/) retain the implementation history,
source-specific results and failed trials.

## Delivered source and current desktop

September 15 current installed checkpoint: verified `cf64ca3` (schema20), with
production inputs identical to merged `4074275` (PR #153). The intact copied
launcher passes strict signature verification, old hook paths work, and all four
external provider identities/PIDs/birth times survived a backed-up coordinator
stop/start. The existing Codex receiver auto-bound and offered a peer message
through the native queue after two legacy offers were read and reconciled; exact
provider receipt and automatic next-turn wake subsequently passed without a
human prompt or manual native queue acknowledgement. See the
[activation evidence](verification/2026-09-12-launcher-hook-repair.json).

Earlier September 15 installed checkpoint: app and production daemon served
merged `3c8c2e1` (schema18), including stale-notice coalescing. All four live external
provider records retained their IDs, PIDs and process birth times; the existing
Claude/Codex connection exchanged messages after the switch. An old-client
autostart race initially relaunched the previous daemon, then an explicit
quit/stop/start selected the current binary; that failure still needs the live
upgrade contract. PRs #142/#147 are merged. PR #148's native Codex queue and PR #149's portable coordination skill are now merged after final review and CI. The installed existing-session wake/receipt trial now passes; fresh startup/reopen and broader acceptance remain open. The initial long-busy trial falsely paused a direct user turn; corrected `5aeb651` passed the full 967-Rust/70-Python gate and an actual 65-second regression plus rate-limit recovery. Corrected recovery34 at `bc0ea04` passed under real daemon supervision; failed trials remain recorded. PR #151's managed-network review is also merged after the final 61-focused/974-Rust/70-Python gate and CI; actual managed-network approval remains unverified.
Thirty queued human/peer inputs passed exact FIFO
receipts in its local fixture, with about five minutes to consume the burst;
no-prompt startup/resume and burst latency remain acceptance gaps.

Historical integration checkpoint: PR #127 (local command review) merged as `75a0d57`,
PR #128 (terminal cancellation/UTF-8) as `573d1f0`, and PR #130 (independent
process ownership and durable recovery) as `b28d24c`. Each completed final
source review and all five CI workflows. PR #130's reviewed `5120f03` passed
911 Rust tests (six skipped), 70 Python checks and actual owned-process restart,
controller-pressure, release-pin and delayed exit-retirement trials. Its merged
remote branch is deleted; Claude's local checkout is retained. This source also
includes the completed PR #129 narrow Inbox/readable-name work and PR #131
provider-availability implementation. Their source-review fixes and native
acceptance have been completed independently while Claude is usage-limited;
the original dirty Claude checkout is preserved. Combined integration and
installation are tracked on [PR #132](https://github.com/brandopakel/AgentDocker/pull/132). Claude availability is not a
dependency for further engineering.

Installation checkpoints below are historical. Use `agentdocker desktop status`
and `agentdocker daemon status` for the actual installed and serving versions;
an app update does not replace the daemon serving active sessions. Combined
`ec45cea` passed 931 Rust tests, 70 Python checks and 225 native workflow steps.
The final review corrections add regression coverage for legacy queue replies,
lease release before rejected recovery and runtime-specific generated names;
935 Rust tests and 70 Python checks pass. Final-source native acceptance and
integration results are recorded on PR #132.
The [message audit](MESSAGE-DELIVERY-AUDIT.md) closes bounded provider queue,
mid-tool, pending-answer and same-identity controller-replacement acceptance.

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
| Narrow conversations and readable names | PR #129 implementation is complete: narrow Inbox switches between the list and the chosen conversation, notifications open that conversation, drafts survive resizing/navigation, and generated adapter/app-launch identifiers render as tool names. Returning to the list cancels delayed answer navigation. Final source `3b39a10` passed 916 Rust tests, 70 Python checks and 211 native workflow steps. |
| Permission review and correct sender identity | Turn-scoped concrete paths/network grants are shown and returned exactly; ambiguous lexical paths fail closed. Implicit CLI sends use the actual registered provider. Actual Claude shell and interactive channel trials verify sender identity, queue receipts and replies. Broader review types remain below. |
| Applications destination and compatible command paths | The hook/MCP executable collision is repaired. A writable system Applications folder is preferred, destination selection persists, and the old per-user path remains compatible. Trial prefixes stay contained. Packaged activation/rollback/uninstall and Launch Services checks pass; actual activation evidence is recorded on PR #119. |
| Pure core environment cleanup | Environment defaults moved from `core::paths` into `agentdocker_host::dirs`; the architecture cleanup item is complete. |
| Independent process ownership | PR #130 is merged and tested; session owners preserve child identity, logs and exact exit status across coordinator failure. Automated live replacement remains separate work below. |
| Provider-limit queue and recovery framework | Implemented in PR #131: durable availability, gated human/peer delivery, exact recovery, explicit provider/quota isolation, Claude and Codex structured signals, MCP reporting and desktop recovery. Source-review corrections passed 926 Rust tests and 70 Python checks. Bounded actual-provider and cross-runtime acceptance is recorded in the message audit; remaining adapter and interruption cases stay below. |
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

At the PR #119 checkpoint, no older PR remained open. The last merged `claude/applications-folder` branch and
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
| User request, September 15 | Portable coordination instructions | One bundled SKILL.md supplies MCP onboarding and preview/apply/undo setup; `agentdocker skill` exports it without a daemon. Combined source `4eb92a7` passed 969 Rust tests (six skipped), 70 Python checks and the full release gate. Actual Codex and Claude skill discovery passed on the recorded earlier release; actual Claude setup/undo also passed; the unchanged skill plus the native queue fix passed a 65-second direct-user busy trial at `528e4e0`, including idle wake, drafts, FIFO and receiver recovery. PR #149 merged after final CI and review; installation, implicit model activation and additional runtime loader acceptance remain. See [guided setup](GUIDED-SETUP.md#shared-coordination-skill). |
| Top | Complete native input lifecycle and throughput | The receiver connects the human/peer queue to Codex 0.154.0 native input with exact receipts and supervised recovery. The long-busy fix at `5aeb651` passed 967 Rust tests, 70 Python checks and actual 65-second busy/idle/draft/FIFO acceptance; rate-limit recovery passed. Corrected recovery trial 34 at `bc0ea04` passed lost-reply and uncertain-input handling under daemon supervision. Failed trials remain recorded. PR #148 merged after final CI and review. Installed `cf64ca3` auto-bound the existing Codex session; a peer input held during its busy turn automatically started the next ordinary turn, produced the exact thread/turn/item receipt and was acknowledged without another human prompt. No-prompt startup/reopen, burst latency and other providers need further work. See [Codex input](CODEX-INPUT.md) and [evidence](verification/2026-09-15-native-codex-queue.json). |
| Top | Broader provider-limit acceptance and adapter-specific detection | Shared availability state, schema18 persistence, strict recovery, queue gating, shared-quota isolation, Claude StopFailure, managed Codex errors, MCP reporting and desktop status/resume are implemented in the current change. Focused tests cover every catalog runtime plus custom runtimes and all nine normalized interruption classes. Bounded actual Claude/Codex integration, Codex same-session recovery, 181 native steps and the real-daemon pressure/restart matrix passed at `64f8e58`. An actual Codex mid-tool 429 trial at `66c4247` also passed: the owned command completed once and later input recovered without replay. Combined `ec45cea` also passed a pending-denial block/recovery and owned-controller replacement under the same agent/conversation identity. Remaining acceptance: actual account reset, broader provider versions/detection, unrelated replacement identities and sustained use. An unsupported adapter has no inferred limit signal; unknown scope/reset stay unknown. Do not equate the common contract with verified automatic detection for every company/model. See the [message audit](MESSAGE-DELIVERY-AUDIT.md#provider-limit-and-session-exhaustion-acceptance-september-14). |
| Top | Complete provider input and review handling | Local command review now includes concrete additional permissions and connection context, rejects remote/incomplete requests, and checks the offered one-time decisions; PR #127 merged after final-source review and five successful CI workflows; reviewed `723e794` passed 896 Rust tests, 70 Python checks and 170 native steps, with bounded actual Codex Allow/Deny trials at the recorded candidate. Managed-network approval presentation, grouped-destination scope disclosure and receipt/refusal regressions are now merged in PR #151 after final review and CI, with 61 focused input tests and the full 974-Rust/70-Python release gate passing. Final review fixes at `b605f8e` also passed that gate: optional context cannot inject display controls, and restored command records cannot carry a network presentation. An actual private-profile trial was blocked by the provider allowlist before an approval callback; managed-network provider acceptance remains open. Finish stdin review, broader permission forms, MCP elicitation and secret-input presentation; test further actual-provider interruptions, uncertain writes and sustained conversations. Preserve one queue/order and exact receipts for human and peer input. Managed Claude channels and the owned Codex bridge have bounded idle/busy/question acceptance; hooks alone cannot wake an idle model. See [message audit](MESSAGE-DELIVERY-AUDIT.md), [Codex input](CODEX-INPUT.md) and [Claude question evidence](verification/2026-09-12-claude-question-queue.json). |
| Acceptance | Verify setup and input readiness on the installed candidate | Source now separates configuration, generation-bound MCP/hook contact and fresh input receiver/receipt evidence for each session. Generic activity no longer produces Connected; stale receivers, pauses and old-generation receipts cannot produce verified delivery. Required setup guidance stays in Details. PR #125 merged as `4d00bec` after all final-head CI workflows and source review. Its reviewed `ab8d718` passed 884 Rust tests, 65 Python checks and 165 native steps. An actual Codex 0.154.0 managed conversation passed six FIFO human/peer inputs, matching replies, a dropped read response and an idle heartbeat over 90 seconds with one identity and unchanged configuration. Hook/MCP lifecycle and bounded channel receipt tests also pass at their recorded binaries. Installed-candidate checks remain pending. See [guided setup](GUIDED-SETUP.md) and [desktop contracts](DESKTOP-UX.md). |
| High | Verify notification clicks in the installed app | Source removes the AppleScript fallback and implements destinations plus native existing/cold-window routing. PR #153 is merged after final CI and review. The launcher integrity repair preserves the signed payload intact and redirects its entry points to the active immutable release; the 979-Rust/71-Python gate, 13 private install scenarios, 12 actual older-release upgrade/rollback scenarios and 26 copied-launcher routing steps passed at the recorded candidate. Production activation and strict signature verification passed with live provider identities preserved. Complete actual Notification Center clicks and signed posting, including expired targets and retained drafts. See [notification audit](NOTIFICATION-ROUTING-AUDIT.md). |
| Next | Safe live daemon replacement | Child/process-group ownership, exact exit status, PTY/pipe I/O, logs, scrollback and identity now live in a per-agent session owner that outlives the daemon and is reattached on restart (schema 17), merged in PR #130. Final `5120f03` passed real daemon crash/restart, 12-second stopped-owner recovery, 80 controller replacements, release-pin retention and durable disk-exit retirement. A further distinct-source `880e111` → `5120f03` daemon restart passed with batch/PTY children, a 12-second owner outage, unchanged child/owner identities and lease, 40 FIFO messages, ordered logs and exact exit codes; all owned processes retired. Remaining: fence all writers/autostart; establish actual successor readiness and recover from failed transfer. Reconcile uncertain writes without blind replay. `daemon reload` remains unavailable. Event continuation, provider event reconnect, output drain and read-only queue retry are merged prerequisites, not completed replacement. See [replacement design](LIVE-DAEMON-UPGRADES.md). |
| Next | Apply legacy reconciliation to production state | September 14 private-snapshot inventory and corrected previews found three safe historical pairs (575 moved inbox rows, 139 duplicate message copies); distinct live-Claude session IDs remain separate. PR #126 merged as `3dc6ee7` after all final-head CI workflows and source review. Reviewed `746ec69` passed 879 Rust tests, 67 Python checks and seven actual repair/restart steps. Source permits only the known hook/MCP provenance difference while archiving both original records. Apply verified pairs with fresh plans and backup/recovery after the daemon and required sessions can safely finish. Do not merge by display name or stop a live process to tidy the list. See [identity repair](IDENTITY-REPAIR.md). |
| Engineering | Storage maintenance delivered; sustained acceptance remains | `[journal] retention` in `agentd.toml` applied by the minute tick in bounded batches, `journal prune --before <seq\|duration>`, `checkpoints prune --older-than` (live authors, acceptors and handoff addressees protected; rows and event in one transaction) and `daemon vacuum` (refused while sessions are live unless forced) are implemented with failure-injection, clock-skew and live-daemon tests. Journal heads survive pruning so sequences and cursors stay monotonic. A [20-minute retention trial](verification/2026-09-15-retention-sustained-use.json) on release binaries (`scripts/retention_sustained.py`: ten registered agents, `[journal] retention = "120s"`, half the population leaving at half time, checkpoints of finished agents pruned every minute) passed twice, the second time with every claimed assertion (per-sample oldest age, eligible checkpoints by id, per-reader ordering, hashed private daemon copy): retention pruned 18 times by itself with the oldest entry at most 179 s old at any sample against the 120 s window, ten readers with pre-prune cursors read 500 pages in order, all 420 eligible finished checkpoints were removed by id and all 845 live ones kept, and the database and daemon memory stayed bounded (1.01x and 1.00x of the middle third) from the tenth minute on under a load that halves at half time, 12,680 cycles, no daemon warnings. What remains is longer than 20 minutes, sleep/reboot, and provider sessions rather than registered records. See [architecture](ARCHITECTURE.md#the-journal). |
| Acceptance | Sustained use and unresolved failures | Real coordinated use on September 15 showed one branch switch producing hundreds of stale and contested-channel notices, one per change per path; the daemon now sends one stale notice per reader and one widening notice per channel on each second's tick, and holds further stale paths while a reader's last notice is still queued (see [architecture](ARCHITECTURE.md#projects)). Complete sleep/reboot and provider-session workloads. A [one-hour 100-agent/10,000-file daemon trial](verification/2026-09-11-hour-sustained-use.json) passed 1,392,836 cycles, a [7.5-hour overnight run](verification/2026-09-14-overnight-sustained-use.json) of 1, 10 and 100 agents (2.5 hours each, 10,000 files) passed 35,050 / 350,118 / 3,470,242 cycles with the daemon under 33 MiB and, over the last 100,000 requests of each population, p99 request latency under 10 ms on a host also running provider sessions and build gates (that binary predates session owners and provider availability), and a [20-minute retention trial](verification/2026-09-15-retention-sustained-use.json) covers journal retention, checkpoint pruning and growth bounds with agents leaving mid-run; none covers sleep/wake, reboot or provider sessions. Diagnose the retained incomplete Iced capture, historical socket timeout and September 15 Linux ARM transport refusal. Bounded inspector diagnostics now retain future failures; the earlier helper discarded the observation, so a later passing trial cannot explain it. See [testing standard](TESTING-AND-BENCHMARKS.md) and [local trial](LOCAL-TRIAL.md). |
| Release | Publish verified downloads and updates | Consumer, opt-in daily scheduler and release archive/feed automation exist. Align the cask template with the stable signing and managed-installation contract. `install.sh` installs the desktop through the app's own installer by default on macOS and refuses to write over a managed installation on its commands route; the cask links the app's own commands, conflicts with the formula and stops the login service on uninstall, so every route is one installation. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and actual update/rollback acceptance. Cargo registry publication is not an established supported route. See [distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md) and [setup](DISTRIBUTION-SETUP.md). |
| Platform | Linux and independent Mac acceptance | Complete target-distribution desktop/service/package trials, a second Mac and physical Intel acceptance. ARM64/x86-64 graphical/package CI and Rosetta execution do not establish those hardware results. See [local trial](LOCAL-TRIAL.md). |
| Platform | Full native Windows product | Integrate daemon/clients with named pipes; finish supervision, ConPTY, identity-safe stop/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and native graphical CI. Existing core/host/desktop adapters are foundations. See [Windows port](WINDOWS-PORT.md). |
| Input | Hands-on accessibility and input methods | Exercise VoiceOver and supported-platform screen readers, keyboard navigation/activation, visible focus, zoom, IME, Unicode and terminal copy/paste. Range selection has [actual macOS acceptance](verification/2026-09-11-terminal-selection.json); broader human trials remain. Repair observed defects. See [Iced contracts](ICED-DESIGN.md). |
| Installed acceptance | Removed-checkout watcher recovery | Source ignores vanished checkout roots and reports lost coverage; independent macOS checkout streams fix a reproduced surviving-file event loss. Both regressions passed 100 repetitions. Verify after the launcher/daemon switch; historical conflict channels remain. See [watcher evidence](verification/2026-09-11-macos-watcher-recovery.json). |

| Later | Existing optional proposals | Federation/host namespaces and cross-host leases/routing are unbuilt. Additional adapters, container log following and proposed CLI conveniences remain deferred behind the single-host desktop. They are existing scope, not prerequisites invented by this audit. See [product direction](PRODUCT-DIRECTION.md), [architecture proposals](ARCHITECTURE.md#planned-protocol-and-event-additions) and [containers](CONTAINER-ENGINES.md). |

## Requested September 15

| Priority | Work remaining | Completion condition and evidence |
| --- | --- | --- |
| Next | Messaging as a workspace (Slack/Discord shape) | Daemon side in source (schema 21): conversations by destination (everyone, all, named channels, collision rooms, direct pairs, notices), the bounded message archive written beside the queues with retention, cap and FTS search, per-conversation read cursors that never regress, threads by `reply_to` within a conversation, humans no longer admitted to collision rooms; `conversations`, `history`, `thread`, `mark_read`, `search_messages`, `channel_open name`; CLI `conversations`, `history [--read]`, `thread`, `search`, `channel open --name`; tests for archive and threads, cursors and unread, names and notices. The app's Messages screen is in source: sidebar (channels, collisions, direct messages with presence, notices, search, *Earlier* for ended sessions), the archive pane with day and unread dividers and question cards in place, threads by `reply_to`, an always-present composer, reading marks read, notifications route to archived messages, and sessions named by tool and branch (numbered only while two live ones share a branch); `scripts/iced_workflow_smoke.py` drives it (reading acknowledges rows, unread reaches zero, the narrow route and drafts). Review regressions now keep finished-session messages searchable within their project, prevent cross-project direct-message rows and rebuild an index skipped after rollback even with equal row counts; nine focused tests passed, with the three original failures retained in [archive evidence](verification/2026-09-11-session-messages.json). Current-main integration `7897fb4` passed the full 999-Rust/77-Python gate and 233 rendered native workflow steps, including archive reads/unread, question routes, narrow conversation/thread drafts and readiness/limit handling. Final CI/review and installed schema21 acceptance remain. Sessions have no History tab: ended sessions sit in a collapsed *Earlier* group under the current ones; searches matching only that group no longer show a contradictory empty state, with rendered regression coverage added (candidate validation pending). Each project row has a menu (rename here, pin, remove from the list for good — discovery no longer brings a removed folder back) and two projects of one name show their parent folder. Still ahead: mentions and a mention count, and a way to tell every agent in a project to pause (today `agentdocker send --to project`, which reaches them all, is the pause; a `pause` request with a reason and an app control would make it one action). Done when the person opens any conversation and replies in place, unread counts are per conversation, and a branch switch adds nothing to the person's count. |
| Next | Portable coordination skill | One instruction source in the repo from which both the MCP server instructions and a `SKILL.md` (frontmatter name/description, harness-agnostic body, execution through the `agentdocker` CLI) are generated; `agentdocker setup` installs the skill into each supported harness's skill directory, and the setup check reports which harnesses load skills at all. Done when a fresh Claude Code or Codex session in a coordinated checkout uses AgentDocker without being told, and the two texts cannot drift. |
| Next | Token usage by agent, model and provider | A host collector reads each runtime's own local log (Codex rollouts under `~/.codex/sessions`, Claude Code transcripts under `~/.claude/projects`), attributes turns to registered agents by session id, and the daemon aggregates input, cached, output and reasoning tokens by agent, model and provider over a time range; a Usage screen and `agentdocker usage` show tokens (never money unless the user configures prices), with AgentDocker's own overhead (bytes it injected as hook context, MCP results and queued messages, with an estimated token count labelled as such) kept apart from provider tokens, and coverage explicit: a runtime whose log is not read shows unknown, not zero. |

## Manual and operational steps

| Step | What remains |
| --- | --- |
| Apple signing/notarization | Supply a Developer ID Application identity and private notary profile; run signing, notarization, stapling and Gatekeeper checks on the final app/DMG before publication. Packaging automation exists; ad-hoc signing only verifies a local preview. The read-only September 15 identity check again found zero valid signing identities. |
| Human accessibility/IME trials | Run the input trials above and record findings on the actual candidate. Automated control/accessibility checks do not replace them. |
| Completed on this Mac: launcher and coordinator switch | Verified `cf64ca3` (schema20) is running after a backed-up coordinator stop/start. All four external provider identities, PIDs and birth times were preserved; strict launcher signature checks and legacy hook paths pass. This closes the old-launcher switch, not the experimental live-transfer or future-upgrade acceptance above. |
| Independent release acceptance | Run second-Mac, Intel and target-Linux trials and sustained actual-provider sessions against the final candidate. |

See [desktop distribution](DESKTOP-DISTRIBUTION.md) for signing and private
credential handling, and [local trial](LOCAL-TRIAL.md) for the operational sequence.

## Retained evidence and release configuration

The September 14 terminal follow-up bounds read-side shutdown and preserves
partial frame bytes across read deadlines. A deterministic reproduction found
that the first timeout draft's `read_line` could discard a split UTF-8 character;
the correction retains bytes and checks closure between chunks. Focused tests
cover actual socket deadlines at every split, following frames, continuous
unterminated input and closure without a usable shutdown handle. PR #128 merged
as `573d1f0` after source review and all five final-head CI workflows passed.
Reviewed `66c0946` passed 890 Rust tests, 70 Python checks and 165 native workflow
steps; 100 independent repetitions of its 14 terminal tests also passed. The
earlier macOS CI writer-failure test
timeout remains recorded; these checks do not establish its original OS-level
cause or explain the separate historical benchmark socket timeout.

A separate five-minute retention trial at clean `66c0946` exercised the actual
minute tick with retention enabled. It recorded 1,532 explicit journal notes and
331 mixed human/peer inputs, preserving the entire FIFO queue through five prune
batches and a daemon restart. The first batch hit the 1,000-row bound; the final
durable journal head advanced from 1,534 to 1,535 after restart. No messages were
acknowledged or drained, and no owned process remained. This closes the bounded
retention/restart case; it does not complete overnight, sleep/reboot or actual
provider recovery acceptance. The connected Claude's separate longer soak uses
an earlier binary and does not enable retention.

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
