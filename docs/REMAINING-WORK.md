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
| Top | Existing idle sessions and provider parity | The user reconfirmed this open bug on September 17: the current plain Claude session has hooks/MCP without a channel input binding and recent handoffs can remain unconsumed at its idle prompt. Saved changes, process presence and MCP registration do not prove delivery. Install the reviewed #164 reconnect support, enable the supported channel route by a safe same-session relaunch, preserve original queued IDs/receipts and drafts, and prove an actual peer-only idle turn with a correlated model reply and no new human prompt. Verify reconnect/retry preserves order and prevents duplicate execution. New Claude/Codex UI launches have input defaults, but consent and actual session readiness still matter. Other supported runtimes need equivalent adapters and idle/busy/limit acceptance. [Message audit](MESSAGE-DELIVERY-AUDIT.md). |
| Top | Project pause and per-recipient delivery | The missed CLI/global and app/project pauses are reproduced and retained. Installed Codex bounded active input now passes, including real project fan-out; queue acceptance alone still does not prove every recipient paused. PR #173 implements durable project pause/resume. Its reviewed follow-up binds forms and replies to project/request identities, preserves reasons on queue/transport refusal, bounds draft/command storage and places form actions on a separate row. Runtime `60380fd` passed 1,125 Rust tests (seven skipped), 84 Python checks and the full gate; the final isolated native workflow passed 429 rendered steps/26 outer checks, including 720×540 pause composition. Final review/CI and installed acceptance remain open. The actual schema23-to22 rollback-refusal trial passed and is folded into the existing integrated desktop record; lifting a pause does not downgrade the database. Actual idle/busy recipient consumption remains open. |
| Top | Native input lifecycle and throughput | Complete zero-prompt startup/reopen, oversized input, burst latency, prolonged busy/approval waits, sleep/reboot, uncertain writes and replacement across actual provider versions. The installed existing-session idle wake and bounded active-input cases pass; one outstanding offer and provider polling cadence still bound throughput. [Codex input](CODEX-INPUT.md). |
| Top | Provider-limit detection and recovery | Common tests cover all catalog runtimes, custom runtimes and nine normalized interruption classes; bounded actual Claude/Codex and mid-tool 429 recovery pass. Actual account resets, additional provider versions/adapters, unrelated replacement identities and sustained use remain. Unsupported adapters do not acquire inferred signals; unknown quota scope/reset stays unknown. [Limit acceptance](MESSAGE-DELIVERY-AUDIT.md#provider-limit-and-session-exhaustion-acceptance-september-14). |
| Top | Provider review and input handling | Local command review and managed-network presentation are merged. The actual network trial stopped at the provider allowlist before a callback. Finish stdin review, broader permissions, MCP elicitation and secret input; retain exact human/peer queue ordering through interruption and uncertain writes. [Input contracts](CODEX-INPUT.md). |
| Engineering / acceptance | Setup and input readiness | Configuration, generation-bound contact and fresh receiver/receipt evidence are distinct. PR #168 merged as `9dac645` after final-head review and all CI checks. Named missing-hook diagnostics reject malformed `disableAllHooks` values as Unverified; the guide includes StopFailure. Source `1cdec50` passed 1,118 Rust tests (seven skipped), 84 Python checks and the full release gate. Installation and actual-session setup acceptance remain open. [Guided setup](GUIDED-SETUP.md). |
| Acceptance | Notification release cases | Approved installed foreground/background message clicks, pending question, stale destination, another conversation's draft and daemon posting passed. Complete zero-process app launch, old notifications, broader question/history/project cases and physical usability on the signed final package. [Notification audit](NOTIFICATION-ROUTING-AUDIT.md). |
| Acceptance | Safe live daemon replacement | The source is merged; `AGENTDOCKER_EXPERIMENTAL_RELOAD` remains required. Complete real model-service and distinct-source provider handovers, Claude/other provider polling, attached-terminal drafts, output drain and uncertainty recovery under sustained use before removing the gate. Successful explicit coordinator restarts do not close this work. [Replacement contract](LIVE-DAEMON-UPGRADES.md). |
| Operational | Legacy production duplicate reconciliation | Offline preview/apply and rollback are implemented in merged #126. Earlier private previews found three safe historical pairs (575 inbox rows, 139 duplicate copies). Recompute plans from a fresh backup only after all required nonhuman records have ended and the daemon is stopped. Never merge by display name or stop live providers to tidy the list. Current same-session resumption is separate. [Identity repair](IDENTITY-REPAIR.md). |
| Acceptance | Sustained storage and real-provider use | A 20-minute retention trial passed with ten registered agents, ordered readers, eligible checkpoint removal and bounded storage/memory; it did not use actual provider conversations. A 7.5-hour fixture run predates current ownership/availability. Complete longer current-candidate, many-project, real-provider, sleep/wake and reboot trials. Retain the historical incomplete Iced capture, benchmark socket timeout and unexplained Linux ARM transport refusal. September 17 PR #172 Linux x86 CI separately captured an lsof mount-stat warning: transport verification refused. PR #174 adds a bounded Linux socket observer, but its graphical CI still refuses an unclassified or changing socket and requires investigation before merge. [Testing standard](TESTING-AND-BENCHMARKS.md), [retention evidence](verification/2026-09-15-retention-sustained-use.json). |
| Acceptance | Watcher resource limits | Removed-checkout recovery is complete within its tested scope: two regressions passed 100 repetitions and installed `3c8c2e1` recovered a removed checkout in 0.31 s with four providers unchanged. Overnight/many-project acceptance of one FSEvents stream per checkout remains. [Watcher evidence](verification/2026-09-11-macos-watcher-recovery.json). |
| Release | Publish verified downloads and updates | Packaging, update consumer, scheduler and archive/feed automation exist. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and actual update/rollback acceptance. A generated cask is not a published route. [Distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md). |
| Platform | Linux and independent Mac acceptance | Complete target-distribution desktop/service/package trials, a second Mac and physical Intel testing. ARM64/x86-64 graphical/package CI and Rosetta are useful evidence but do not establish independent hardware acceptance. [Local trial](LOCAL-TRIAL.md). |
| Platform | Full native Windows product | Named-pipe foundations exist. Finish daemon/client integration, supervision, ConPTY, identity-safe stop/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and actual native graphical/provider acceptance. [Windows port](WINDOWS-PORT.md). |
| Input | Accessibility, physical keyboard and input methods | Exercise VoiceOver/supported screen readers, focus, keyboard activation, zoom, IME, Unicode and broader terminal copy/paste. Automated submit actions do not establish physical Enter. The current installed test opened the new-message form, then macOS reported the screen locked; no physical Enter event was sent. Resume that test when unlocked. Multiline/Shift-Enter composition is not implemented. [Iced contracts](ICED-DESIGN.md). |

## Requested September 15

| Status | Existing request | Remaining scope |
| --- | --- | --- |
| Delivered with acceptance remaining | Messaging as a workspace (Slack/Discord shape) | Archive/search/threads/read state, project navigation, resizable panes, mentions, New DM/channel and invitations are merged and installed. Finish the physical input checks and #173 project pause above. [Desktop behavior](DESKTOP-UX.md#messages-inbox-and-tools). |
| Acceptance | Portable coordination skill | PR #149's single bundled SKILL.md, export and preview/apply/undo are implemented and included in the installed source. Bounded Codex/Claude loader and Claude setup/undo trials passed. Fresh-session implicit activation, current installed-candidate checks and other runtime loaders remain. [Shared coordination skill](GUIDED-SETUP.md#shared-coordination-skill). |
| Engineering | Token usage by agent, model and provider | PR #165 accounting/parser foundation is merged. PR #167's bounded file reader passed the combined 1,116-Rust/84-Python gate (seven skipped); final review/CI remain pending. Directory discovery, growing-file prefix validation, atomic ingestion/retention, historical attribution, protocol, CLI, Usage screen and separate emitted-byte overhead remain unbuilt. Missing coverage stays unknown; totals are tokens, not money. [Existing architecture contract](ARCHITECTURE.md#planned-protocol-and-event-additions). |
| Engineering | A board of work beside the sessions | Daemon side in source: cards as `task` documents with a title, acceptance text and a column; `task_pull` takes a Ready card once under the state lock and holds it as a `task:<id>` lease in the same commit (the second taker is told who holds it; a pull of one's own held card renews; a card whose holder's lease lapsed is refused with `hold: lapsed` and taken over only by naming that holder in `take_over_from`, `from` in the event; a paused project refuses the pull), an agent's `task_move`/`task_update`/`task_archive` require a live hold, the person's moves back and hands end and take leases in the one commit, a shared claim by hand is no hold (`a_lapsed_hold_is_recovered_by_name_and_the_persons_hands_move_the_lease`), a store that fails mid-transaction or a coordinator fence leaves no card change, lease or event behind (`a_pull_the_store_fails_midway_or_a_fence_refuses_leaves_nothing_behind`), `tasks` as a bounded page (`offset`, `limit`, a byte budget, `more`) and a prefix lookup that reads two rows, a failed store answered `storage_unavailable` (`the_board_is_paged_and_a_failed_store_is_not_an_empty_board`); five events; CLI `agentdocker task create|pull|move|update|archive|list`; MCP `list_tasks`, `pull_task`, `move_task`, `create_task`; the bundled skill tells agents to pull Ready cards and read the acceptance text first (`a_card_is_pulled_once_and_moved_by_its_holder_or_the_person`, core rules unit-tested). The shape follows the Paprika research in the [landscape note](LANDSCAPE-2026-09-11.md). The Board tab is in the app: five columns, filing a card as Ready or into Backlog, a card opened to its acceptance text and moved a column at a time, handed to an agent or released, archived; the holder shown with presence and *hold lapsed* when its lease is gone; the board read again on every board or lease event and kept as last read when a read fails; drafts per project, answered by their own reply and told when the queue refuses them (`a_card_draft_survives_other_board_actions_late_replies_and_refused_queues`); a card's accessible name says who holds it. Source `6e1f62b` passed 1,122 Rust tests (seven skipped) and the full formatting/lint/doctest/installer/packaging/release gate. The narrow workflow smoke files a card, pulls a pre-filed one as an agent (the second pull is refused), moves it to Review and Done and archives it, checking the daemon afterwards; its steps are in source but the two runs so far ended at the window's 150-second deadline inside the earlier, unchanged scenario while the display server was saturated, so a passing run is still owed. Still ahead: a pull refused while the project is paused (after #173), typed links, roles, webhooks. |

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
