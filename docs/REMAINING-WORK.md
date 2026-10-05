# Remaining engineering and release work

This is the active backlog, not a record of every attempt. Exact source commits,
failed trials, subsequent corrections and raw evidence locations remain in
[the verification index](verification/INDEX.md) and linked PRs. Source,
publication and installed acceptance are separate: use `agentdocker desktop
status` and `agentdocker daemon status` to identify what a machine runs.

Published `0.2.0-beta.7` is reviewed, integrated source `372ba880`. Its 31 hosted
assets, nine archives, five manifests and both preview feeds passed verification.
Actual beta.6 → beta.7 installation/update and schema rollback refusal passed
on Mac ARM64, Oracle Ubuntu and Windows x64. Hosted terminal/graphical and
installed-service checks passed for the exact private scopes indexed below.
Stable `v0.1.0` and Homebrew remain unchanged from the prepublication snapshot.

Beta.7 includes native owner cleanup, stop-priority, explicit retained-input
recovery, MCP website/nonsecret form review, Windows ledger publication and
Mac daemon install-start fixes. Schema 25 prevents binary rollback to beta.6's
schema 23 while preserving the installation. Never restore an older delivery
ledger over acknowledged input; an older-version trial needs isolated state.

**The rollout is incomplete.** Native Codex launch and live daemon replacement
remain experimental. Preview distribution does not establish stable signing,
independent hardware, fresh accounts or physical accessibility. The installed
Oracle accounting preflight passed; its 48-hour trial is running, not accepted.

## Release and first run

| Requirement | Current evidence | Remaining work |
| --- | --- | --- |
| Publish and verify the preview | Beta.7 is published after final local checks, CI and review. All 31 hosted assets and both preview feeds passed; actual beta.6 → beta.7 installation/update and unchanged-installation schema rollback refusal passed on Mac ARM64, Oracle Ubuntu and Windows x64. Windows service trials use the retained beta.6 bootstrap forwarding to verified beta.7. Stable and Homebrew are unchanged. | Keep `channel-preview-windows` separate from the four-target Mac/Linux feed, stable release and Homebrew. Repeat hosted acceptance for later previews; stable signing, fresh accounts and independent hardware remain gates. [Distribution contract](DISTRIBUTION-SETUP.md). |
| First run on every supported platform | Hosted beta.7 passed Mac ARM64/Oracle installation, update/schema rollback refusal, eight terminal and 31 graphical workflow checks; Windows passed its two-release lifecycle/default feed and both installed services. Actual-model Ubuntu Codex delivery and physical AWBP Claude idle/busy/draft receipts exist for their recorded packages. | Test final downloads with fresh configuration and provider authentication, trust and channel consent; idle/busy delivery, preserved drafts, restart and reopen. Existing-account reuse, synthetic providers and CI do not prove fresh-account acceptance. |
| Independent machines and distributions | Oracle is Ubuntu 24.04.4 x86_64 with private graphical dependencies/Xvfb. AWBP is Windows 11 Home build 26200 x64. Local Mac, Intel/ARM CI and Rosetta evidence is separately pinned. | Independent second Mac, physical Intel, normal target Linux desktop prerequisites, physical Linux input and current-candidate Windows service/provider trials. AWBP testing is currently deferred at the user's request. [Trial procedure](LOCAL-TRIAL.md). |
| Stable macOS signing | Previews are ad-hoc signed; Developer ID/notarization is not configured. | Configure private signing/notary credentials, sign/notarize/staple the final app and DMG, and test Gatekeeper on an independent Mac. |
| Mac disk-image contention | Bounded recovery for the exact `hdiutil` resource-busy failure is implemented and reviewed. Subsequent ARM/Intel package runs passed image verification; the original failed run is retained. | Preserve attempt diagnostics and distinguish recovery from first-attempt success. The intermittent runner contention itself is unexplained. |
| Tester reports | Trial issue template #198 is merged. | Record actual app/daemon/runtime versions and reproducible steps, excluding private prompts, paths and credentials. |

## Provider reliability and parity

| Requirement | Current evidence | Remaining work |
| --- | --- | --- |
| Idle delivery and existing sessions | Managed Claude/Codex trials and physical Windows Claude 2.1.280 idle/busy/draft/restart receipts passed for their exact revisions. Claude development-channel consent can recur on reconnect. Plain hooks/MCP alone do not wake a model. | Current hosted-package acceptance, broader runtime/version coverage, restart/reopen and supported adapter parity. [Claude](CLAUDE-CHANNEL-INPUT.md), [Codex](CODEX-INPUT.md). |
| Native Codex startup and identity | Reviewed startup, descendant containment and capability-lifetime fixes have actual Codex/private-model acceptance on Mac, Oracle and Windows for their indexed sources. Explicit existing-entry recovery passed prepared-intent, lost-client/journal-reply and pause/rate-hold trials; possibly transmitted intents remain read-only. Windows `8a249fde` passed both Codex 0.160 and 0.155.1, including draft/replacement/reopen and three recovery scenarios. | Keep native launch experimental pending integrated/current-package, real-account/physical input, desktop adoption, broader versions/startup races, lost-provider replies, Windows journal-boundary faults and unsent drafts through crashes. Recovery never overrides interruption or resends uncertain input automatically. Birth admission requires an owned empty thread; reopen requires persisted history. See [native input](CODEX-INPUT.md). |
| Identity and uncertain delivery | Exact native TUI attribution, detached-helper refusal, lost-output readback and stale-confirmation refusal have bounded regressions/trials. Oracle passed a 90-second question wait with peer/human steering. Reviewed #286 folds only verified helper records into their session. | Broader native version/platform review, longer waits, sustained queues, receiver replacement and uncertain-write recovery. Preserve original IDs, conversation, questions and ownership; never replay consumed input or merge by display name. Managed zero-prompt reopen remains unaccepted; native explicit UUID reopen has the scoped draft evidence above. |
| Project pause/resume | Oracle actual Codex `b334cc70` passed two recipients (busy and idle), four lifecycle receipts/replies in the original project, held/lifted leases and exclusion of an unrelated project. Required local MCP startup prevents the earlier fallback to an account cloud connector; the misplaced replies remain recorded. | Every intended recipient must reach a safe boundary, reply in the original chat and resume across idle, busy and account-limited states. Queued routing alone proves neither pause nor receipt. |
| Throughput, drafts and oversized input | Oracle drained 32 mixed peer/human messages through a held question in 146.88s with ordered receipts and all echoes. Native Mac/Linux normal-PTY trials passed oversized-line refusal followed by exact Unicode-edited input and the 16,000-byte boundary. Active hook input remains bounded to 6,000 bytes at tool boundaries. | Larger sustained queues, other providers and form drafts, authenticated oversized-input turns, physical keyboard/IME and preservation through sleep, reboot and reopen. The bounded throughput result is not low-latency acceptance. |
| Account-limit and sign-in recovery | Bounded 429 trials pass. Published beta.4 retains structured authentication failure when later errors in the same turn are generic; a local 401 fixture plus explicit resume preserved the original receipt and delivered waiting input once. | Actual account reset/sign-in recovery, unrelated replacement identities, more versions/adapters and sustained use. Expose unknown quota scope/reset and pending states without blind replay or automatic model changes. |
| Permissions, stdin and long waits | Native Mac command approval held 65s with ordered input. Ubuntu Codex 0.155.1 passed writeStdin Allow/Deny and a guided 240s hold with exact receipts. Indexed Mac/Linux/Windows URL trials passed four decisions/eight ordinary receipts, including both Windows provider versions; separate five-minute unanswered expiry passed. Native Windows approval used synthetic Return. | Hosted final-package, real-account/browser consent, broader versions, pending-restart and waits beyond five minutes remain. Complete late requests without turn IDs, broader permission/network approval, extended MCP forms/device elicitation and secret input. Preserve human/peer ordering, drafts and no-replay decisions through interruption and uncertain writes. [Input contract](CODEX-INPUT.md). |
| Unprompted coordination and adapters | Neutral-task trials coordinated at Claude/Codex hook and OpenCode plugin boundaries; MCP-only trials allowed edits to a leased file. Goose has no hook point. | Repeat the accepted adapters on the current hosted package and improve routing to MCP messaging. Copilot CLI/Gemini integration and additional adapters remain deferred proposals, not completed parity. |
| Setup and readiness | Preview/apply/undo, guarded CLI undo, named hook diagnostics, shell opt-in and app reconnect exist. Windows actual Claude configuration checks preserved user settings. | Fresh-user/account authentication, trust, consent and delivery. Configuration, adapter contact, receiver attachment and a durable receipt are separate checks. [Guided setup](GUIDED-SETUP.md). |

## Platform and operational work

| Requirement | Current evidence | Remaining work |
| --- | --- | --- |
| Mac daemon service startup | Hosted beta.7 passed ten install/stop/start/restart/repeated-uninstall checks after explicit kickstart fixed the on-demand GUI-domain startup timeout. All three generations independently retired, owned label removed and production connector unchanged. Desktop-file removal correctly refused while an unrelated connector registration exists; the private installation remains retained. | Automatic crash recovery in that GUI domain, login/reboot and independent Mac acceptance. Earlier beta.6 install/crash and socket-export failures remain indexed; later lifecycle passes do not establish crash recovery. |
| Native Windows installer and daemon | Reviewed per-user Task Scheduler startup, bounded crash supervision, protected storage, native pipes/ConPTY, terminal reattachment, installer/update/rollback and loaded-launcher retirement are implemented. Hosted beta.7 passed its two-release update/schema-refusal lifecycle and both installed service trials. | Start menu/PATH, physical console attachment, real provider shims, broader native daemon/CLI suites, logon/reboot and managed-provider survival. Portable acceptance does not prove installed behavior. [Windows contract](WINDOWS-PORT.md). |
| Windows browser connector service | Reviewed setup pins tunnel paths and guards delayed-start ownership under the mutation lock. Published beta.7 passed 24 installed connector and 13 daemon checks after actual beta.6 → beta.7 update/schema rollback refusal; cleanup failures reject acceptance. | Actual tunnels/browser accounts and logon/reboot. Private fixtures do not replace production services. |
| Windows terminal stop and receiver recovery | Reviewed #295 prioritizes pending stop and retries failed writes before reattachment without consuming input. Corrected `c21b90c5` Windows package passed; child exit 2.094s and final state 2.438s in that trial. | Retain bounded in-flight/reconnect delay; no loaded-host wall-clock guarantee. Original slow stop and separate busy-named-pipe diagnostic remain unexplained despite the deterministic starvation regression and later passes. Exact failures, sources and cleanup are indexed. |
| Native receiver exit during recovery | Windows `50efe320` left a daemon-started receiver after owner exit, despite provider exit/capability revocation and unchanged receipts. Source `c59b605a` checks exact provider generation once/second during paused recovery. The old-behavior regression failed; the fix passed with unchanged memory/disk ledgers. Corrected Windows Codex 0.160 package passed every exit probe with no survivors/forced cleanup; daemon-controller logs are now retained. | Broader races, versions and hosted-package acceptance. The original failed report omitted that receiver log, so the 30-second recovery wait is a candidate explanation rather than a demonstrated cause of that particular failure. Historical first-start failures remain separate. |
| Windows Scheduler cold startup | Exact `c3cf464f` passed 143 daemon checks/forty starts, then its first service operation timed out: 17.697 s to the first PowerShell trace left 2.329 s for task lookup. Cleanup found no installed task. The next source allows 60 s per Scheduler script, including interpreter/module startup, with no automatic mutation retry. | Validate native daemon and connector lifecycles on the changed source. Preserve original traces; this budget change does not explain the OS delay or resolve historical daemon first-start failures. Readiness, process-stop and existing fixture deadlines remain unchanged. |
| Intermittent Windows startup | Extracted first-start failures remain unexplained: retained diagnostics reached the coordinator lock but not store/listener readiness. Bounded phase logging, retained failed packages and 40-home sampling exist. Later schema-transaction changes passed separate 40-home trials. | Reproduce and isolate the historical first-start failure; later passes are not a demonstrated fix. Preserve the separate 20-second PowerShell/Task Scheduler timeout and cross-host differences. Receipt-free `daemon status` now skips unrelated service queries, but that correction does not explain either intermittent failure. |
| Token accounting | Collection, resumable discovery, bounded batches and a 256 MiB logical tracking budget are implemented. Hosted beta.6 Mac/Oracle scanned a 303 MB synthetic Claude log with 4,608 exact counters and no restart gaps. The installed Mac capacity trial retained 449,638 records unchanged through restart, reported one coverage gap and kept messaging available. | Installed beta.7 Oracle preflight passed three real Codex messages with exact provider-counter attribution; the 48h/48-message/three-provider/ten-project trial is running, not accepted. Complete it, historical-gap reconciliation, emitted-byte/index overhead and broader formats/large files. Logical tracking excludes SQLite indexes/pages/WAL; only observed versions are supported and gaps remain visible. Capacity fixtures and the separate 48-hour trial do not prove service/sleep/reboot or recover uncounted records. [Accounting contract](ARCHITECTURE.md#planned-protocol-and-event-additions). |
| Browser accounts and service | Scoped OAuth/MCP messaging, multi-project consent, egress/CIMD validation and bounded startup/hourly vendor-list refresh exist. Hosted beta.6 admitted ChatGPT public metadata to pairing and refused an untrusted callback without granting access. Ubuntu passed an hourly refresh/restart/desktop-button tunnel launch; installed read-only identity passed. Service setup preserves differing settings. | Real-account CIMD, correlated reply/ACK, hosted Mac connector login-service and real-account service trials, and additional verified vendor identities. Earlier browser write refusal remains unaccepted; ordinary OAuth does not prove CIMD. Browser connected status requires a tool call within one hour; the connector cannot initiate hosted model turns. [Connector contract](REMOTE-CONNECTOR.md). |
| Linux service removal | Reviewed #289 stops/disables before removal, preserves the definition on stop failure and reloads afterward. Hosted beta.7 passed all 17 Oracle systemd lifecycle checks; six generations independently retired and pre-existing links were restored. | Broader distributions and logon/reboot. The original beta.5 uninstall failure and fixture restoration correction remain indexed; later passes do not erase them. |
| Storage and sustained operation | Activation pruning preserves active/rollback/pinned and possibly service-referenced builds. Reviewed `fcaa22f9` completed 48h on Oracle with three actual Codex 0.155.1 sessions/ten projects, 48 exact receipts, matched counters, zero gaps and 110 watches; cleanup passed. Separate `8778a67f` ten-minute idle observation measured 1.113323% of one core with stable watches/records and six exact receipts. | Selective exact service-reference pruning; multi-day installed-current/accounting trials and CPU/watch/storage behavior through sleep/wake/reboot on all platforms. The 48h run overlapped other work and had provider transport retries without AgentDocker resubmission. Ten-second idle sampling adds load and establishes no CPU threshold. Earlier 24h gaps and controlled SHA benchmark results remain indexed; neither becomes production-idle acceptance. |
| Terminal shutdown and retained failures | The Darwin blocked-receive cause was corrected by bounded poll/nonblocking receive; four 45-minute stress lanes passed and #273 shipped in beta.4. The separate Oracle graphical fixture lifetime error was corrected and rerun against identical hosted bytes. | Final-package/sustained acceptance. Unexplained retained failures: Linux managed-group/owned-child shutdown, controller-upgrade injected-storage assertion, Iced capture, benchmark socket timeout and Linux ARM transport refusal. Diagnostics and later passes do not establish their causes. |
| Watcher limits | Removed-checkout recovery passed repeated regressions and installed recovery with providers unchanged. | Overnight/many-project acceptance of one FSEvents stream per checkout and resource behavior around removal/recreation. |
| Live daemon replacement | Experimental Ubuntu same-binary and actual-provider beta.2 ↔ beta.3 handovers preserved threads/receipts. `8778a67f` passed eight pending-form handovers on each of Mac/Linux with 32 stable seconds between replacements; the earlier rapid reconnect exhaustion remains failed. | Keep `AGENTDOCKER_EXPERIMENTAL_RELOAD` until sustained service handovers, Claude/other providers, attached drafts, other pending questions, output drain and uncertainty recovery pass. Coordinator restart alone is insufficient. |
| Legacy reconciliation | Offline preview/apply/rollback exists. Reviewed #286 uses the provider directory for hooks and reconciles matching named live sessions without losing queued state; refused folds retry each minute. Registration, fold/refusal and one-row UI regressions pass. | Complete installed acceptance on the reporting Mac.  Recompute from a fresh backup only after required nonhuman records end and the daemon stops. Do not stop live providers merely to tidy inventory; current session resumption is separate. [Setup](GUIDED-SETUP.md). |
| Notifications and accessibility | Earlier installed notification routes, unlocked-Mac AX traversal/synthetic Return and widget composition/clipboard regressions pass for their exact builds. | Final-package physical notification clicks, bounded history page-back, broader routes, VoiceOver/supported screen readers, keyboard activation, IME composition, zoom/focus and terminal clipboard. Synthetic input does not establish human acceptance. [UI contracts](ICED-DESIGN.md). |

Cross-host federation/namespaces, cross-host leases/routing, the herdr focus
bridge, additional adapters, container log following and proposed CLI
conveniences remain deferred. See [architecture](ARCHITECTURE.md#planned-protocol-and-event-additions),
[containers](CONTAINER-ENGINES.md) and [local trial](LOCAL-TRIAL.md).

MCP nonsecret form review shipped in beta.7 after PR #294 review, integration
and final CI. The exact final source passed 1,573 Rust/192 Python tests (8/1
skipped), zero retries. Indexed Mac/Linux/Windows actual-provider trials cover
four decision paths/eight receipts, unanswered expiry, rendered Mac UI, spaced
pending-form handovers and private-daemon crash recovery. Provider versions
0.160 and 0.155.1 were tested. Final hosted lifecycle/graphical/service checks
passed in their private scopes. Real accounts, physical input, provider/GUI
crashes, extended/device forms, secret input and broader service handovers remain
open. Original runtime/fixture failures remain indexed; later passes do not
erase them or explain the historical first-start failure.

PR #299 adds owned-thread MCP form/URL review while idle, using ledger 14 and
state schema 26. Local `b096554d` passed the full standard gate: 1,575 Rust/193
Python tests, zero retries. Actual Mac package acceptance passed upgrade from
hosted beta.7/schema25, refusal of rollback and refusal of older-package install,
with unchanged selection and signed executable bytes. No daemon/database was
started in that installer trial. Never restore an older delivery ledger over
already acknowledged input.

The actual Codex refusal control reproduced the previous no-active-turn
rejection; Codex translates it into an MCP decline. Runtime `99a247b0` passed
eight idle form/URL decisions, nine receipts and ten private-model requests on
Mac ARM64/Codex 0.160, then eight pending-review daemon crashes with original
questions, held input and the same provider. Oracle Ubuntu passed all eight
decisions on actual Codex 0.160 and 0.155.1 at precursor `552272e4`, with source,
archive, provider, helper, copied evidence and process retirement independently
verified. Windows `552272e4` passed all eight active/idle/native/recovery scopes;
each idle mode completed its initial turn before external callbacks, held peer
input, and required four explicit decisions/five exact receipts with no early
model call. Those precursor Windows/Linux trials do not prove schema 26. Review candidate `c3cf464f`
Linux CI package acceptance separately passed actual hosted beta.7 → schema 26
installation and unchanged rollback/older-package refusal on Oracle. The final
full local gate also passed on `c3cf464f`. Windows CI aggregate budgets now
cover the eight sequential native/MCP/recovery trials while retaining their
individual deadlines; observed precursor steps took 5 min 53 s ordinarily and
9 min 16 s with services/forty startups.

Final PR CI/review/integration, broader versions and provider/GUI restarts remain
pending. Command/file/permission questions still require their exact active
turn; secret/device callbacks and longer waits remain open. This follow-up is
not included in immutable beta.7.
