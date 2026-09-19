# Remaining engineering and release work

Current audit: **September 19, 2026 UTC**, main `16bf69a`. This is the
single backlog for existing project requirements. The [documentation index](README.md)
classifies reference documents, completed historical audits, implemented features
and outstanding acceptance. The root README's duplicate roadmap is removed.
Original source pins and failed trials remain in the existing verification records;
a new merge does not turn an old test into evidence for different binaries.

**Not everything is complete.** The immediate coworker-trial blockers are the
last integrations, a versioned downloadable desktop candidate, and fresh-machine
acceptance. The user confirmed all three platforms for the first coworkers:
macOS, Linux and native Windows. Windows completion and Linux desktop acceptance
therefore belong to this rollout, alongside full provider parity and the matrix
below. They are not silently removed from scope to declare v1 done.

## The road to v1: a release other people can install and try

A coworker preview and a stable release have different acceptance requirements.
The existing workflow permits an explicitly labeled unsigned **prerelease**;
a **stable** macOS release requires Developer ID signing and notarization.
The next version and supported-platform promise must match the candidate we test.

| # | Existing requirement | State on September 19 | What remains |
| --- | --- | --- | --- |
| 1 | Integrate reviewed implementation | Done: #191 `abec6ec`, #196 `bfd79ff`, #194 `615ae79`, #199 `7022ead`, #198 `181d78b`, #201 `57a9d4e`, #202 `14f1c519`, #203 `16bf69a`; connector changes are also on main. Open: #200 shared-chat workspace and #204 build-campaign ownership. | Finish final-head CI/review and merge these two. #203 merged after review fixes; #204 still has no substantive CodeRabbit review because of the rate limit. Independent reviews and passing tests remain recorded. |
| 2 | Publish a current desktop candidate | Not done. The only published release is [v0.1.0](https://github.com/brandopakel/AgentDocker/releases/tag/v0.1.0), from before the current desktop; default installer/tap downloads still use it. Packaging, four-target macOS/Linux desktop archives, checksums, feeds and formula/cask generation are implemented; Windows packaging remains to build. | Select the integrated commit and matching Cargo/tag version; run the tag workflow; verify the actual hosted archives, explicit-version installer and update/rollback. A prerelease must stay off the stable feed/tap. Tag protection prevents deletion/rewrites, not creation; it is not a manual-only blocker. |
| 3 | A Mac download that opens | Local previews are ad-hoc signed. Read-only `security find-identity -v -p codesigning` on September 19 found zero valid identities. | For stable release: configure private Developer ID/notary credentials and validate the final download with Gatekeeper on an independent Mac. An unsigned preview uses the explicit prerelease route and documented per-app approval; it cannot be advertised as a signed stable app. |
| 4 | First run from the README | Fresh-state #199 CLI rehearsal passed (`newcomer_first_run_2026_09_18` in the [integrated record](verification/2026-09-12-integrated-desktop.json)); empty-provider-profile preview/apply/undo also passed on installed `ce82d06`. No fresh OS-user, actual first-profile model session or second-machine desktop acceptance is claimed. | Install each platform's actual candidate on a clean account/machine, add a coworker's project, preview/apply one provider integration, complete provider consent, verify a real idle/busy message and restart/reopen with drafts intact. Test the download without development checkout paths or existing user configuration. |
| 5 | App-guided Claude reconnect and idle receipt | **Complete for the recorded managed Claude trial.** #199/#196 are merged. After the person's in-app consent, project probe `91b30d039e3c4d09` started the existing session's idle turn and reply `77976146612248b9` arrived about seven seconds later without keyboard input. See `user_session_idle_wake_2026_09_18` in the [input-delivery record](verification/2026-09-12-input-delivery-status.json). | Keep broader provider/version and pause-throughput acceptance open. A separate plain Claude process without the input channel is not made wakeable by this successful trial. |
| 6 | Useful coworker bug reports | **Complete:** the [trial issue template](../.github/ISSUE_TEMPLATE/trial-report.md) is on main through #198. It requests actual app/daemon/runtime status and reproduction steps. | Testers use the template; redact private project paths, prompts and credentials before posting. |
| 7 | Coworkers on all three platforms | **Required by the user for the first rollout:** macOS, Linux and native Windows. macOS has local evidence; Linux has graphical/package CI; Windows has the daemon and CLI answering over the named pipe on a real runner (#206) and no candidate yet. | Finish native Windows supervision/ConPTY, provider setup, service and installer/update path. Produce each platform's candidate and run the same first-run, real-provider queue/idle/busy/limit, terminal, draft, sleep/restart and update/rollback cases on declared OS versions/architectures. macOS/Linux availability does not close Windows. |

## Delivered source and current desktop

These are different installations; the shared `0.1.0` version string does not
identify their source. Read `agentdocker desktop status` and `agentdocker daemon status`.

| Component | Last verified September 19 | Meaning |
| --- | --- | --- |
| Main | `16bf69a` | Includes usage collection, reconnect, naming, peer-answer delivery and session presentation; two integrations remain above. |
| Installed app/CLI | Local preview `9e75e834`, clean source `ce82d06` | Shared-chat candidate from #200; packaged workflow passed 545 rendered steps / 31 checks across 12 windows and 13 installation/rollback scenarios. Final runtime `932b566` passed 1,284 Rust tests (seven skipped), 94 Python checks (one skipped), lint, doctests, packaging and release build. These are source-specific results, not a new gate on main. |
| Serving coordinator | `6bd97894`, source `705f924`, schema 23, PID 92608 | Preserved to keep managed agents running. Experimental reload remains disabled; the app update did not activate newer daemon features such as usage collection. |
| Codex input receiver | `486c5dd0`, source `c0a7c56`, PID 60959 | Fixed active peer-answer delivery, provider process/binding preserved; #202 is now merged. |
| Public download | `v0.1.0`, source `52fd88d` | Does not contain the current desktop. |

The shared-chat installation preserved all eight live registry IDs, five provider
PID/birth pairs and the running receiver/coordinator. Its source and packaged
checks are retained in #200's existing integrated-desktop record; installed visual
review was still pending while the display was locked. New source, actual keyboard,
IME and screen-reader testing need their own acceptance. No restart of the serving
coordinator is required merely to correct these docs.

### Completed implementation

| Delivered requirement | Completion evidence and boundary |
| --- | --- |
| Launcher and notification navigation repair | The `unknown argument: hook` collision is repaired; the person confirmed ordinary Claude prompts work. AppleScript fallback is removed. Actual installed foreground/background, archived-message and cold-start notification routes passed; physical Reply and signed-release cases remain below. |
| Shared messaging queue | Durable archive, channels, direct messages, threads, search, invitations, mentions and read cursors are merged. Provider receipts are separate from a person's read markers. New DM/channel and Enter actions have rendered/native-event evidence. |
| Codex peer-answer stall | #202 merged `14f1c519`. Fixed source `ee7bb1a` passed its full gate and a real Codex 0.154.0/loopback-model trial: a peer answer followed by two human messages arrived in order during one turn in 8.63 s. Lost-hook output stayed queued without false receipt/replay. Installed receiver recovery delivered the original blocked message; no manual queue acknowledgement. [Exact sources](verification/2026-09-15-native-codex-queue.json). |
| Claude channel receipt and app reconnect | #196/#199 are merged and the person's managed-session consent/idle trial passed (row 5 above). Missing explicit ACK, final-response flush and startup ordering have retained failing-before/passing-after trials. This does not make a plain external session wakeable. |
| Draft persistence | Conversation/thread, session/channel, question-answer and Board drafts are implemented and restored as unsent; #178 and combined #191 are merged. Current packaged workflow tests reopen hidden drafts without sending or filing them. Other forms and physical input remain below. |
| Project pause and readiness | #173 and combined #191 implement durable project pause/resume, refusal of new agent leases and per-recipient missing/stale/limited input warnings. Delivery to every actual provider and latency still require acceptance. |
| Board, roles, typed links, webhooks, CLI exit statuses | #176/#181/#182/#183/#191 implement the requested local work board and coordination additions. Their broader acceptance limits remain below; optional external-service bridges are separate. |
| Initial usage collection and UI | #194 merged `615ae79`; collector, persistent ingestion, CLI/MCP and Usage screen have real-binary and native workflow evidence. Activation and the explicit collection/resource gaps below remain. |
| Retention, availability and safe process ownership | Bounded journal/checkpoint retention, provider-limit queue gating, transactional resumption and supervised owner processes are implemented. Experimental live reload retains its gate until the remaining matrix passes. |
| Tester reporting and documentation inventory | Trial issue template is merged. Historical audits are complete records; reference docs describe implemented contracts. Neither is an unfinished feature checklist. |

`make install` from the repository root updates the app and CLI. It requests a
daemon reload; with the experimental gate disabled the daemon keeps serving.
Do not stop managed agents merely to make installed source versions agree.

## Engineering and acceptance still open

| Priority | Work remaining | Completion condition and evidence |
| --- | --- | --- |
| Top | Existing idle sessions and provider parity | The recorded managed Claude consent/idle trial and Codex bounded idle/active trials pass. Finish equivalent adapters and idle/busy/limit/receipt acceptance for other supported runtimes, zero-prompt startup/reopen and additional provider versions. Plain hooks/MCP alone do not wake a model. Resumption must preserve identity, original queued IDs, questions, observations and task ownership; unsafe live/subscribed cases refuse. [Message audit](MESSAGE-DELIVERY-AUDIT.md), [Claude input](CLAUDE-CHANNEL-INPUT.md). |
| Top | Project pause and per-recipient delivery | The source is implemented and actual Codex project fan-out and managed Claude idle project input have receipts. Still verify every intended recipient stops at a safe boundary, replies in the original chat, and resumes after a real project pause during idle, busy and limited states. Queue acceptance is not proof everyone paused. Keep missed CLI/global and app/project episodes in the records. |
| Top | Native input lifecycle and throughput | Conversation/thread, session/channel, question and Board drafts are implemented and installed as unsent text. Remaining work: other form drafts, zero-prompt startup/reopen, oversized messages, measured burst latency, prolonged busy/approval waits, sleep/reboot, uncertain writes and receiver replacement across actual provider versions. Active hook input is bounded to 6,000 bytes and tool boundaries; one outstanding offer and provider polling limit throughput. [Codex input](CODEX-INPUT.md). |
| Top | Provider-limit detection and recovery | Common tests cover all catalog runtimes, custom runtimes and nine normalized interruption classes; bounded actual Claude/Codex and mid-tool 429 recovery pass. Actual account resets, additional provider versions/adapters, unrelated replacement identities and sustained use remain. Unsupported adapters do not acquire inferred signals; unknown quota scope/reset stays unknown. [Limit acceptance](MESSAGE-DELIVERY-AUDIT.md#provider-limit-and-session-exhaustion-acceptance-september-14). |
| Top | Provider review and input handling | Local command review and managed-network presentation are merged. The actual network trial stopped at the provider allowlist before a callback. Finish stdin review, broader permissions, MCP elicitation and secret input; retain exact human/peer queue ordering through interruption and uncertain writes. [Input contracts](CODEX-INPUT.md). |
| Engineering / acceptance | Setup and input readiness | Saved preview/apply/undo, named hook diagnostics, channel-capable setup, terminal shell opt-in and **Reconnect here** are implemented. The person accepted the app consent and idle receipt passed. Empty-profile preview/apply/health/undo passed on the recorded `ce82d06` package, including both skills and shell setup. Actual fresh-provider authentication/trust/consent/delivery and independent-user acceptance remain. Configuration, contact, receiver and receipt remain separate evidence. Provider startup consent remains the provider's requirement. [Guided setup](GUIDED-SETUP.md). |
| Acceptance | Notification release cases | The installed `f5e298f4` preview includes bounded archived-message reveal, fresh-history checks, explicit queue-refusal errors and cancellation when a newer notification arrives; 171 UI tests and 429 native steps/26 checks passed. On September 17 an actual Notification Center press on that installed revision, with the app explicitly hidden, made it visible with the exact message revealed and every provider process unchanged — the person's already-open-app case is closed for that installed revision ([record](verification/2026-09-12-integrated-desktop.json), `installed_notification_click_2026_09_17`); a second press with no GUI process launched the installed app and revealed the message in 1.7 s (`installed_cold_notification_click_2026_09_17`), closing zero-process cold launch for that revision. Earlier installed foreground/background clicks, pending questions, stale destinations and draft preservation passed within their recorded scope. Finish physical clicks, bounded page-back and broader historical/project routes on the signed final package. PR #175 merged as `2f74955` after final-head review and all five CI workflows. [Notification audit](NOTIFICATION-ROUTING-AUDIT.md). |
| Acceptance | Safe live daemon replacement | The source is merged; `AGENTDOCKER_EXPERIMENTAL_RELOAD` remains required. Complete real model-service and distinct-source provider handovers, Claude/other provider polling, attached-terminal drafts, output drain and uncertainty recovery under sustained use before removing the gate. Successful explicit coordinator restarts do not close this work. [Replacement contract](LIVE-DAEMON-UPGRADES.md). |
| Operational | Legacy production duplicate reconciliation | Offline preview/apply and rollback are implemented in merged #126. Earlier private previews found three safe historical pairs (575 inbox rows, 139 duplicate copies). Recompute plans from a fresh backup only after all required nonhuman records have ended and the daemon is stopped. Never merge by display name or stop live providers to tidy the list. Current same-session resumption is separate. [Identity repair](IDENTITY-REPAIR.md). |
| Acceptance | Sustained storage and real-provider use | A 20-minute retention trial passed with ten registered agents, ordered readers, eligible checkpoint removal and bounded storage/memory; it did not use actual provider conversations. A 7.5-hour fixture run predates current ownership/availability. Complete longer current-candidate, many-project, real-provider, sleep/wake and reboot trials. Retain the historical incomplete Iced capture, benchmark socket timeout and unexplained Linux ARM transport refusal. September 17 PR #172 Linux x86 CI separately captured an lsof mount-stat warning: transport verification refused. PR #174 adds a bounded Linux socket observer. Its first graphical run refused changing/unclassified sockets; private concurrent-RPC trials reproduced the issue and the bracketed-sample follow-up passed 100 samples. PR #174 merged as `764955a` after clean final-head review and all five CI workflows passed on `4fcf2d5`, including Linux ARM/x86 and both macOS graphical jobs. Its report accepts at most 4,096 socket descriptors and requires readable TCP6 evidence; the Python suite ran 94 checks with one Linux-only skip. The previous refusals remain evidence. [Testing standard](TESTING-AND-BENCHMARKS.md), [retention evidence](verification/2026-09-15-retention-sustained-use.json). |
| Acceptance | Watcher resource limits | Removed-checkout recovery is complete within its tested scope: two regressions passed 100 repetitions and installed `3c8c2e1` recovered a removed checkout in 0.31 s with four providers unchanged. Overnight/many-project acceptance of one FSEvents stream per checkout remains. [Watcher evidence](verification/2026-09-11-macos-watcher-recovery.json). |
| Release | Publish verified downloads and updates | Packaging, update consumer, scheduler and archive/feed automation exist. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and actual update/rollback acceptance. A generated cask is not a published route. [Distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md). |
| Top / platform | Linux and independent Mac acceptance | Complete target-distribution desktop/service/package trials, a second Mac and physical Intel testing. ARM64/x86-64 graphical/package CI and Rosetta are useful evidence but do not establish independent hardware acceptance. [Local trial](LOCAL-TRIAL.md). |
| Top / platform | Full native Windows product | Slice one is in source (#206): `agentd` and `agentdocker` build and lint natively, the daemon serves the shared named pipe and the CLI registers, lists, sends, reads, claims and stops through it, with the birth-checked `procinfo::end`, `files::identity` and thread-bounded hook I/O shared by every platform. The Windows workflow runs `scripts/windows_daemon_smoke.py` on a real runner (its `windows-daemon-smoke` artifact is the evidence; no person has run a Windows build, so no verification record exists). The first runner exposed what a cross-compile cannot: the CLI overflowed the 1 MiB main-thread stack on `ping` (both binaries now run on a 32 MiB thread) and a home the smoke pre-created was refused as Administrators-owned state (the daemon now creates its own, the first-run path). Refused on Windows in words, not with a hang: managed sessions and `attach`, the native Codex queue, `daemon reload`, container transport, the installer and `daemon install`/`uninstall` (no service yet; `daemon start`/`stop`/`status`/`vacuum` still speak to the daemon over the pipe, and the smoke starts one on demand). Finish ConPTY and a session owner, the Codex queue over the pipe, provider/desktop inventory, service/session startup, installer/update/rollback, the daemon and CLI suites on the runner, then native graphical and real-provider acceptance. [Windows port](WINDOWS-PORT.md). |
| Engineering / acceptance | Agents that work inside the browser | Inventory/helper exclusion and the OAuth/MCP connector are merged. Actual Claude and ChatGPT accounts exchanged scoped messages through the login service and stable Tailscale hostname; browser sessions stay in their chosen project. Multi-project consent, egress allowlisting and CIMD parsing are implemented. Remaining: real-account CIMD connection, desktop service start/install, periodic OpenAI egress-feed refresh and additional verified vendor identities. Browser inboxes are polled by the hosted model, not idle-woken. [Connector contract and evidence](REMOTE-CONNECTOR.md). |
| Input | Accessibility, physical keyboard and input methods | Exercise VoiceOver/supported screen readers, focus, keyboard activation, zoom, IME, Unicode and broader terminal copy/paste. Automated submit actions do not establish physical Enter. On September 17, both installed `79981beb` and updated `f5e298f4` accepted targeted synthetic Return key events in its project composer: one message was archived, the composer cleared and Codex received the original ID with a completed receipt. This verifies the native key-event path; human keyboard and IME composition remain untested. Multiline/Shift-Enter composition is not implemented. [Iced contracts](ICED-DESIGN.md). |

## Requested September 15

| Status | Existing request | Remaining scope |
| --- | --- | --- |
| Delivered with acceptance remaining | Messaging as a workspace (Slack/Discord shape) | Conversation archive, named rooms, creation/invites, mentions, threads, search and durable drafts are merged. Naming #201 and session presentation #203 are merged; #200 shared-chat integration remains in review and installed as a local preview. Finish installed visual/physical-input acceptance and multiline/Shift-Enter composition. Historical aliases keep their own history; separate concurrent sessions are not merged merely because they use the same provider. |
| Acceptance | Portable coordination skill | PR #149's single bundled SKILL.md, export and preview/apply/undo are implemented and included in the installed source. Bounded Codex/Claude loader and Claude setup/undo trials passed. Fresh-session implicit activation, current installed-candidate checks and other runtime loaders remain. [Shared coordination skill](GUIDED-SETUP.md#shared-coordination-skill). |
| Installed; acceptance remaining | Roles as hand-off labels | Implemented via #191: project-scoped role routing requires one live holder and refuses missing/ambiguous roles. Broader coworker handoff acceptance remains; roles do not grant permissions or create a scheduling service. |
| Installed; acceptance remaining | Webhooks on the event stream | Implemented in #183: opt-in signed event sinks, bounded queues/deadlines/retries, throttled failure events and generation-safe reload. Local receiver failure-path tests pass. Real chosen destination acceptance remains. Delivery is best effort without a durable outbox; no destination is enabled by default. |
| Installed; acceptance remaining | Typed links on cards, messages and hand-offs | Implemented in #182, with at most sixteen shape-checked links on messages, cards, checkpoints and handoffs. App renders them and the daemon opens nothing. Broader actual-project link/navigation acceptance remains. |
| Installed; acceptance remaining | Command-line exit status for agents | Implemented in #181: distinct classes for invalid, not-found, held, refused/paused, unavailable and unexpected outcomes; real CLI fixture covers parser/daemon statuses. Final coworker automation acceptance remains. |
| Installed; acceptance remaining | Reply from the notification | Implemented via merged #191: original-destination routing, exact success/refusal/unknown outcome, and unsent reply retention beside the composer. A physical Notification Center Reply on the final signed candidate remains untested. |
| Merged; installation and acceptance remaining | Token usage by agent, model and provider | Initial collector/protocol/CLI/MCP/UI and final CI/review are complete in merged #194. Serving-daemon activation is pending. The frozen 692 MB actual-session corpus matched 4,703 unique Claude counters exactly, retaining one explicit Codex history/reset gap; a separate 711-second fixture passed 20 epochs/40,000 records with ordered queued inputs through two forced and one graceful restart. These do not certify billing or overnight provider use. Finish persisted discovery resumption, bounded long-term dedupe/baseline storage, standalone scans beyond the current 16 MiB prefix bound, emitted-byte overhead measurement and sustained installed acceptance. Missing coverage remains unknown and totals are tokens, not money. [Existing usage trials](verification/2026-09-12-integrated-desktop.json), [contract](ARCHITECTURE.md#planned-protocol-and-event-additions). |
| Engineering; #200 pending | Chat-first UX cleanup (September 18) | The person's review of the app found branch-based session names indistinguishable, repeated direct-message and system feeds, temporary fixture projects listed as projects, a huge Earlier list, a raw internal error in Activity, a confusing Board and no obvious reconnect or project terminal. Split three ways: claude-code-74334's stable session names and Messages grouping (#201, merged `57a9d4e`: `app/naming.rs`, tool plus the session's own id prefix, canonical direct-message rows, **From AgentDocker** fold); claude-code-74231's sessions list (#203, merged `16bf69a`: **Reconnect here** on the session row, the Earlier groups paged eight at a time with their own folds, discovered-under-scratch projects in a **Temporary** fold, the Board's copy in plain words, a launch that ends before activation described in words — new and already stored); codex-51242's shared chat first per project and the project terminal (#200, `codex/chat-first-workspace`, integrated with main at `d2461a6`, its combined gate and 31-check native workflow passed locally and it is installed as preview `9e75e834`). Open: #200's macOS-ARM graphical CI job fails deterministically at the Messages thread row in the compact Chat layout (the same scenario passes on Intel macOS and both Linux runners), so #200 waits on that fix and a substantive review; then the person's own review of the result. |
| Implemented; acceptance remaining | A board of work beside the sessions | Implemented and merged in #176, layout #189 and combined #191; installed local preview includes the Board and retained text drafts. Actual card creation and rendered create/pull/lease/move/archive workflows passed. Finish final-candidate coworker usability/physical-input acceptance; task ownership, queue refusal and stored draft boundaries remain enforced. Typed links, roles and webhooks are merged, not future Board implementation. |

Federation/host namespaces, cross-host leases/routing, the herdr focus bridge,
additional adapters, container log following and proposed CLI conveniences are
existing deferred proposals. They are not prerequisites for current single-host
delivery. See [product direction](PRODUCT-DIRECTION.md),
[architecture proposals](ARCHITECTURE.md#planned-protocol-and-event-additions)
and [containers](CONTAINER-ENGINES.md).

## Manual and operational steps

| Step | What remains |
| --- | --- |
| Apple signing/notarization | Supply a Developer ID Application identity and private notary profile, then sign, notarize, staple and run Gatekeeper checks on the final app/DMG. The September 19 read-only check found no valid signing identity. Ad-hoc signing validates a local preview only. |
| Human accessibility/IME trials | Run the hands-on input cases above on the actual candidate and record findings. Automated accessibility controls do not replace them. |
| Completed on this Mac: launcher and coordinator switch | The old launcher problem is closed. The earlier `79981beb`/schema22 switch activated desktop, daemon and receiver from a verified package with backup and all four external provider processes retained; the recorded `d14610b7`/schema 23 switch (September 17, 20:55 UTC) did the same with both live providers and all 128 retained receipts preserved. This does not certify future unattended live transfers. |
| Independent release acceptance | Run second-Mac, physical Intel, target-Linux and sustained actual-provider trials against the final candidate. |

Signing/private credential handling is in [desktop distribution](DESKTOP-DISTRIBUTION.md);
the operational sequence is in [local trial](LOCAL-TRIAL.md).

## Retained evidence and release configuration

Existing verification reports preserve original sources and failed attempts.
The September 18 documentation reconciliation adds exact merged-PR dispositions
to seven older records whose index summaries still said review/CI or installation
was pending. The historical trial fields remain unchanged; these corrections
close stale tracking statements, not untested acceptance requirements.
Later merges close their old integration notes, not every acceptance category.
The [documentation index](README.md#verification-records) lists all retained
reports; the [testing crosswalk](DELIVERY-PLAN.md#testing-standard-crosswalk)
identifies complete and partial categories.

PR #128's terminal timeout/partial-UTF-8 fix passed 100 repetitions of 14 tests;
the original macOS writer failure and separate benchmark socket timeout remain
unexplained. On September 17, #185's macOS gate at `9ca3c5f` timed out
in the same unchanged writer-failure test after 90 seconds (1,171 other tests
passed). Five hundred isolated local repetitions passed without reproducing
the hang; this does not close that failure. A five-minute retention/restart trial at `66c0946` preserved 331
mixed queued inputs through five prune batches and restart after 1,532 journal
notes. Longer retention and overnight fixtures keep their separate source pins;
none establishes current-provider sleep/reboot acceptance.

The September 19 read-only release check still found v0.1.0 as the latest
published release. The earlier September 17 check found only a README in the tap's Casks directory. The September 9 check
found a publishing variable and token secret name; it did not inspect secret
values or establish token validity. Publication must be verified when performed.

Current active Codex/Claude exchanges have exact queue/context receipts in the
existing native queue record. They establish active delivery, not idle wake for
a plain Claude session or universal-provider completion.
