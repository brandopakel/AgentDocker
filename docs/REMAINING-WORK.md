# Remaining engineering and release work

September 17 UTC integration: source `f4ef6c3` passed the full 1,087-Rust/84-Python
gate (seven skipped), formatting, strict lint, doctests, packaging and release.
Earlier actual reload/controller trials retain their original source pins;
this gate does not claim a new runtime trial or production installation.
PR #155 merged as `fec093c` after final-head review and CI; its included #161
is also merged. PR #169 merged as `82ff4da` after final review and all CI checks. Its source `5545697` passed the 1,093-Rust/84-Python gate
(seven skipped), 14 focused tests and actual-client active-hook/lost-output trials.
Review follow-up `e896111` passed 1,094 Rust tests (seven skipped), 84 Python
checks and both actual-client repeats, adding kernel hook-peer authentication
and explicit provider-visible order/exactly-once assertions.
The hook endpoint follows the existing trusted owning-user host boundary; it does
not authenticate mutually untrusted same-user subprocesses. This limit and the
possibility of a fabricated call stalling an offer are explicit in
[Codex input](CODEX-INPUT.md).

The installed receiver upgrade remains open; see the priority row below.
A separate candidate now implements an explicit same-provider receiver upgrade,
with schema-22 durable replacement intent and retained ledger reconciliation.
Source `5b3d688` passed three focused regressions, the full 1,097-Rust/84-Python
gate (seven skipped), and actual Codex 0.154.0/local-model replacement of an older
receiver with a pending offer. The provider, token, binding time and six prior
receipts survived; three messages arrived exactly once in order during the same
active turn (14.85 seconds including handover), with clean retirement and cleanup.
The disposable installed-package repeat also passed, observing lifetime pins
and successful pruning of the retired release after cleanup.
Final review follow-up `8d42db5` defaults the upgrade identity from the environment and documents the request table. The full 1,097-Rust/84-Python gate passed again (seven skipped), as did a repeat with the actual older receiver: the same provider, token and six prior receipts survived, and three messages entered the same active turn in order in 14.84 seconds with clean cleanup.
Final review, CI and installed-session acceptance remain open. Evidence is in the
existing [native queue record](verification/2026-09-15-native-codex-queue.json).


Audited September 14 against `aaa1b61`, with merged PRs #150/#152/#154, the September 15
`72b1eb4` installed preview and PR #155 review acceptance recorded below. This is the current
backlog for the requirements already in the project documents. The
[documentation index](README.md) records coverage of all 38 Markdown files;
the [delivery crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk) records
which test categories are complete or partial. Dated audits and
[verification reports](verification/) retain the implementation history,
source-specific results and failed trials.

## Delivered source and current desktop

Latest read-only check on September 16 confirms installed UI source `1e90f83`
(schema21, release `3add4bea`), with daemon PID 3366 and GUI PID 4519 running
from that release. The existing Codex provider PID 51242 and receiver PID 94744
remain alive; this observation does not establish all queued-message receipts.
PR #160 merged as `37725fe` after final review and CI.

Previous observed installation: Claude installed UI source `72b1eb4` (schema21)
on September 15. Read-only verification confirms that release serving as daemon
PID 67791 and GUI PID 67836, with strict bundle signature verification passing.
This Codex provider and receiver retained their PIDs; the switch itself was done
by Claude. A subsequent read-only check confirms native receipt of queued message
`b76fa0ce74ca4519` at 05:49:38 UTC in the unchanged Codex thread, after this
installation. No manual inbox read or acknowledgement was used. PR #152 subsequently merged
as `ece76fd` after final review and CI; broader startup/provider and installation
acceptance remain pending. See the
[observed installation](verification/2026-09-12-integrated-desktop.json).
The reload candidate now integrates messaging/schema21 and the validated UI
changes, including Linux shared-temporary-root discovery. Archive migration,
cursor acknowledgement and retention now obey the handover fence and atomic
event contract. Combined `a1475a1` passed 1,053 Rust tests and 81 Python checks,
plus an actual schema20-to-21 handover preserving queued/history messages,
lease and native batch/PTY identity and exits. Later review fixes now make
channel actions atomic with their ancillary effects, fence terminal mutations,
and join stress-test workers. Combined `64d4762` passed 1,056 Rust tests, 83
Python checks, 275 native UI steps, 20 pressured handovers (334 ordered
messages, 127 completed launches, no warnings/survivors), four native Codex
loopback handovers and another distinct-source schema20-to21 migration. Final
PR #155 review/CI and production activation remain pending. Review followup
`981be68` passed 1,058 Rust tests, 84 Python checks, 20 pressured handovers
(625 FIFO messages, 211 finished launches, four retained deferred-exit/recovery
warnings, no survivors), four native Codex loopback handovers, private desktop
install/rollback and retained-version pin trials. Lost reload replies now report
an unknown outcome and probe the serving daemon without replay; a Linux pin-trial
regression is fixed. See the existing
[reload evidence](verification/2026-09-16-reload-controller-episode.json).

September 15 prior installed checkpoint: verified `cf64ca3` (schema20), with
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
| Simpler home, Inbox and Tools | Home groups agents by project with a short attention strip. Inbox has agent conversations, visible direct-message/question badges, folded text, draft-preserving replies and project-wide send. Setup details are available on demand. Native sends were verified against schema15 and schema16 daemons.  The PR #152 Linux graphical failure exposed shared `/tmp` projects being hidden by the new temporary-directory filter; source now distinguishes shared scratch roots from per-user fixture roots, with the path-classifier regression passing. Corrected `e0d6f8a` passed the full 1006-Rust/77-Python gate and 263 rendered native workflow steps with `TMPDIR=/tmp`, preserving discovery, conversation reads, drafts and provider-limit recovery. Linux ARM/x86 and both Mac CI jobs passed that candidate. Final review fixes at `2a51d13` exclude interpreter-launched sidecars, prevent a Unicode room-name panic and use saved renamed-project headings. The full 1007-Rust/77-Python gate and 275 rendered native workflow steps passed, including the renamed heading. PR #152 merged as `ece76fd` after final GitHub CI/review; installed acceptance remains. See [integrated evidence](verification/2026-09-12-integrated-desktop.json).|
| Narrow conversations and readable names | PR #129 implementation is complete: narrow Inbox switches between the list and the chosen conversation, notifications open that conversation, drafts survive resizing/navigation, and generated adapter/app-launch identifiers render as tool names. Returning to the list cancels delayed answer navigation. Final source `3b39a10` passed 916 Rust tests, 70 Python checks and 211 native workflow steps. |
| Permission review and correct sender identity | Turn-scoped concrete paths/network grants are shown and returned exactly; ambiguous lexical paths fail closed. Implicit CLI sends use the actual registered provider. Actual Claude shell and interactive channel trials verify sender identity, queue receipts and replies. Broader review types remain below. |
| Applications destination and compatible command paths | The hook/MCP executable collision is repaired. A writable system Applications folder is preferred, destination selection persists, and the old per-user path remains compatible. Trial prefixes stay contained. Packaged activation/rollback/uninstall and Launch Services checks pass; actual activation evidence is recorded on PR #119. |
| Pure core environment cleanup | Environment defaults moved from `core::paths` into `agentdocker_host::dirs`; the architecture cleanup item is complete. |
| Independent process ownership | PR #130 is merged and tested; session owners preserve child identity, logs and exact exit status across coordinator failure. Automated live replacement remains separate work below. |
| Provider-limit queue and recovery framework | Implemented in PR #131: durable availability, gated human/peer delivery, exact recovery, explicit provider/quota isolation, Claude and Codex structured signals, MCP reporting and desktop recovery. Source-review corrections passed 926 Rust tests and 70 Python checks. Bounded actual-provider and cross-runtime acceptance is recorded in the message audit; remaining adapter and interruption cases stay below. |
| Notification and question navigation | Destinations select the correct conversation and retain other drafts. Existing-window and cold-window native routes pass. Physical Notification Center and signed posting remain below. |

`make install` from the repository root updates the app and CLI without stopping
active agents. `agentdocker desktop status` reports the installed source and
application path, and what daemon actually serves. An installation asks a
running daemon to reload to the activated release and reports its answer; while
the reload gate is off, the daemon refuses and keeps serving its release until
an explicit safe restart, and the report says so. The earlier blocked Claude
prompt was repaired, installed and confirmed by the user.

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

The reconnect review found a channel ownership race if SessionStart folds a
registration after MCP acquires its agent-ID lock. Source now holds an additional
provider-process-generation lock, and hooks check it across an ID change. An
already initialized receiver stays separate to preserve its offered message and
the old backlog. Integrated `b5ea76c` passed the full 1,094-Rust/84-Python
gate (seven skipped), formatting, strict lint, doctests, packaging and release.
The actual daemon/MCP transport trial passed both regression paths and eight
existing channel cases in 38.66 seconds, with zero surviving processes. The old
binary admitted a duplicate channel and failed the same driver as expected.
[Evidence](verification/2026-09-11-claude-channel-input.json) retains both results.
Final review/CI and actual existing-Claude startup/idle-wake acceptance remain
open; the fixture speaks MCP and does not prove a model was woken.
The next combined review found that a different-active-turn steering refusal was
handled like a just-finished turn. Source now distinguishes them and pauses on
changed ownership, retaining the original receipt and the unsubmitted queue
message. A new actual-client scenario injects that refusal followed by completion
of the reported other turn. Source `36ac92b` passed the full 1,077-Rust/84-Python
gate (seven Rust tests skipped), lint, packaging and release build. The new
actual-Codex scenario passed, retaining the queued message once; the prior binary
failed the same scenario as expected. Busy, just-finished and lost-reply recovery
trials also passed without duplicate submission. The existing
[reload evidence](verification/2026-09-16-reload-controller-episode.json) retains
the reproduction, an aborted build campaign and all passing reruns. Final CI and
review remain pending.

The next reload review also found the transfer-refusal reply still used a
blocking write on the async startup path. Both readiness outcomes now share the
blocking-worker helper; a regression holds the predecessor socket unread and
checks that async work progresses. The initial fixture failed because macOS
rejected the filled socket's descriptor header immediately; that failure is
retained. Corrected source `71033b7` passed 1,075 Rust tests (seven skipped), 84
Python checks and the full packaging/release gate. Its combined binaries passed
20 handovers with 900 FIFO messages and 461 finished launches, four actual Codex
TUI handovers preserving a draft and pending question, and owned-input
busy/refusal/lost-reply trials without duplicate submission. Fourteen deferred
exit warnings covered seven agents; each exit was subsequently recorded. All
fixtures retired. Final combined source `0993d1b`, including PR #161's
ended-session thread-send guard and merged PR #163's unread fix, passed 1,077 Rust
tests (seven skipped), 84 Python checks, lint, packaging and release build.
Its CLI and daemon hashes match the native-trial binaries. Final CI and review
remain pending. Experimental reload remains gated; production was not switched
by these trials.

PR #155 next review found discovery scans could mutate the cached projection
while fenced, and reload could race restricted-listener registration. Source now
refuses mutating discovery requests during transfer, retains the pre-transfer
scan snapshot, and checks/takes descriptors under the registration lock.
Successor readiness writes run on a blocking worker with a bounded timeout.
Source `0bcde3c` passed the full 1,068-Rust/84-Python gate and 20 pressured
handovers with 900 ordered messages and 563 finished launches. Ten deferred-exit
warnings occurred behind transfer fences; every affected exit was eventually
recorded and no fixture process survived. The first actual Codex trial completed four
handovers but failed its immediate completed-history assertion. Daemon queue
acknowledgement precedes moving the persisted receipt into local completed
history; the corrected harness waits for that transition. Its rerun passed all
four handovers and the exact answer receipt with no survivors. The retained
release-pin trial also passed. Final source `54a47f8` passed the full
1,068-Rust/84-Python gate; final GitHub CI and review remain pending.

PR #155 final-head review follow-up (September 16) now rolls back an agent
whose first write fails or is fenced, cleans an isolated pane worktree after
successful pane retirement, reports failed aborts without implying writes resumed,
and bounds repeated empty stream/attachment reconnects with backoff. Transfer IDs
use Unicode-safe formatting. The first follow-up gate passed 1,060 Rust tests but failed the real reload
stream test: shutdown during reconnect backoff became a connection error. Source
now probes after the backoff. Corrected `09e1559` passed 1,065 Rust tests and
84 Python checks, 20 pressured handovers (312 FIFO messages, 128 finished
launches, no daemon warnings or survivors), four actual Codex/loopback
handovers and the retained-release pin trial. The native client kept its thread,
draft, provider identity, busy-message ordering and exact question-answer receipt.
Final review/CI remain pending; the earlier failed trials are retained in the
[reload record](verification/2026-09-16-reload-controller-episode.json).

The next idle-connection review found thread replies still enabled for ended
direct sessions. Both composers and the submit handler now share canonical
recipient liveness, preserve both drafts after an ended-session submit, and
allow replies again when the canonical session is live. Source `f974f3d` passed
the full 1,018-Rust/77-Python release gate, including the ended-thread draft and
submission regression; final GitHub CI and review remain pending.

September 16 notification Hide trial on installed `1e90f83` routed the exact
message but left the private window hidden. Source now explicitly unhides the
macOS application before window focus. Candidate `2de5994` passed the actual
Notification Center click, exact message selection and visible-window assertion,
plus the full 1,013-Rust/77-Python release gate. The installed poster and private
candidate window were used; production sessions were retained.
The same follow-up applies canonical-agent liveness consistently to the
Messages sidebar, header and composer. A 375-step rendered workflow rerun passed;
an earlier same-source run unexpectedly left Sessions for Messages before its
search step and timed out. Its cause remains unisolated and the failure is
retained in the [input evidence](verification/2026-09-12-input-delivery-status.json).

September 16 idle-connection UI source `e417ca5` passed 1,009 Rust tests,
77 Python checks and 338 rendered native workflow steps. New supported launches
default to idle delivery, and direct/thread composers expose receiver readiness
with draft-preserving Connection guidance. This closes the bounded UI wiring;
existing-session reconnect, standalone-terminal active input and broader provider
acceptance remain open. The interrupted pre-sleep gate is retained beside the passing rerun in
[input-delivery evidence](verification/2026-09-12-input-delivery-status.json).

Usage-accounting foundation PR #165 merged as `f6f2f96` after final review and
CI. Optional counters, reset handling, time bounds and versioned Codex/Claude
normalization passed 1,026 Rust tests (seven skipped), 77 Python checks, lint,
packaging and release build. The file reader is under separate review in #167;
directory discovery, atomic ingestion, the usage command and screen remain open.

September 16 owned Codex active-input source `7fbb8c4` passed the full
1,015-Rust/77-Python gate. Actual Codex 0.154.0 with a local model fixture
consumed CLI human, peer and human-broadcast inputs in the same active turn,
with distinct exact receipts. A separate dropped steering response recovered
automatically under the same agent/thread after controller restart, without
resubmission. These checks exercise the owned bridge; the existing standalone
TUI pause-delivery defect, other providers and real model-service acceptance
remain open. See [input evidence](verification/2026-09-11-codex-input-review.json).

The September 16 review follow-up suppresses further steering after an explicit
precondition refusal for the current turn, retaining its queue head until the
ordinary turn completes. Corrected `01531dc` passed the full 1,015-Rust/77-Python
gate and actual-Codex busy/broadcast, injected-refusal, and dropped-reply recovery
trials against the local model fixture. Existing
standalone TUI delivery still uses an idle-only native queue, and existing Claude
sessions without a channel receiver still need safe reconnect. Hosted-model and
broader provider acceptance remain open; no universal-provider completion is
claimed. See [Codex input](CODEX-INPUT.md#active-turn-steering-acceptance-september-16).

| Priority | Work remaining | Completion condition and evidence |
| --- | --- | --- |
| Top, reproduced September 16 | CLI broadcast input priority and pause delivery | Human `send --to all` message `5d0f2b149aa44cb6` is archived, but Codex kept working until the user repeated the pause directly. The app project pause `b8f2cb39f7f44b4c` reproduced it at 13:59 PDT: the idle-only receiver suppressed hook delivery during the active turn. PR #169 source `5545697` passed actual Codex 0.154.0/local-model busy input: peer, human project and global messages entered the same active turn within 8.7 seconds with exact receipts. Dropped hook output retained all queued IDs without false receipt or replay. The full 1,093-Rust/84-Python gate passed (seven skipped). Installed receiver replacement remains open: its fixed launch descriptor pins the older binary, so app installation alone does not upgrade delivery. The resume also requests @mentions, New DM/channel with invitations, and Enter to send; Claude owns those UI changes and acceptance. Trace fan-out, queue position, active-turn input/steering, controller offers and exact receipts. CLI input must match directly typed human input priority; peer messages share the provider route with their attribution intact. Test an idle and a busy recipient, provider waits, per-recipient pause receipt, draft preservation and duplicate prevention. PR #162 merged as `7c6e779`: the owned Codex bridge now uses the same active-turn input route for human CLI, broadcast and peer messages. Actual Codex 0.154.0 trials passed same-turn exact receipts, refusal fallback and lost-reply recovery without resubmission; combined reload binaries also passed those trials. The installed standalone Codex receiver remains idle-only until upgraded; the candidate adds tool-boundary delivery with a 6,000-byte context limit. Existing Claude sessions require a supported channel relaunch. Broad provider parity and a per-recipient pause control remain open; queue acceptance alone does not establish delivery. |
| Top, reproduced September 16 | Connect existing idle sessions and close provider gaps | Live Claude confirmed twelve peer messages waited for a human prompt (`835523c328494b82`); hooks/MCP had no channel receiver. New Claude/Codex UI launches now default to idle input and direct/thread composers show recipient readiness with connection guidance; provider consent remains required. The pre-sleep gate was interrupted at the user’s request; the resumed source e417ca5 passed 1,009 Rust tests, 77 Python checks and 338 native workflow steps. Final review adds fresh runtime discovery on Connection and canonical-recipient liveness for retired DM aliases; their follow-up source `1e1654a` passed 1,013 Rust tests, 77 Python checks and 375 rendered native workflow steps. This does not repair the existing session: safe resume must retain old queued IDs, receipts and drafts, then pass a peer-only idle turn. Other runtimes still need supported input adapters and their own acceptance. See [message audit](MESSAGE-DELIVERY-AUDIT.md). |
| User request, September 15 | Portable coordination instructions | One bundled SKILL.md supplies MCP onboarding and preview/apply/undo setup; `agentdocker skill` exports it without a daemon. Combined source `4eb92a7` passed 969 Rust tests (six skipped), 70 Python checks and the full release gate. Actual Codex and Claude skill discovery passed on the recorded earlier release; actual Claude setup/undo also passed; the unchanged skill plus the native queue fix passed a 65-second direct-user busy trial at `528e4e0`, including idle wake, drafts, FIFO and receiver recovery. PR #149 merged after final CI and review; installation, implicit model activation and additional runtime loader acceptance remain. See [guided setup](GUIDED-SETUP.md#shared-coordination-skill). |
| Top | Complete native input lifecycle and throughput | The receiver connects the human/peer queue to Codex 0.154.0 native input with exact receipts and supervised recovery. The long-busy fix at `5aeb651` passed 967 Rust tests, 70 Python checks and actual 65-second busy/idle/draft/FIFO acceptance; rate-limit recovery passed. Corrected recovery trial 34 at `bc0ea04` passed lost-reply and uncertain-input handling under daemon supervision. Failed trials remain recorded. PR #148 merged after final CI and review. Installed `cf64ca3` auto-bound the existing Codex session; a peer input held during its busy turn automatically started the next ordinary turn, produced the exact thread/turn/item receipt and was acknowledged without another human prompt. No-prompt startup/reopen, burst latency and other providers need further work. See [Codex input](CODEX-INPUT.md) and [evidence](verification/2026-09-15-native-codex-queue.json). A Claude session launched plainly (the user-level MCP entry, `claude --resume`) has no channel and is not woken by peer messages until the person types — the channel is launch-time only — so the answer is a relaunch: `--claude-channel` in the one configured entry now serves the ordinary MCP under a plain launch (the channel transport smoke asserts it), and a session that comes back as a new process takes up its ended records at registration (same `session_id`, runtime, project and checkout, processes gone; the last-ended stays, earlier lives and the newcomer fold into it with every queue in order; `session_resumed`; daemon regression `a_session_that_comes_back_takes_up_its_ended_record`, core rule `identity::resumed_session`), keeping its id, conversation, cursor and queue instead of becoming a second agent; a managed launch is a new supervised agent, not a resumption. Still open: making the channel launch the default for sessions started outside the app. |
| Top | Broader provider-limit acceptance and adapter-specific detection | Shared availability state, schema18 persistence, strict recovery, queue gating, shared-quota isolation, Claude StopFailure, managed Codex errors, MCP reporting and desktop status/resume are implemented in the current change. Focused tests cover every catalog runtime plus custom runtimes and all nine normalized interruption classes. Bounded actual Claude/Codex integration, Codex same-session recovery, 181 native steps and the real-daemon pressure/restart matrix passed at `64f8e58`. An actual Codex mid-tool 429 trial at `66c4247` also passed: the owned command completed once and later input recovered without replay. Combined `ec45cea` also passed a pending-denial block/recovery and owned-controller replacement under the same agent/conversation identity. Remaining acceptance: actual account reset, broader provider versions/detection, unrelated replacement identities and sustained use. An unsupported adapter has no inferred limit signal; unknown scope/reset stay unknown. Do not equate the common contract with verified automatic detection for every company/model. See the [message audit](MESSAGE-DELIVERY-AUDIT.md#provider-limit-and-session-exhaustion-acceptance-september-14). |
| Top | Complete provider input and review handling | Local command review now includes concrete additional permissions and connection context, rejects remote/incomplete requests, and checks the offered one-time decisions; PR #127 merged after final-source review and five successful CI workflows; reviewed `723e794` passed 896 Rust tests, 70 Python checks and 170 native steps, with bounded actual Codex Allow/Deny trials at the recorded candidate. Managed-network approval presentation, grouped-destination scope disclosure and receipt/refusal regressions are now merged in PR #151 after final review and CI, with 61 focused input tests and the full 974-Rust/70-Python release gate passing. Final review fixes at `b605f8e` also passed that gate: optional context cannot inject display controls, and restored command records cannot carry a network presentation. An actual private-profile trial was blocked by the provider allowlist before an approval callback; managed-network provider acceptance remains open. Finish stdin review, broader permission forms, MCP elicitation and secret-input presentation; test further actual-provider interruptions, uncertain writes and sustained conversations. Preserve one queue/order and exact receipts for human and peer input. Managed Claude channels and the owned Codex bridge have bounded idle/busy/question acceptance; hooks alone cannot wake an idle model. See [message audit](MESSAGE-DELIVERY-AUDIT.md), [Codex input](CODEX-INPUT.md) and [Claude question evidence](verification/2026-09-12-claude-question-queue.json). |
| Acceptance | Verify setup and input readiness on the installed candidate | Source now separates configuration, generation-bound MCP/hook contact and fresh input receiver/receipt evidence for each session. Generic activity no longer produces Connected; stale receivers, pauses and old-generation receipts cannot produce verified delivery. Required setup guidance stays in Details. PR #125 merged as `4d00bec` after all final-head CI workflows and source review. Its reviewed `ab8d718` passed 884 Rust tests, 65 Python checks and 165 native steps. An actual Codex 0.154.0 managed conversation passed six FIFO human/peer inputs, matching replies, a dropped read response and an idle heartbeat over 90 seconds with one identity and unchanged configuration. Hook/MCP lifecycle and bounded channel receipt tests also pass at their recorded binaries. Installed-candidate checks remain pending. See [guided setup](GUIDED-SETUP.md) and [desktop contracts](DESKTOP-UX.md). |
| Acceptance | Complete notification release acceptance | AppleScript fallback removal, destination metadata and native routing are implemented. The intact installed `cf64ca3` bundle passes strict signature checks. With explicit user approval, macOS notification permission changed from denied to authorized; unchanged installed binaries then passed real foreground/background message clicks, pending-question navigation, stale-message fallback and another conversation's draft retention. A production-daemon send posted and routed end to end, and a cold private-origin window used the matching installed UI hash while retaining the production window. Owned fixtures were cleaned up. PR #158's diagnostic/first-focus changes are merged after final CI/review. Remaining: zero-process app launch, old-notification and broader question/history/project cases, physical usability and Developer ID/notarized release acceptance. See [notification audit](NOTIFICATION-ROUTING-AUDIT.md). |
| Next | Safe live daemon replacement | (Status as of September 16, 2026.) Child/process-group ownership, exact exit status, PTY/pipe I/O, logs, scrollback and identity now live in a per-agent session owner that outlives the daemon and is reattached on restart (schema 17), merged in PR #130. Final `5120f03` passed real daemon crash/restart, 12-second stopped-owner recovery, 80 controller replacements, release-pin retention and durable disk-exit retirement. A further distinct-source `880e111` → `5120f03` daemon restart passed with batch/PTY children, a 12-second owner outage, unchanged child/owner identities and lease, 40 FIFO messages, ordered logs and exact exit codes; all owned processes retired. The coordinator fence is in source (offer/accept/abort as a compare-and-set on one durable row; mutations answer `transferring`, tick writers skip, reads continue; tested in both race orders). The successor handover is in source behind `AGENTDOCKER_EXPERIMENTAL_RELOAD`: `reload` validates the candidate by `--build-info`, offers, hands the listening socket, daemon lock and container endpoint over `SCM_RIGHTS` to a `--take-over` successor started in its own session, and leaves only on its *serving* answer; every other outcome aborts the offer and keeps this daemon serving. A real-binary test reloads three daemons in a row with batch and PTY agents intact; private trials covered an older-schema, a dying and a silent candidate. The CLI follows a replaced daemon (a `transferring` answer is retried unchanged, `watch`/`events` subscribe again, `attach` attaches again, `daemon reload` waits through `backpressure`), and an installation asks a running daemon to reload to the release it activated and reports the daemon's answer, with `pong` naming the serving pid and executable. The input bindings are fenced with it (binding transitions gate on a committed write; a fenced daemon neither notes a controller's end nor launches one nor takes a pin; `delivery_queue` is refused while fenced, `peek_input` served), and a successor's pending open defers the data migrations that change what rows mean (the v19/v20 offered marks) to its acceptance transaction, so an aborted takeover leaves the rows as the predecessor wrote them. Review fixes now commit native stop records/events before signaling, refuse stops after failed policy clearing, report skipped container transitions, fence history pruning and admit background host work before a handover; six focused failure/transfer regressions and the full `055f45f` gate pass (1,017 Rust tests, seven skipped, 79 Python checks, lint and release packaging). Its clean-source native trial passed 20 handovers, 900 ordered messages, 424 completed launches, preserved batch/PTY identity, logs, lease and question, and controller retry exhaustion; no owned process survived. Ten deferred-exit warnings recovered durably and remain recorded. The reload acceptance trial now carries a bound controller's restart episode: five launches with attempts 1 to 5 across 20 handovers, each launched process noted ended before the next, no daemon warnings ([record](verification/2026-09-16-reload-controller-episode.json)). The controller's installation pin holds across handovers too: a controller bound to the first installed release keeps it through two later generations and a prune, and unbinding lets it go (`desktop_reload_smoke.py --pin-trial`, in the same record). The actual Codex 0.154.0 client with a loopback model fixture also passed four handovers using clean driver `e09db70` and the same release binaries: idle wake, preserved Codex draft, mixed-origin busy input and a pending question with one exact answer receipt; all trial daemons/controllers retired. Remaining: other provider input polls, real model-service and distinct-source handovers, AgentDocker attached-terminal drafts, reconciling uncertain writes without blind replay, and the rest of the acceptance list before the gate is removed. Without the gate `daemon reload` remains unavailable and an installation reports that it keeps the previous daemon serving. Event continuation, provider event reconnect, output drain and read-only queue retry are merged prerequisites, not completed replacement. See [replacement design](LIVE-DAEMON-UPGRADES.md).  Post-review `ad9d075` passed 1,023 Rust tests (seven skipped), 81 Python checks, six focused regressions, 20 private handovers, four actual Codex/loopback handovers and three terminal refusal/exit cases: atomic channel/human/pane transitions, restart recovery after transfer, negotiated handover refusals for legacy clients, honest attach errors and identity-checked successor cleanup.|
| Next | Apply legacy reconciliation to production state | September 14 private-snapshot inventory and corrected previews found three safe historical pairs (575 moved inbox rows, 139 duplicate message copies); distinct live-Claude session IDs remain separate. PR #126 merged as `3dc6ee7` after all final-head CI workflows and source review. Reviewed `746ec69` passed 879 Rust tests, 67 Python checks and seven actual repair/restart steps. Source permits only the known hook/MCP provenance difference while archiving both original records. Apply verified pairs with fresh plans and backup/recovery after the daemon and required sessions can safely finish. Do not merge by display name or stop a live process to tidy the list. See [identity repair](IDENTITY-REPAIR.md). |
| Engineering | Storage maintenance delivered; sustained acceptance remains | `[journal] retention` in `agentd.toml` applied by the minute tick in bounded batches, `journal prune --before <seq\|duration>`, `checkpoints prune --older-than` (live authors, acceptors and handoff addressees protected; rows and event in one transaction) and `daemon vacuum` (refused while sessions are live unless forced) are implemented with failure-injection, clock-skew and live-daemon tests. Journal heads survive pruning so sequences and cursors stay monotonic. A [20-minute retention trial](verification/2026-09-15-retention-sustained-use.json) on release binaries (`scripts/retention_sustained.py`: ten registered agents, `[journal] retention = "120s"`, half the population leaving at half time, checkpoints of finished agents pruned every minute) passed twice, the second time with every claimed assertion (per-sample oldest age, eligible checkpoints by id, per-reader ordering, hashed private daemon copy): retention pruned 18 times by itself with the oldest entry at most 179 s old at any sample against the 120 s window, ten readers with pre-prune cursors read 500 pages in order, all 420 eligible finished checkpoints were removed by id and all 845 live ones kept, and the database and daemon memory stayed bounded (1.01x and 1.00x of the middle third) from the tenth minute on under a load that halves at half time, 12,680 cycles, no daemon warnings. What remains is longer than 20 minutes, sleep/reboot, and provider sessions rather than registered records. See [architecture](ARCHITECTURE.md#the-journal). |
| Acceptance | Sustained use and unresolved failures | Real coordinated use on September 15 showed one branch switch producing hundreds of stale and contested-channel notices, one per change per path; the daemon now sends one stale notice per reader and one widening notice per channel on each second's tick, and holds further stale paths while a reader's last notice is still queued (see [architecture](ARCHITECTURE.md#projects)). Complete sleep/reboot and provider-session workloads. A [one-hour 100-agent/10,000-file daemon trial](verification/2026-09-11-hour-sustained-use.json) passed 1,392,836 cycles, a [7.5-hour overnight run](verification/2026-09-14-overnight-sustained-use.json) of 1, 10 and 100 agents (2.5 hours each, 10,000 files) passed 35,050 / 350,118 / 3,470,242 cycles with the daemon under 33 MiB and, over the last 100,000 requests of each population, p99 request latency under 10 ms on a host also running provider sessions and build gates (that binary predates session owners and provider availability), and a [20-minute retention trial](verification/2026-09-15-retention-sustained-use.json) covers journal retention, checkpoint pruning and growth bounds with agents leaving mid-run; none covers sleep/wake, reboot or provider sessions. Diagnose the retained incomplete Iced capture, historical socket timeout and September 15 Linux ARM transport refusal. Bounded inspector diagnostics now retain future failures; the earlier helper discarded the observation, so a later passing trial cannot explain it. See [testing standard](TESTING-AND-BENCHMARKS.md) and [local trial](LOCAL-TRIAL.md). |
| Release | Publish verified downloads and updates | Consumer, opt-in daily scheduler and release archive/feed automation exist. Align the cask template with the stable signing and managed-installation contract. `install.sh` installs the desktop through the app's own installer by default on macOS and refuses to write over a managed installation on its commands route; the cask links the app's own commands, conflicts with the formula and stops the login service on uninstall, so every route is one installation. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and actual update/rollback acceptance. Cargo registry publication is not an established supported route. See [distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md) and [setup](DISTRIBUTION-SETUP.md). |
| Platform | Linux and independent Mac acceptance | Complete target-distribution desktop/service/package trials, a second Mac and physical Intel acceptance. ARM64/x86-64 graphical/package CI and Rosetta execution do not establish those hardware results. See [local trial](LOCAL-TRIAL.md). |
| Platform | Full native Windows product | Integrate daemon/clients with named pipes; finish supervision, ConPTY, identity-safe stop/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and native graphical CI. Existing core/host/desktop adapters are foundations. See [Windows port](WINDOWS-PORT.md). |
| Input | Hands-on accessibility and input methods | Exercise VoiceOver and supported-platform screen readers, keyboard navigation/activation, visible focus, zoom, IME, Unicode and terminal copy/paste. Range selection has [actual macOS acceptance](verification/2026-09-11-terminal-selection.json); broader human trials remain. Repair observed defects. See [Iced contracts](ICED-DESIGN.md). |
| Done (bounded) | Removed-checkout watcher recovery | Source ignores vanished checkout roots and reports lost coverage; independent macOS checkout streams fix a reproduced surviving-file event loss. Both regressions passed 100 repetitions, and the installed production daemon (3c8c2e1, 2026-09-15) recovered a removed secondary checkout in 0.31 s with no false contest and all four live provider sessions unchanged; a failed warm-up attempt is retained beside it. Historical conflict channels remain. Still open: overnight and many-project resource acceptance of one FSEvents stream per checkout. See [watcher evidence](verification/2026-09-11-macos-watcher-recovery.json). |

| Later | Existing optional proposals | Federation/host namespaces and cross-host leases/routing are unbuilt. Additional adapters, container log following and proposed CLI conveniences remain deferred behind the single-host desktop. They are existing scope, not prerequisites invented by this audit. See [product direction](PRODUCT-DIRECTION.md), [architecture proposals](ARCHITECTURE.md#planned-protocol-and-event-additions) and [containers](CONTAINER-ENGINES.md). |

## Requested September 15

| Priority | Work remaining | Completion condition and evidence |
| --- | --- | --- |
| Partial | Messaging as a workspace (Slack/Discord shape) | PR #150 merged as `93b76b8` after final review and CI: schema21 archives, destination-scoped conversations, named channels, read cursors, unread counts, search, threads and the Messages workspace are implemented. Reading acknowledges only the person's displayed rows; agent input queues remain separate. The reviewed integration passed 999 Rust tests, 77 Python checks and 233 native workflow steps, with retained archive/index regression evidence in [session messages](verification/2026-09-11-session-messages.json). PR #152 adds the Earlier group, persistent project menus, saved names and corrected discovery; final source `2a51d13` passed 1007 Rust tests, 77 Python checks and 275 native steps. It merged as `ece76fd` after final CI/review. PR #160 pane resizing and hidden-conversation read-state protection merged as `37725fe` after final review and CI; widths remain saved, compact layouts preserve usable conversation space, and hidden panes do not acknowledge messages. Read-only September 16 verification observes source `1e90f83` installed as release `3add4bea`, with the daemon and GUI running from that release. PR #163 merged as `ad96698`: collision rooms retain their row counts but no longer inflate the person's unread badge or Mark all read. Broader installed acceptance remains. Still unbuilt: mentions/counts and an explicit project-wide pause request/control. See [desktop behavior](DESKTOP-UX.md#messages-inbox-and-tools) and [integrated evidence](verification/2026-09-12-integrated-desktop.json). |
| Acceptance | Portable coordination skill | Source implementation and review are complete in merged PR #149; the bundled `SKILL.md` supplies MCP instructions and preview/apply/undo setup without duplicated instruction text. Bounded Codex/Claude loader and Claude setup/undo trials passed. Fresh-session implicit activation, installed-candidate checks and other runtime loaders remain open, as detailed in the portable coordination instructions row above and [guided setup](GUIDED-SETUP.md#shared-coordination-skill). |
| Engineering | Token usage by agent, model and provider | PR #165 accounting/parser foundation merged as `f6f2f96`, with ten focused tests and the full 1,026-Rust/77-Python gate (seven skipped). PR #167 adds tested bounded file reading. Directory discovery, growing-file prefix validation, atomic ingestion/retention, historical attribution, protocol, CLI, Usage screen and separate emitted-byte overhead remain open under the [existing architecture contract](ARCHITECTURE.md#planned-protocol-and-event-additions). Missing coverage remains unknown, not zero; totals show tokens, not money. |

## Manual and operational steps

| Step | What remains |
| --- | --- |
| Apple signing/notarization | Supply a Developer ID Application identity and private notary profile; run signing, notarization, stapling and Gatekeeper checks on the final app/DMG before publication. Packaging automation exists; ad-hoc signing only verifies a local preview. The read-only September 15 identity check again found zero valid signing identities. |
| Human accessibility/IME trials | Run the input trials above and record findings on the actual candidate. Automated control/accessibility checks do not replace them. |
| Completed on this Mac: launcher and coordinator switch | The backed-up `cf64ca3` (schema20) switch passed; Claude subsequently installed `72b1eb4` (schema21), now observed serving. All four external provider identities, PIDs and birth times were preserved; strict launcher signature checks and legacy hook paths pass. This closes the old-launcher switch, not the experimental live-transfer or future-upgrade acceptance above. |
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

Session reconnect review follow-up: failed observation reads now latch the daemon storage error, resume queries bind identity strings, and the atomic fold removes accepted empty observations belonging to retired identities. The regressions cover malformed stored observations, embedded NUL/quote identity strings, and absence of retired empty observation rows after reopen. Integrated source `2ed9f0b` passed both focused regressions and the full 1,102-Rust/84-Python gate (seven skipped), formatting, strict lint, doctests, packaging and release. Its actual MCP adapter repeat passed in 38.92 seconds with no surviving fixture processes; that validates transport and queue behavior, not a Claude model idle wake. Final review, CI and installed acceptance remain open.

Receiver-upgrade review follow-up: the hidden CLI accepts the standard
`AGENTDOCKER_AGENT_ID` default, and the architecture request table now spells out
`upgrade_controller`, its binding response and commit-before-stop semantics.
The full source gate passed for these two review corrections at `8d42db5`; final review, CI and installed-session acceptance remain open.
