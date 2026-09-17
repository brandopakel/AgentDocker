# Remaining engineering and release work

Current status: September 17, 2026 UTC. This is the single backlog for the
requirements already in the project docs. It replaces duplicated progress notes;
their source pins, failed trials and acceptance limits remain in the existing
[verification records](verification/), [delivery plan](DELIVERY-PLAN.md) and Git
history. The [documentation index](README.md) classifies all 38 Markdown files.
Reference documents and historical audits do not need to be marked as unfinished
features. The root README's duplicate roadmap has already been removed.

The project is **not fully complete**. The current Mac has the reviewed messaging
controls and repaired Codex active-input delivery. Other-provider idle delivery,
project pause, usage collection, release/platform work and hands-on acceptance
remain below. Optional research is outside the current delivery closure.

## Delivered source and current desktop

The installed desktop and daemon use source `5c461e0`, schema22, release
`79981beb`, activated with a state backup on September 17 at 03:58 UTC. PR #170
merged as `7df165e` after final-head review and all CI checks. All four external
provider identities and processes survived. The Codex receiver changed from
20912 to 29976; its binding/token and all 97 retained receipts survived. The GUI
was restarted: visible editable fields were empty, but this does not establish
persistence of hidden drafts. The previous `9bc0f0fc` release remains retained.
See [installed messaging evidence](verification/2026-09-12-integrated-desktop.json).

| Delivered behavior | Evidence and limits |
| --- | --- |
| Simpler home, projects, Messages and Tools | PRs #119/#129/#152/#160/#163 are merged: compact navigation, saved project names, resizable panes, readable session names, hidden-pane read protection, and collision notices excluded from the person's unread total. |
| New DM/channel, invitations, mentions and submit | PR #170 is merged and installed. **+** opens creation, channel members can be invited, suggestions use the selected conversation's live recipients, Unicode names work, and saved retired names retain mention counts. Enter is wired to submit in every single-line composer. Final source `5c461e0` passed 1,108 Rust tests (seven skipped), 84 Python checks and the full lint/doctest/package/release gate. The preceding runtime passed 415 rendered steps/26 outer checks; final graphical CI passed. Actual installed New DM/channel controls opened successfully. Physical Enter/IME and multiline input remain below. |
| Durable messaging workspace | PR #150 archives conversations, channels, threads, search and reader cursors. Human read markers acknowledge displayed rows; they do not acknowledge an agent's provider-input queue. Creation/invitation membership, events and notices commit atomically. |
| Codex input priority and safe receiver replacement | PRs #162/#169/#171 are merged. Human and peer input share the provider route. Actual installed CLI human-route, Claude peer and project fan-out messages entered this same live Codex turn without another prompt or manual queue acknowledgement. One CLI sample reached provider context in 0.176 s; this is a sample, not a latency guarantee. Active hook context is limited to 6,000 bytes and tool boundaries. [Exact receipts and limits](verification/2026-09-15-native-codex-queue.json). |
| Reconnect implementation | PR #164 merged as `ce0ff1d` after final-head review and all CI checks. Eligible ended records of the same provider session fold transactionally, preserving durable queue order, names and ancillary state; unsafe/live cases refuse. Combined source `3c41b3b` passed 1,116 Rust tests (seven skipped), 84 Python checks and an actual MCP adapter resume fixture (38.41 s, no survivors). Installation and real Claude idle-resume acceptance remain open; this fixture is not a live model. |
| Process ownership and experimental reload | PRs #130/#155/#161 are merged. Session owners, transfer fencing, successor readiness, retry/reconciliation and input-binding preservation have bounded process/client trials. PR #155 merged as `fec093c`; final integration `f4ef6c3` passed 1,087 Rust tests (seven skipped), 84 Python checks and the full release gate. Source-specific pressured and native-client handovers remain in [reload evidence](verification/2026-09-16-reload-controller-episode.json). The experimental gate stays on. |
| Compatible launcher and notifications | The `unknown argument: hook` collision is repaired and the user confirmed Claude prompts work. Applications installation and old hook paths are compatible. AppleScript fallback removal and native destination routing are implemented; approved installed Notification Center clicks have bounded passing evidence. |
| Provider-limit framework and maintenance | Durable availability, queue gating, exact recovery and quota isolation cover the common interruption contract. Bounded journal/checkpoint retention and pure-core environment cleanup are implemented. Broader detection, actual account recovery and sustained acceptance remain open. |

`make install` from the repository root updates the app and CLI. It asks the
running daemon to reload; with experimental reload disabled, that daemon keeps
serving until an explicit safe restart. Use `agentdocker desktop status` and
`agentdocker daemon status` for the actual installed and serving versions. The
current installed app is already updated; no user installation is needed for #170.

## Engineering and acceptance still open

| Priority | Work remaining | Completion condition and evidence |
| --- | --- | --- |
| Top | Existing idle sessions and provider parity | The user reconfirmed this open bug on September 17: the current plain Claude session has hooks/MCP without a channel input binding and recent handoffs can remain unconsumed at its idle prompt. Saved changes, process presence and MCP registration do not prove delivery. The September 17 preflight also found that the saved entry omitted `--claude-channel`, while setup/health rejected that documented argument form. PR #177 generates and recognizes it for new registrations; legacy entries need a reviewed configuration update, and provider startup consent is still required. Install the reviewed #164 reconnect support, enable the supported channel route by a safe same-session relaunch, preserve original queued IDs/receipts and drafts, and prove an actual peer-only idle turn with a correlated model reply and no new human prompt. Verify reconnect/retry preserves order and prevents duplicate execution. New Claude/Codex UI launches have input defaults, but consent and actual session readiness still matter. Other supported runtimes need equivalent adapters and idle/busy/limit acceptance. [Message audit](MESSAGE-DELIVERY-AUDIT.md). |
| Top | Project pause and per-recipient delivery | The missed CLI/global and app/project pauses are reproduced and retained. Installed Codex bounded active input now passes, including real project fan-out; queue acceptance alone still does not prove every recipient paused. PR #173 merged as `d0cd2d7` after final-head review and all CI checks; it implements durable project pause/resume. Its reviewed follow-up binds forms and replies to project/request identities, preserves reasons on queue/transport refusal, bounds draft/command storage and places form actions on a separate row. Runtime `60380fd` passed 1,125 Rust tests (seven skipped), 84 Python checks and the full gate; the final isolated native workflow passed 429 rendered steps/26 outer checks, including 720×540 pause composition. Installed acceptance remains open. The actual schema23-to22 rollback-refusal trial passed and is folded into the existing integrated desktop record; lifting a pause does not downgrade the database. Actual idle/busy recipient consumption remains open. |
| Top | Native input lifecycle and throughput | Conversation, thread and session drafts are currently window memory, not saved workspace state; do not quit a window with unaccounted unfinished text during activation. Complete draft persistence, zero-prompt startup/reopen, oversized input, burst latency, prolonged busy/approval waits, sleep/reboot, uncertain writes and replacement across actual provider versions. The installed existing-session idle wake and bounded active-input cases pass; one outstanding offer and provider polling cadence still bound throughput. [Codex input](CODEX-INPUT.md). |
| Top | Provider-limit detection and recovery | Common tests cover all catalog runtimes, custom runtimes and nine normalized interruption classes; bounded actual Claude/Codex and mid-tool 429 recovery pass. Actual account resets, additional provider versions/adapters, unrelated replacement identities and sustained use remain. Unsupported adapters do not acquire inferred signals; unknown quota scope/reset stays unknown. [Limit acceptance](MESSAGE-DELIVERY-AUDIT.md#provider-limit-and-session-exhaustion-acceptance-september-14). |
| Top | Provider review and input handling | Local command review and managed-network presentation are merged. The actual network trial stopped at the provider allowlist before a callback. Finish stdin review, broader permissions, MCP elicitation and secret input; retain exact human/peer queue ordering through interruption and uncertain writes. [Input contracts](CODEX-INPUT.md). |
| Engineering / acceptance | Setup and input readiness | Configuration, generation-bound contact and fresh receiver/receipt evidence are distinct. PR #168 merged as `9dac645` after final-head review and all CI checks. Named missing-hook diagnostics reject malformed `disableAllHooks` values as Unverified; the guide includes StopFailure. Source `1cdec50` passed 1,118 Rust tests (seven skipped), 84 Python checks and the full release gate. PR #177 corrects channel-capable setup and inventory. Combined `57c2612` passed 1,144 Rust tests (seven skipped), 94 Python checks (one Linux-only skip), the full release gate, 39 focused setup tests and private-profile setup/undo with actual Claude Code 2.1.274. The real MCP resume fixture passed ten cases in 38.82 seconds with no survivors; it is not a live model. Candidate `28f4f1f3` passed installation preview; final review/CI, activation and actual-session setup acceptance remain open. [Guided setup](GUIDED-SETUP.md). |
| Acceptance | Notification release cases | Approved installed foreground/background message clicks, pending question, stale destination, another conversation's draft and daemon posting passed. Complete zero-process app launch, old notifications, broader question/history/project cases and physical usability on the signed final package. [Notification audit](NOTIFICATION-ROUTING-AUDIT.md). PR #175 implements bounded notification-message reveal (five earlier pages, one request in flight, 5,000 retained rows), highlights the target and cancels stale searches on navigation, disconnection or pruning. Source `ae93e55` passed the full 1,120-Rust gate (seven skipped) and all final CI checks; the merge with project pause is being rechecked. The review follow-up waits for fresh history before using a cached complete page, rejects pre-click responses, reports initial/earlier-page queue refusal and leaves live questions out of archive searches. These regressions and the combined build are being validated before activation. Actual installed clicks remain open. |
| Acceptance | Safe live daemon replacement | The source is merged; `AGENTDOCKER_EXPERIMENTAL_RELOAD` remains required. Complete real model-service and distinct-source provider handovers, Claude/other provider polling, attached-terminal drafts, output drain and uncertainty recovery under sustained use before removing the gate. Successful explicit coordinator restarts do not close this work. [Replacement contract](LIVE-DAEMON-UPGRADES.md). |
| Operational | Legacy production duplicate reconciliation | Offline preview/apply and rollback are implemented in merged #126. Earlier private previews found three safe historical pairs (575 inbox rows, 139 duplicate copies). Recompute plans from a fresh backup only after all required nonhuman records have ended and the daemon is stopped. Never merge by display name or stop live providers to tidy the list. Current same-session resumption is separate. [Identity repair](IDENTITY-REPAIR.md). |
| Acceptance | Sustained storage and real-provider use | A 20-minute retention trial passed with ten registered agents, ordered readers, eligible checkpoint removal and bounded storage/memory; it did not use actual provider conversations. A 7.5-hour fixture run predates current ownership/availability. Complete longer current-candidate, many-project, real-provider, sleep/wake and reboot trials. Retain the historical incomplete Iced capture, benchmark socket timeout and unexplained Linux ARM transport refusal. September 17 PR #172 Linux x86 CI separately captured an lsof mount-stat warning: transport verification refused. PR #174 adds a bounded Linux socket observer. Its first graphical run refused changing/unclassified sockets; private concurrent-RPC trials reproduced the issue and the bracketed-sample follow-up passed 100 samples. All final checks on `5c3eccc` passed, including Linux ARM/x86 and macOS graphical jobs; the integration with merged project pause and the report-bound/TCP6-evidence corrections are being rechecked (94 Python checks, one Linux-only skip). The previous refusals remain evidence. [Testing standard](TESTING-AND-BENCHMARKS.md), [retention evidence](verification/2026-09-15-retention-sustained-use.json). |
| Acceptance | Watcher resource limits | Removed-checkout recovery is complete within its tested scope: two regressions passed 100 repetitions and installed `3c8c2e1` recovered a removed checkout in 0.31 s with four providers unchanged. Overnight/many-project acceptance of one FSEvents stream per checkout remains. [Watcher evidence](verification/2026-09-11-macos-watcher-recovery.json). |
| Release | Publish verified downloads and updates | Packaging, update consumer, scheduler and archive/feed automation exist. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and actual update/rollback acceptance. A generated cask is not a published route. [Distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md). |
| Platform | Linux and independent Mac acceptance | Complete target-distribution desktop/service/package trials, a second Mac and physical Intel testing. ARM64/x86-64 graphical/package CI and Rosetta are useful evidence but do not establish independent hardware acceptance. [Local trial](LOCAL-TRIAL.md). |
| Platform | Full native Windows product | Named-pipe foundations exist. Finish daemon/client integration, supervision, ConPTY, identity-safe stop/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and actual native graphical/provider acceptance. [Windows port](WINDOWS-PORT.md). |
| Input | Accessibility, physical keyboard and input methods | Exercise VoiceOver/supported screen readers, focus, keyboard activation, zoom, IME, Unicode and broader terminal copy/paste. Automated submit actions do not establish physical Enter. On September 17, the installed `79981beb` app accepted targeted synthetic Return key events in its project composer: one message was archived, the composer cleared and Codex received the original ID with a completed receipt. This verifies the native key-event path; human keyboard and IME composition remain untested. Multiline/Shift-Enter composition is not implemented. [Iced contracts](ICED-DESIGN.md). |

## Requested September 15

| Status | Existing request | Remaining scope |
| --- | --- | --- |
| Delivered with acceptance remaining | Messaging as a workspace (Slack/Discord shape) | Archive/search/threads/read state, project navigation, resizable panes, mentions, New DM/channel and invitations are merged and installed. Finish the physical input checks and #173 project pause above. [Desktop behavior](DESKTOP-UX.md#messages-inbox-and-tools). |
| Acceptance | Portable coordination skill | PR #149's single bundled SKILL.md, export and preview/apply/undo are implemented and included in the installed source. Bounded Codex/Claude loader and Claude setup/undo trials passed. Fresh-session implicit activation, current installed-candidate checks and other runtime loaders remain. [Shared coordination skill](GUIDED-SETUP.md#shared-coordination-skill). |
| Engineering | Token usage by agent, model and provider | PR #165 accounting/parser foundation is merged. PR #167's bounded file reader passed combined source `632d0f1`: 1,126 Rust tests (seven skipped), 92 Python checks (one Linux-only check skipped locally and exercised in the Linux VM), lint/doctest/package/release. This combined proof includes the separately reviewed Linux observer. Windows CI then exposed a same-length/restored-mtime rewrite missed by metadata (job `105082608981`); runtime `d643275` now passes ten reader regressions and strict lint; Windows head `63ea3ca` passed all 238 tests (two skipped), including that original failure. Follow-up `344516e` adds bounded quarantine recovery and validates quarantined prefixes before retry refusal; all 13 reader regressions and the combined `be3086d` gate passed (1,142 Rust tests, seven skipped; 92 Python checks, one Linux-only skip). The earlier quarantine fixture budget failure on Linux/Windows is retained in the integrated evidence. Final source review is clean; platform CI remains pending. Prefixes beyond the 16 MiB validation cap remain incomplete. Directory discovery, larger/growing-file prefix validation, atomic ingestion/retention, historical attribution, protocol, CLI, Usage screen and separate emitted-byte overhead remain unbuilt. Missing coverage stays unknown; totals are tokens, not money. [Existing architecture contract](ARCHITECTURE.md#planned-protocol-and-event-additions). |
| Optional proposal | A board of work beside the sessions (from the Paprika research) | Research only; not part of the current delivery closure. Not started. [Paprika](LANDSCAPE-2026-09-11.md#paprika-added-17-september-2026) shows the shape: a card with acceptance text and an atomic pull, roles as owner hints, typed links (PR, path, memory for the next agent), signed webhooks on the event stream, rules that only comment or move, a `--json`/exit-code contract for the CLI. The first step here is a `task` card over the existing `task:<name>` lease (title, what done means, column, assignee; `task pull` claims or refuses) and a Board tab per project in the app; then an optional bridge that takes the lease when an agent pulls a Paprika card. Done when the person can file work in the app, an agent can claim it while a valid exclusive lease excludes other local claimants, long-running work renews that TTL, and expired claims stop implying ownership. The bridge must reconcile card assignment/state with the local lease before work or retry, expose stale cards, require confirmed reassignment or explicit recovery plus a fresh lease before taking over, and release claims for closed/reassigned cards. The journal must record those transitions. |

Federation/host namespaces, cross-host leases/routing, the herdr focus bridge,
additional adapters, container log following and proposed CLI conveniences are
existing deferred proposals. They are not prerequisites for current single-host
delivery. See [product direction](PRODUCT-DIRECTION.md),
[architecture proposals](ARCHITECTURE.md#planned-protocol-and-event-additions)
and [containers](CONTAINER-ENGINES.md).

## Manual and operational steps

| Step | What remains |
| --- | --- |
| Apple signing/notarization | Supply a Developer ID Application identity and private notary profile, then sign, notarize, staple and run Gatekeeper checks on the final app/DMG. The September 15 read-only check found no valid signing identity. Ad-hoc signing validates a local preview only. |
| Human accessibility/IME trials | Run the hands-on input cases above on the actual candidate and record findings. Automated accessibility controls do not replace them. |
| Completed on this Mac: launcher and coordinator switch | The old launcher problem is closed. Current `79981beb`/schema22 desktop, daemon and receiver were activated from a verified package with backup and all four external provider processes retained. This does not certify future unattended live transfers. |
| Independent release acceptance | Run second-Mac, physical Intel, target-Linux and sustained actual-provider trials against the final candidate. |

Signing/private credential handling is in [desktop distribution](DESKTOP-DISTRIBUTION.md);
the operational sequence is in [local trial](LOCAL-TRIAL.md).

## Retained evidence and release configuration

Existing verification reports preserve original sources and failed attempts.
Later merges close their old integration notes, not every acceptance category.
The [documentation index](README.md#verification-records) lists all retained
reports; the [testing crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk)
identifies complete and partial categories.

PR #128's terminal timeout/partial-UTF-8 fix passed 100 repetitions of 14 tests;
the original macOS writer failure and separate benchmark socket timeout remain
unexplained. A five-minute retention/restart trial at `66c0946` preserved 331
mixed queued inputs through five prune batches and restart after 1,532 journal
notes. Longer retention and overnight fixtures keep their separate source pins;
none establishes current-provider sleep/reboot acceptance.

The September 14 read-only release check found v0.1.0 as the latest published
release and only a README under the tap's Casks directory. The September 9 check
found a publishing variable and token secret name; it did not inspect secret
values or establish token validity. Publication must be verified when performed.

Current active Codex/Claude exchanges have exact queue/context receipts in the
existing native queue record. They establish active delivery, not idle wake for
a plain Claude session or universal-provider completion.
