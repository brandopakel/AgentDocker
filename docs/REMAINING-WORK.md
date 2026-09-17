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
project-pause acceptance, usage collection, release/platform work and hands-on acceptance
remain below. Optional research is outside the current delivery closure.

## Delivered source and current desktop

The installed desktop and daemon use reviewed local-preview release `d14610b7`,
source `3785e81` with runtime inputs identical to merged `e9c4ab2`, schema 23.
The September 17 20:55 UTC activation used a fresh integrity-checked state backup;
both provider processes (Codex51242 and Claude18563) and every registered live
identity remained unchanged. Receiver 25642 became 18735 with the same binding,
token and all 128 retained receipts in order. Release `f5e298f4` is retained.

The task board (#176), persisted message drafts (#178), preserved resume state
(#179) and startup ordering (#180) are merged after final review and all five
CI workflows, and are now installed. The combined source gate passed 1,167 Rust
tests (seven skipped), 94 Python checks (one skipped) and the release checks;
the packaged candidate passed 485 rendered steps/28 checks across 11 windows.
An actual installed conversation draft containing Unicode survived close/reopen
and returning to that conversation. It was never sent and the test text was
cleared; question, board and other form drafts remain separate.

The actual Claude MCP entry was updated through its own configuration CLI to
include `--claude-channel`, preserving unrelated entries; no provider restart or
parent input opt-in occurred. Safe existing-session handoff/idle receipt remains
open. See [installed evidence](verification/2026-09-12-integrated-desktop.json).

| Delivered behavior | Evidence and limits |
| --- | --- |
| Simpler home, projects, Messages and Tools | PRs #119/#129/#152/#160/#163 are merged: compact navigation, saved project names, resizable panes, readable session names, hidden-pane read protection, and collision notices excluded from the person's unread total. |
| New DM/channel, invitations, mentions and submit | PR #170 is merged and installed. **+** opens creation, channel members can be invited, suggestions use the selected conversation's live recipients, Unicode names work, and saved retired names retain mention counts. Enter is wired to submit in every single-line composer. Final source `5c461e0` passed 1,108 Rust tests (seven skipped), 84 Python checks and the full lint/doctest/package/release gate. The preceding runtime passed 415 rendered steps/26 outer checks; final graphical CI passed. Actual installed New DM/channel controls opened successfully. Physical Enter/IME and multiline input remain below. |
| Durable messaging workspace | PR #150 archives conversations, channels, threads, search and reader cursors. Human read markers acknowledge displayed rows; they do not acknowledge an agent's provider-input queue. Creation/invitation membership, events and notices commit atomically. |
| Codex input priority and safe receiver replacement | PRs #162/#169/#171 are merged. Human and peer input share the provider route. Actual installed CLI human-route, Claude peer and project fan-out messages entered this same live Codex turn without another prompt or manual queue acknowledgement. One CLI sample reached provider context in 0.176 s; this is a sample, not a latency guarantee. After the September 17 receiver upgrade, one direct CLI-origin self-addressed message also started a fresh Codex turn while idle, without human input; its exact completed receipt and correlated reply are in the [installed record](verification/2026-09-12-integrated-desktop.json). Active hook context is limited to 6,000 bytes and tool boundaries. [Exact receipts and limits](verification/2026-09-15-native-codex-queue.json). |
| Reconnect implementation | PR #164 merged as `ce0ff1d` after final-head review and all CI checks. Eligible ended records of the same provider session fold transactionally, preserving durable queue order, names and ancillary state; unsafe/live cases refuse. Combined source `3c41b3b` passed 1,116 Rust tests (seven skipped), 84 Python checks and an actual MCP adapter resume fixture (38.41 s, no survivors). The implementation is included in installed `d14610b7`; actual Claude idle-resume acceptance remains open. This fixture is not a live model. |
| Process ownership and experimental reload | PRs #130/#155/#161 are merged. Session owners, transfer fencing, successor readiness, retry/reconciliation and input-binding preservation have bounded process/client trials. PR #155 merged as `fec093c`; final integration `f4ef6c3` passed 1,087 Rust tests (seven skipped), 84 Python checks and the full release gate. Source-specific pressured and native-client handovers remain in [reload evidence](verification/2026-09-16-reload-controller-episode.json). The experimental gate stays on. |
| Compatible launcher and notifications | The `unknown argument: hook` collision is repaired and the user confirmed Claude prompts work. Applications installation and old hook paths are compatible. AppleScript fallback removal and native destination routing are implemented; approved installed Notification Center clicks have bounded passing evidence. |
| Provider-limit framework and maintenance | Durable availability, queue gating, exact recovery and quota isolation cover the common interruption contract. Bounded journal/checkpoint retention and pure-core environment cleanup are implemented. Broader detection, actual account recovery and sustained acceptance remain open. |

`make install` from the repository root updates the app and CLI. It asks the
running daemon to reload; with experimental reload disabled, that daemon keeps
serving until an explicit safe restart. Use `agentdocker desktop status` and
`agentdocker daemon status` for the actual installed and serving versions. The
current installed app is already updated; no user installation is needed for the already activated features.

The notification follow-up also cancels a prior message reveal as soon as a
new notification for this workspace arrives, including while its destination
is still waiting for snapshots. Focus-only and foreign-workspace activations
preserve the current reveal. The regression checks a late page from the old click.

## Engineering and acceptance still open

| Priority | Work remaining | Completion condition and evidence |
| --- | --- | --- |
| Top | Existing idle sessions and provider parity | The current plain Claude session has hooks/MCP and no input binding; recent handoffs can wait at its idle prompt. PR #177 is installed and the actual MCP entry includes `--claude-channel`, but provider startup consent and a same-session relaunch remain. Production preflight found six lives with retained observations and open-channel membership. PR #179 now preserves bounded observations, memberships, queue order, aliases and typed task assignee/creator references transactionally; conflicting captures, unsafe ownership and active input/subscribers still refuse the fold. Runtime `197bca4` passed 1,165 Rust tests (seven skipped), 94 Python checks (one skipped), the full release gate, a real-daemon before/after card migration and the MCP transport fixture. PR #180 keeps MCP control/receipts responsive while waiting up to ten seconds for SessionStart to name the same provider process generation; command arguments do not establish session identity. Its final runtime `a45f831` passed 1,149 Rust tests (seven skipped), 94 Python checks (one skipped) and the full gate. PR #179 merged as `c73b475` after final review and all five CI workflows. PR #180 merged as `e9c4ab2` after final review and all five CI workflows; both fixes are installed in `d14610b7`. Still prove actual Claude peer-only idle input after consent/relaunch, preserving original queue IDs and preventing duplicate execution. Other supported runtimes still need equivalent adapters and idle/busy/limit acceptance. [Message audit](MESSAGE-DELIVERY-AUDIT.md). |
| Top | Project pause and per-recipient delivery | The missed CLI/global and app/project pauses are reproduced and retained. Installed Codex bounded active input now passes, including real project fan-out; queue acceptance alone still does not prove every recipient paused. PR #173 merged as `d0cd2d7` after final-head review and all CI checks; it implements durable project pause/resume. Its reviewed follow-up binds forms and replies to project/request identities, preserves reasons on queue/transport refusal, bounds draft/command storage and places form actions on a separate row. Runtime `60380fd` passed 1,125 Rust tests (seven skipped), 84 Python checks and the full gate; the final isolated native workflow passed 429 rendered steps/26 outer checks, including 720×540 pause composition. Installed acceptance remains open. The actual schema 23-to22 rollback-refusal trial passed and is folded into the existing integrated desktop record; lifting a pause does not downgrade the database. Actual idle/busy recipient consumption remains open. |
| Top | Native input lifecycle and throughput | PR #178 implements conversation/thread, channel and session text persistence. Source `017ef68` passed 1,152 Rust tests (seven skipped), 94 Python checks (one skipped), the full release gate and 461 rendered steps across 11 windows with 27 checks. Native close/reopen retained all three draft destinations and sent none of their text automatically. Review-requested channel close/reopen coverage also checks its queue and archive; PR #178 merged as `8bd054f` after final-head review and all five CI workflows. Installed `d14610b7` passed an actual conversation-draft close/reopen check with no submission. It restores unsent text, preserves newer edits across old receipts, refuses storage overflow without evicting nonempty drafts, and holds a failed close for retry or explicit unsaved closure. Question answers and other forms remain window-local; their persistence remains open. Complete remaining draft/form persistence, zero-prompt startup/reopen, oversized input, burst latency, prolonged busy/approval waits, sleep/reboot, uncertain writes and replacement across actual provider versions. The installed existing-session idle wake and bounded active-input cases pass; one outstanding offer and provider polling cadence still bound throughput. [Codex input](CODEX-INPUT.md). |
| Top | Provider-limit detection and recovery | Common tests cover all catalog runtimes, custom runtimes and nine normalized interruption classes; bounded actual Claude/Codex and mid-tool 429 recovery pass. Actual account resets, additional provider versions/adapters, unrelated replacement identities and sustained use remain. Unsupported adapters do not acquire inferred signals; unknown quota scope/reset stays unknown. [Limit acceptance](MESSAGE-DELIVERY-AUDIT.md#provider-limit-and-session-exhaustion-acceptance-september-14). |
| Top | Provider review and input handling | Local command review and managed-network presentation are merged. The actual network trial stopped at the provider allowlist before a callback. Finish stdin review, broader permissions, MCP elicitation and secret input; retain exact human/peer queue ordering through interruption and uncertain writes. [Input contracts](CODEX-INPUT.md). |
| Engineering / acceptance | Setup and input readiness | Named missing-hook diagnostics and channel-capable Claude setup/inventory are included in installed `d14610b7`. The actual global MCP entry now includes `--claude-channel`; unrelated entries were preserved and no provider was restarted. Configuration, contact, receiver readiness and actual receipt remain distinct. Finish the safe existing-session handoff and provider consent, then prove idle input in the actual session. PR #177 merged as `4fb3ae8`. Full build, private-profile setup/undo, MCP fixture and installation evidence are retained in the [integrated record](verification/2026-09-12-integrated-desktop.json). [Guided setup](GUIDED-SETUP.md). |
| Acceptance | Notification release cases | The installed `f5e298f4` preview includes bounded archived-message reveal, fresh-history checks, explicit queue-refusal errors and cancellation when a newer notification arrives; 171 UI tests and 429 native steps/26 checks passed. On September 17 an actual Notification Center press on that installed revision, with the app explicitly hidden, made it visible with the exact message revealed and every provider process unchanged — the person's already-open-app case is closed for that installed revision ([record](verification/2026-09-12-integrated-desktop.json), `installed_notification_click_2026_09_17`); a second press with no GUI process launched the installed app and revealed the message in 1.7 s (`installed_cold_notification_click_2026_09_17`), closing zero-process cold launch for that revision. Earlier installed foreground/background clicks, pending questions, stale destinations and draft preservation passed within their recorded scope. Finish physical clicks, bounded page-back and broader historical/project routes on the signed final package. PR #175 merged as `2f74955` after final-head review and all five CI workflows. [Notification audit](NOTIFICATION-ROUTING-AUDIT.md). |
| Acceptance | Safe live daemon replacement | The source is merged; `AGENTDOCKER_EXPERIMENTAL_RELOAD` remains required. Complete real model-service and distinct-source provider handovers, Claude/other provider polling, attached-terminal drafts, output drain and uncertainty recovery under sustained use before removing the gate. Successful explicit coordinator restarts do not close this work. [Replacement contract](LIVE-DAEMON-UPGRADES.md). |
| Operational | Legacy production duplicate reconciliation | Offline preview/apply and rollback are implemented in merged #126. Earlier private previews found three safe historical pairs (575 inbox rows, 139 duplicate copies). Recompute plans from a fresh backup only after all required nonhuman records have ended and the daemon is stopped. Never merge by display name or stop live providers to tidy the list. Current same-session resumption is separate. [Identity repair](IDENTITY-REPAIR.md). |
| Acceptance | Sustained storage and real-provider use | A 20-minute retention trial passed with ten registered agents, ordered readers, eligible checkpoint removal and bounded storage/memory; it did not use actual provider conversations. A 7.5-hour fixture run predates current ownership/availability. Complete longer current-candidate, many-project, real-provider, sleep/wake and reboot trials. Retain the historical incomplete Iced capture, benchmark socket timeout and unexplained Linux ARM transport refusal. September 17 PR #172 Linux x86 CI separately captured an lsof mount-stat warning: transport verification refused. PR #174 adds a bounded Linux socket observer. Its first graphical run refused changing/unclassified sockets; private concurrent-RPC trials reproduced the issue and the bracketed-sample follow-up passed 100 samples. PR #174 merged as `764955a` after clean final-head review and all five CI workflows passed on `4fcf2d5`, including Linux ARM/x86 and both macOS graphical jobs. Its report accepts at most 4,096 socket descriptors and requires readable TCP6 evidence; the Python suite ran 94 checks with one Linux-only skip. The previous refusals remain evidence. [Testing standard](TESTING-AND-BENCHMARKS.md), [retention evidence](verification/2026-09-15-retention-sustained-use.json). |
| Acceptance | Watcher resource limits | Removed-checkout recovery is complete within its tested scope: two regressions passed 100 repetitions and installed `3c8c2e1` recovered a removed checkout in 0.31 s with four providers unchanged. Overnight/many-project acceptance of one FSEvents stream per checkout remains. [Watcher evidence](verification/2026-09-11-macos-watcher-recovery.json). |
| Release | Publish verified downloads and updates | Packaging, update consumer, scheduler and archive/feed automation exist. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and actual update/rollback acceptance. A generated cask is not a published route. [Distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md). |
| Platform | Linux and independent Mac acceptance | Complete target-distribution desktop/service/package trials, a second Mac and physical Intel testing. ARM64/x86-64 graphical/package CI and Rosetta are useful evidence but do not establish independent hardware acceptance. [Local trial](LOCAL-TRIAL.md). |
| Platform | Full native Windows product | Named-pipe foundations exist. Finish daemon/client integration, supervision, ConPTY, identity-safe stop/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and actual native graphical/provider acceptance. [Windows port](WINDOWS-PORT.md). |
| Input | Accessibility, physical keyboard and input methods | Exercise VoiceOver/supported screen readers, focus, keyboard activation, zoom, IME, Unicode and broader terminal copy/paste. Automated submit actions do not establish physical Enter. On September 17, both installed `79981beb` and updated `f5e298f4` accepted targeted synthetic Return key events in its project composer: one message was archived, the composer cleared and Codex received the original ID with a completed receipt. This verifies the native key-event path; human keyboard and IME composition remain untested. Multiline/Shift-Enter composition is not implemented. [Iced contracts](ICED-DESIGN.md). |

## Requested September 15

| Status | Existing request | Remaining scope |
| --- | --- | --- |
| Delivered with acceptance remaining | Messaging as a workspace (Slack/Discord shape) | Archive/search/threads/read state, project navigation, resizable panes, mentions, New DM/channel and invitations are merged and installed. Finish the physical input checks and the installed project-pause acceptance above. [Desktop behavior](DESKTOP-UX.md#messages-inbox-and-tools). |
| Acceptance | Portable coordination skill | PR #149's single bundled SKILL.md, export and preview/apply/undo are implemented and included in the installed source. Bounded Codex/Claude loader and Claude setup/undo trials passed. Fresh-session implicit activation, current installed-candidate checks and other runtime loaders remain. [Shared coordination skill](GUIDED-SETUP.md#shared-coordination-skill). |
| Engineering | Typed links on cards, messages and hand-offs | `links` — up to sixteen `{kind, target, note?}` of kind path, commit, pr, url, task, message or memory, each checked for its kind's shape — on `task_create`/`task_update`, `send`, `checkpoint` and `handoff`, carried on the card, the envelope, the checkpoint and the bundle; `--link kind:target` on the matching commands and in the MCP tools; shown under a message and on an open card in the app; the daemon opens nothing (`links_have_a_kind_and_a_shape`, card/message/hand-off links in the daemon tests). The typed-links item from the Paprika research. |
| Engineering | Command-line exit status for agents | PR #181 is in final integration; it is not included in installed `d14610b7`. The `agentdocker` command in that source ends with a status by the class of the daemon's answer (2 invalid, 3 not found/ambiguous, 4 held, 5 refused/paused, 6 unavailable, 1 unexpected or not the daemon's answer), words and details on stderr, so an agent driving the CLI branches without parsing text; a real-binary test drives daemon statuses 1 and 3–6 against a fake daemon and separately checks parser status 2 (`the_exit_status_is_the_class_of_what_went_wrong`). This is the exit-code contract the Paprika research listed as worth taking; successful command output is unchanged. |
| Engineering | Token usage by agent, model and provider | Parser/accounting foundation #165 and bounded file reader #167 are merged; the reader is in installed `d14610b7`. Its final review and all CI workflows passed. Oversized-record quarantine and accepted-prefix validation are covered; prefixes beyond 16 MiB remain incomplete. Directory discovery, larger/growing-file prefix validation, atomic ingestion/retention, historical attribution, protocol, CLI, Usage screen and separate emitted-byte overhead remain unbuilt. Missing coverage stays unknown; totals are tokens, not money. Earlier failed trials and corrections remain in the [integrated record](verification/2026-09-12-integrated-desktop.json). [Architecture contract](ARCHITECTURE.md#planned-protocol-and-event-additions). |
| Engineering / acceptance | A board of work beside the sessions | PR #176 implements the project Board, durable cards with acceptance text, five columns, create/edit/assign/archive controls and paged reads. An agent pulls a Ready card under an exclusive `task:<id>` lease; paused projects, live holders and uncertain storage refuse the change. Recovery of an expired hold names its prior holder. UI requests keep their project/request identity, preserve drafts and the visible card count through refresh, and report transport or queue refusal. CLI and MCP expose the same operations. PR #176 merged as `d2fa399` after final review and all five CI workflows, including both macOS graphical jobs. Installed `d14610b7` includes the Board. Filing existing throughput card `027e77502b68` through its actual controls passed, with exact title/acceptance verified through CLI and durable storage. The combined candidate passed 1,167 Rust tests (seven skipped), 94 Python checks (one skipped), and 485 rendered steps/28 checks across 11 windows, including board actions. Typed links, roles and webhooks are separate follow-ups in the [landscape note](LANDSCAPE-2026-09-11.md). |

Federation/host namespaces, cross-host leases/routing, the herdr focus bridge,
additional adapters, container log following and proposed CLI conveniences are
existing deferred proposals. They are not prerequisites for current single-host
delivery. See [product direction](PRODUCT-DIRECTION.md),
[architecture proposals](ARCHITECTURE.md#planned-protocol-and-event-additions)
and [containers](CONTAINER-ENGINES.md).

## Manual and operational steps

| Step | What remains |
| --- | --- |
| Apple signing/notarization | Supply a Developer ID Application identity and private notary profile, then sign, notarize, staple and run Gatekeeper checks on the final app/DMG. The September 17 read-only check found no valid signing identity. Ad-hoc signing validates a local preview only. |
| Human accessibility/IME trials | Run the hands-on input cases above on the actual candidate and record findings. Automated accessibility controls do not replace them. |
| Completed on this Mac: launcher and coordinator switch | The old launcher problem is closed. The earlier `79981beb`/schema22 switch activated desktop, daemon and receiver from a verified package with backup and all four external provider processes retained; the current `d14610b7`/schema 23 switch (September 17, 20:55 UTC) did the same with both live providers and all 128 retained receipts preserved. This does not certify future unattended live transfers. |
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

The September 17 read-only release check still found v0.1.0 as the latest
published release and only a README under the tap's Casks directory. The September 9 check
found a publishing variable and token secret name; it did not inspect secret
values or establish token validity. Publication must be verified when performed.

Current active Codex/Claude exchanges have exact queue/context receipts in the
existing native queue record. They establish active delivery, not idle wake for
a plain Claude session or universal-provider completion.
