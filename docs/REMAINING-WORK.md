# Remaining engineering and release work

Reconciled September 11, 2026 against this checkout's implementation and project
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
- Suppress known Codex interpreter launchers when their native child is present. This
  addresses a discovery path found in code, not a demonstrated second live
  registration on this Mac. Suppress transient discovery/registration overlap
  only with matching known PID and birth time; retain PID reuse and unknown cases.
- Extend unit and native workflow coverage for history separation, attention,
  process identity evidence and narrow-window navigation.
- Keep successful channel sends visible across navigation and inbox refreshes;
  group incoming messages by their actual channel destination. The bounded
  receipt cache is partial history from this window, not a durable transcript.
- Retain Claude's visual cleanup and plain message text, raise secondary-text
  and primary-button contrast, and reset session controls when forgetting a project.

Source changes take effect in rebuilt binaries. They do not replace the installed
launcher or the daemon hosting existing sessions.

## Engineering delivered in this pass

- Codex prompt, tool-completion and Stop hooks deliver bounded inbox context and
  acknowledge only after successful output. Fresh actual-provider trials cover
  all three boundaries and correlated peer replies. Live coordination was also
  exercised with the user's independently launched Claude session.
- Schema 9 persists pending questions and their original expiry. Message fanout,
  question creation/closure and ordered events now commit together. Restart
  trials use real daemon crashes and refuse an incompatible downgrade without
  changing the newer state.
- Startup now refuses ambiguous duplicate live names before recovery writes.
  It no longer arbitrarily retires a record and releases its protection. This
  prevents a destructive recovery path; it does not merge legacy identities.
- Added sustained-use and restart drivers with exact executable/driver hashes,
  private fixtures and owned-process cleanup. Sustained campaigns snapshot the
  daemon so concurrent builds cannot invalidate the executable mid-trial.
- Added verified feed generation and made Homebrew publication depend on uploaded
  release assets. Native graphical/package CI now covers ARM64 and x86-64 on both
  macOS and Linux; all four jobs passed the schema-9 implementation checkpoint.
- Fixed benchmark selection to use Cargo's emitted executable paths and verify
  their hashes before/after workloads. A custom target directory previously left
  the runner pointing at potentially stale `target/release` binaries. This does
  not establish the cause of the older retained socket timeout.
- Implemented notification destination metadata, native response handling and
  forwarding to the window for the correct daemon origin. Removed the macOS
  AppleScript fallback and preserved drafts during navigation. Release/native
  click trials remain open in the notification audit; the installed launcher is
  unchanged. Bundled Inter font licenses now accompany Mac and Linux packages.

Schema 10 now retains addressed messages during streaming and rejects full inboxes without silent eviction. Its full standard gate passed 701 Rust tests (six skipped), 48 Python checks and lint/package/release gates. Actual-daemon trials passed reconnect/crash recovery, over-limit schema-9 migration, atomic full-recipient rejection, byte pressure and downgrade refusal. Provider input/wake adapters remain open below.

The opt-in Claude channel adapter now has actual idle/busy/mixed-input evidence
at clean source `c9677ab`: four model receipts and correlated replies, six
release-transport scenarios, 114 native workflow steps and 23 notification
navigation steps. The standard gate passed 715 Rust tests and 48 Python checks.
The trial preserved an unsubmitted terminal draft. Three global Claude usage
counters changed during concurrent use; the failed whole-file guard and narrower
backup comparison are retained in the [report](verification/2026-09-11-claude-channel-input.json).

The updater now checks the feed, verifies and previews its archive, and applies
through the existing installation pins. Its CLI and native Settings path have
isolated fixture evidence. Release automation now prepares installable archives
for four native targets and a verified stable/preview feed before publishing a
draft release. Real signing, hosted-release and update-download acceptance remain.

[PR #98](https://github.com/brandopakel/AgentDocker/pull/98) merged as `aca89e1`
after all CI passed and CodeRabbit reviewed `f9caf00` with no new actionable
findings. This completes the engineering and review gate for
[proven offline duplicate repair](IDENTITY-REPAIR.md), including transactional
removal of canonical records and old-ID routes. Validation reached 756 Rust tests
and 54 Python checks. Ambiguous identities and live/managed ownership transfers
remain refused; applying a repair to installed user state still requires a
quiescent maintenance window. No production registry repair was performed.

## Engineering still open

| Priority | Work | Completion condition | Supporting documents |
| --- | --- | --- | --- |
| Top priority; Claude partial acceptance | Unified user/agent input queue and idle wake | The opt-in Claude adapter and managed launch passed actual idle, mixed-sender and terminal-draft cases; the earlier adapter trial also covered busy input. Complete compact durable delivery status, actual-provider reconnect/ambiguous receipt and sustained conversations. The [Codex app-server trial](verification/2026-09-11-codex-appserver-input.json) proved idle start and busy acceptance, but repeating the same client message ID created another turn. A [real-daemon recovery prototype](verification/2026-09-11-codex-queue-recovery.json) preserved mixed-sender FIFO, recovered one accepted input from exact thread/turn/item history without resending, and refused automatic replay of uncertain input. Implement this in the owned native bridge with visible delivery state; lifecycle hooks alone do not wake idle Codex. | [Message delivery audit](MESSAGE-DELIVERY-AUDIT.md), [Claude input guide](CLAUDE-CHANNEL-INPUT.md), [active delivery plan](DELIVERY-PLAN.md) |
| High priority; routing implemented, physical acceptance open | Notification clicks open blank Script Editor | Native posting, destination metadata and existing/cold-window navigation are implemented; the AppleScript fallback is removed in source. Complete actual Notification Center click trials, signed posting and installed-launcher acceptance while preserving drafts and handling expired targets. The running old installation still needs the safe switch. | [Notification routing audit](NOTIFICATION-ROUTING-AUDIT.md), [active delivery plan](DELIVERY-PLAN.md) |
| Next | Safe live daemon replacement | Pending questions now retain answer routing across restart, with atomic message fanout and closure. Full replacement still must preserve child ownership, batch/PTY I/O, logs, identity, leases and schema compatibility; require the actual successor to be ready before retiring its predecessor, with failure recovery. `daemon reload` deliberately returns unavailable today. | [Architecture](ARCHITECTURE.md#sessions-and-persistence), [delivery plan](DELIVERY-PLAN.md) |
| Partial acceptance | Sustained-use bounds and unresolved performance failures | A stable schema-9 checkpoint passed ten minutes each at 1/10/100 agents (255,891 cycles); the final package passed actual crash/schema-upgrade and distinct-source installation/rollback trials. Hours/overnight, actual-provider queues, reboot/sleep, growth/retention and broader checkout workloads remain. Diagnose the retained socket timeout; a fresh passing diagnostic campaign does not explain it. | [Current verification](verification/2026-09-10-desktop-delivery.json), [testing standard](TESTING-AND-BENCHMARKS.md), [local trial](LOCAL-TRIAL.md) |
| Release; consumer and producer implemented | Download/update distribution | CLI and Settings update check/download/preview/apply exist with local fixture evidence. Release automation prepares package.py archives and verified stable/preview feeds. Complete protected-tag signing, hosted archive/feed downloads, formula/cask publication, target Linux acceptance and scheduled checks. Registry Cargo publication is not an established supported route. | [Desktop distribution](DESKTOP-DISTRIBUTION.md), [release automation](RELEASE-AUTOMATION.md), [distribution setup](DISTRIBUTION-SETUP.md) |
| Platform | Linux delivery acceptance | ARM64/x86-64 Linux and Mac graphical/package CI, including update-consumer scenarios, passed checkpoint `d630d9f`. Target-distribution desktop/service/package trials and independent hardware acceptance remain gates. | [Current verification](verification/2026-09-10-desktop-delivery.json), [product direction](PRODUCT-DIRECTION.md), [local trial](LOCAL-TRIAL.md) |
| Platform | Full native Windows product | Integrate the daemon and clients with named pipes; finish supervised lifecycle, ConPTY, identity-safe stopping/recovery, provider/desktop inventory, user service/session behavior, installer/update/rollback and native graphical CI. Core/host/desktop adapter coverage is only a foundation. | [Windows port](WINDOWS-PORT.md), [architecture](ARCHITECTURE.md) |
| Range selection accepted on this Mac; broader input trials open | Terminal selection and richer interaction | Range selection/copy retains a bounded visible-grid snapshot, preserves Unicode and wrapped text, and releases it after copy or resumed input. The [selection checkpoint](verification/2026-09-11-terminal-selection.json) passed 762 Rust tests, 58 Python checks, 114 native workflow steps and an actual macOS drag/copy/changed-output trial with clipboard restoration. Complete human accessibility/IME and other-platform input trials, and repair observed defects. | [Iced contracts](ICED-DESIGN.md), [desktop guide](DESKTOP-UX.md) |
| Acceptance | Removed-checkout conflict fix in installed app | Source now ignores filesystem events for vanished checkout roots and reports lost coverage, with a reproduced regression and actual macOS watcher test. Verify after the safe launcher/daemon switch; existing historical conflict channels are retained. | [Bulk receipts and watcher evidence](verification/2026-09-10-bulk-receipts.json) |
| Implemented; PR gate pending | Concurrent provider configuration mutation | Canonical-target locks now coordinate guided apply/undo, legacy setup and hook installation across AgentDocker homes. The [configuration checkpoint](verification/2026-09-11-provider-configuration.json) passed 765 Rust tests, 58 Python checks, 114 native steps and six actual CLI contention/recovery scenarios. Complete final CI and review. Independent provider CLIs/editors remain outside these advisory locks; exact-entry checks and receipts still guard ownership. | [Guided setup](GUIDED-SETUP.md), [delivery checkpoints](DELIVERY-PLAN.md) |
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

The [September 10 delivery verification](verification/2026-09-10-desktop-delivery.json)
records the later code checkpoint `bf39280`: 686 Rust tests (six skipped), 48 Python
checks, lint/package/release gates, 105 packaged native workflow steps, actual
Codex delivery, crash recovery and distinct-source installation/rollback. Its Mac
preview is 24.8 MiB installed and 10.6 MiB zipped. The report separately pins the
immutable 30-minute sustained-use checkpoint and the fresh diagnostic benchmark;
neither proves idle-agent wake or completes the remaining engineering table.

The [button-interaction follow-up](verification/2026-09-10-button-interaction.json)
records code checkpoint `5772736`: 687 Rust tests (six skipped), 48 Python checks,
the full standard gate and 105 fresh packaged native workflow steps. Primary
labels retain the tested contrast during hover and press in both themes. The
packaged CLI/daemon hashes match the preceding delivery/crash-recovery trial;
the UI has its own new binary and workflow evidence. Notification click routing
and provider idle wake remain open requirements.

The [schema-10 checkpoint](verification/2026-09-10-durable-queue.json) pins clean source `a9b54b5` and matching immutable binaries: 701 Rust tests, 48 Python checks, five actual queue scenarios, seven restart/upgrade checks, 105 native workflow steps and 23 notification-navigation steps passed. Provider input acceptance/idle wake and physical Notification Center clicks remain distinct open gates.

The [receipt follow-up](verification/2026-09-10-message-receipts.json) pins `f81df24`: MCP reads retain messages by default, agents explicitly acknowledge received IDs, and the desktop can dismiss one received message without disturbing later arrivals or drafts. Validation passed 704 Rust tests, 48 Python checks, real MCP interruption/receipt scenarios, 110 native workflow steps and 23 notification-navigation steps. Provider idle-wake integration remains open.

The [bulk-receipt checkpoint](verification/2026-09-10-bulk-receipts.json) pins `796270a`: Dismiss shown clears only currently displayed received messages; unanswered questions appear once, CLI output exposes receipt IDs, repeated acknowledgements emit no false events, and legacy duplicate channel membership produces one delivery. Validation passed 710 Rust tests, 48 Python checks, actual queue/MCP trials, 114 native workflow steps on a diagnostic repeat, and 23 notification-routing steps. The original idle-sample process exit is retained as unexplained; high UI resource use also needs investigation. These results do not complete provider idle wake or physical notification-click acceptance.

The [update and release checkpoint](verification/2026-09-11-desktop-release.json)
pins clean source `a910d81`: 727 Rust tests, 54 Python checks, strict lint/package
gates, 114 packaged native steps and 11 packaged updater scenarios passed.
Malformed versions, ambiguous feeds, archive links and failed downloads now have
explicit rejection coverage. Release automation produces installable archives
and keeps older/preview releases from moving stable distribution backwards.
The earlier large light-theme shadow allocation was corrected; measured native
resource samples are retained separately from long-duration acceptance. Public
signing, hosted downloads, scheduled checks and safe daemon transfer remain open.
