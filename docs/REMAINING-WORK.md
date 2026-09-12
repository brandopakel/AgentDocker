# Remaining engineering and release work

Updated September 12, 2026. This is the current backlog. Dated audits and
[verification reports](verification/) retain the implementation history,
source-specific results and failed trials.

## Delivered and awaiting integration

PRs #98–#114 have merged after final CI and actual source review; the latest
merge is `71f2b31`. Delivered source includes compact Current/Needs input/History
views, identity-safe discovery and offline duplicate repair, native notification
routing, durable queues, opt-in Claude/Codex idle input, shared human/peer
submission, structured questions and receipts, and update checking/downloads.
The installed launcher and running daemon have not been replaced.

| Candidate | Verified locally | Integration still required |
| --- | --- | --- |
| [Turn-scoped Codex permission review](verification/2026-09-12-permission-review.json), PR #115 | 856 Rust tests, 65 Python checks, actual Allow/Deny with three ordered mixed inputs each, and 18 native review/draft/migration steps. Concrete paths and network access are shown; unknown/conflicting grants are refused. | Final CI passed at `70408ab`; actual source review remains. Broader permission forms are separate below. |
| [Correct CLI sender identity](verification/2026-09-12-cli-sender-identity.json), PR #116 | 859 Rust tests, 65 Python checks, six prior/corrected process scenarios and actual Claude Code 2.1.270 Bash trials. Omitted send/ask/answer/cancel identities use the exact registered provider, preserving the human's question route. | Final CI passed at `359c0b6`; actual source review and integration remain. This does not add idle wake to an existing hook-only session. |
| [Simpler home and helper filtering](verification/2026-09-12-ux-home.json) | 863 Rust tests and 65 Python checks. Home groups sessions by project, limits attention to three short previews, opens exact questions and preserves drafts/selection. The known Claude Chrome native host is excluded from discovery. Native and actual-provider reports are linked in the evidence. | Final CI, source review and integration remain. The 100-agent comparison measured higher UI CPU (about 6–7% versus 4–5%); profiling is in progress. The installed build still needs the safe switch. |

The [30-minute actual Codex trial](verification/2026-09-12-thirty-minute-codex-queue.json)
passed 60 ordered human/peer inputs and exact replies, seven read-response cuts,
one retained controller/conversation and normal cleanup. Profile and binary
hashes were unchanged. It does not establish overnight, sleep/reboot or live
replacement acceptance.

## Engineering and acceptance still open

| Priority | Work remaining | Completion condition and evidence |
| --- | --- | --- |
| Top | Complete provider input and review handling | Finish broader permission forms, MCP elicitation and secret-input presentation; test further actual-provider interruptions, uncertain writes and sustained conversations. Preserve one queue/order and exact receipts for human and peer input. Managed Claude channels and the owned Codex bridge have bounded idle/busy/question acceptance; hooks alone cannot wake an idle model. See [message audit](MESSAGE-DELIVERY-AUDIT.md), [Codex input](CODEX-INPUT.md) and [Claude question evidence](verification/2026-09-12-claude-question-queue.json). |
| High | Verify notification clicks in the installed app | Source removes the AppleScript fallback and implements destinations plus native existing/cold-window routing. Complete actual Notification Center clicks, signed posting and installed-launcher acceptance, including expired targets and retained drafts. See [notification audit](NOTIFICATION-ROUTING-AUDIT.md). |
| Next | Safe live daemon replacement | Retain child/process-group ownership, exact exit status, PTY/pipe I/O, logs, identity and leases; fence all writers/autostart; establish actual successor readiness and recover from failed transfer. Reconcile uncertain writes without blind replay. `daemon reload` remains unavailable. Event continuation, provider event reconnect, output drain and read-only queue retry are merged prerequisites, not completed replacement. See [replacement design](LIVE-DAEMON-UPGRADES.md). |
| Next | Apply legacy reconciliation to production state | Offline proof-based repair exists. Complete an actual production inventory/preview and apply verified pairs with backup/recovery after the old daemon can be stopped safely. Do not merge records by display name or stop a live process to tidy the list. Current/History separation and helper filtering improve presentation independently. |
| Acceptance | Sustained use and unresolved failures | Complete overnight, sleep/reboot, broader queue/retention and checkout workloads. A [one-hour 100-agent/10,000-file daemon trial](verification/2026-09-11-hour-sustained-use.json) passed 1,392,836 cycles; it does not cover the remaining cases. Diagnose the retained incomplete Iced capture and historical socket timeout: later passing trials do not explain them. See [testing standard](TESTING-AND-BENCHMARKS.md) and [local trial](LOCAL-TRIAL.md). |
| Release | Publish verified downloads and updates | Consumer, opt-in daily scheduler and release archive/feed automation exist. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication and actual update/rollback acceptance. Cargo registry publication is not an established supported route. See [distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md) and [setup](DISTRIBUTION-SETUP.md). |
| Platform | Linux and independent Mac acceptance | Complete target-distribution desktop/service/package trials, a second Mac and physical Intel acceptance. ARM64/x86-64 graphical/package CI and Rosetta execution do not establish those hardware results. See [local trial](LOCAL-TRIAL.md). |
| Platform | Full native Windows product | Integrate daemon/clients with named pipes; finish supervision, ConPTY, identity-safe stop/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and native graphical CI. Existing core/host/desktop adapters are foundations. See [Windows port](WINDOWS-PORT.md). |
| Input | Hands-on accessibility and input methods | Exercise VoiceOver and supported-platform screen readers, keyboard navigation/activation, visible focus, zoom, IME, Unicode and terminal copy/paste. Range selection has [actual macOS acceptance](verification/2026-09-11-terminal-selection.json); broader human trials remain. Repair observed defects. See [Iced contracts](ICED-DESIGN.md). |
| Installed acceptance | Removed-checkout watcher recovery | Source ignores vanished checkout roots and reports lost coverage; independent macOS checkout streams fix a reproduced surviving-file event loss. Both regressions passed 100 repetitions. Verify after the launcher/daemon switch; historical conflict channels remain. See [watcher evidence](verification/2026-09-11-macos-watcher-recovery.json). |
| Later | Optional expansion | Authenticated federation/host namespaces, cross-host leases/routing, additional adapters and container capabilities remain behind a dependable single-host desktop. See [product direction](PRODUCT-DIRECTION.md) and [containers](CONTAINER-ENGINES.md). |

## Manual and operational steps

| Step | What remains |
| --- | --- |
| Apple signing/notarization | Supply a Developer ID Application identity and private notary profile; run signing, notarization, stapling and Gatekeeper checks on the final app/DMG before publication. Packaging automation exists; ad-hoc signing only verifies a local preview. The September 12 identity check found no valid signing identities. |
| Human accessibility/IME trials | Run the input trials above and record findings on the actual candidate. Automated control/accessibility checks do not replace them. |
| Switch the old launcher after sessions finish | Verify package and installation preview, account for provider/service paths, then activate and check app/CLI/daemon versions with rollback available. A launcher update affects future launches and does not upgrade the running daemon. The old daemon lacks the live-transfer protocol. |
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

A September 9 read-only GitHub check found the Homebrew formula at v0.1.0,
the publishing variable and token secret name configured, and only a README in
`Casks/`. The tap exists; publishing a newer verified release and app cask remains.
That check did not inspect secret values or verify token validity.
