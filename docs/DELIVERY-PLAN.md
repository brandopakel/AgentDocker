# Native delivery and verification plan

Updated September 11, 2026. This is the active delivery plan requested by the user, including a renewed review of recent commits, PRs and all project documentation. The [product direction](PRODUCT-DIRECTION.md) defines the intended product; [the delivery record](NATIVE-DELIVERY.md) records implementation progress. The [review ledger](REVIEW-2026-09-07.md) pins the initial review scope and evidence. This plan is unfinished work, not release certification.


For the September 9 desktop cleanup and a consolidated distinction between open
engineering, acceptance and manual release work, start with
[Remaining work](REMAINING-WORK.md). The dated checkpoints below retain historical
evidence; an old “pending” entry is not by itself a current implementation gap.

## Product and engineering requirements

### Submitted-input parity and idle wake (September 10)

The user requires peer messages to follow the same provider input workflow and
queue as messages they submit themselves, including waking an idle agent. Make
the [message delivery audit](MESSAGE-DELIVERY-AUDIT.md) a top-priority part of
delivery step 4 and L09/L13. Trace every queue/notification/acknowledgement boundary,
then implement supported per-provider input/wake adapters, busy and mixed-input
ordering, backpressure, retry/deduplication and restart recovery. Verify with
actual Claude/Codex idle conversations. Hooks that only run on another lifecycle
event do not complete this requirement, and an inbox acknowledgement does not
prove provider acceptance. The audit document defines the required artifacts and
negative-path acceptance cases.

The first queue correction is in source: schema 10 retains addressed messages while subscribed and rejects count/byte pressure without evicting accepted work. The full standard gate and actual-daemon reconnect/crash, mixed-sender, upgrade/downgrade and atomic-fanout trials passed. This closes neither provider acceptance nor idle wake; both still require the adapters and actual-provider trials above.

The [September 11 Claude adapter checkpoint](verification/2026-09-11-claude-channel-input.json)
adds actual idle wake and ordered peer/user delivery while a tool waits, including
a queued terminal prompt and preserved unsubmitted draft. Four actual model
receipts/replies and six release-transport scenarios passed at `c9677ab`. A
whole-file profile guard failed; exact backup comparison isolated changes to
three Claude usage counters, with no provider settings/authentication changes.
Visible provider receipt state, Codex input/wake, actual-provider
reconnect and sustained acceptance remain. The full standard gate passed 715
Rust tests and 48 Python checks; native workflows passed 114 steps and routing
passed 23 steps.

The [managed Claude launch checkpoint](verification/2026-09-11-managed-claude-input.json)
at `78fc835` adds an explicit desktop option and CLI `run --claude-channel`.
Actual Claude 2.1.268 received its first input while idle without any typed model
prompt, and a canonical-user follow-up preserved a terminal draft. Both messages
were explicitly acknowledged and replied to under the original managed identity.
Standard validation passed 735 Rust tests and 54 Python checks; the UI recheck
passed 106 tests and the separate visual trial passed 26 steps. These close the
bounded managed-launch item; provider recovery/status and sustained acceptance
remain open. CI for the preceding `d630d9f` checkpoint passed all four native
desktop targets, Windows foundations, container engines, coverage and benchmarks;
CodeRabbit reviewed `d630d9f` and reported three findings: schema downgrade checks without an activation record, priority receipt batches and explicit inbox-acknowledgement success. Fixes are now in the working tree; final validation and follow-up review remain.

The update consumer is implemented in CLI and Settings with local preview/apply
evidence. [Release automation](RELEASE-AUTOMATION.md) now prepares the installable
archives and complete feed. Signed protected-tag publication, hosted update
verification and scheduled checks remain release work.

The [reviewed update checkpoint](verification/2026-09-11-desktop-release.json)
at `a910d81` passed 727 Rust tests, 54 Python checks, 114 packaged native steps and
11 packaged updater scenarios. Workflow lint passed. The update smoke now also
runs in all four desktop CI jobs; its synthetic version exercise does not replace
distinct-source or hosted-update acceptance.

### Legacy duplicate repair (September 11)

[Offline identity repair](IDENTITY-REPAIR.md) now has a read-only preview and an
exact-plan apply transaction under exclusive database ownership. It preserves
accepted messages and original history, records before-images, and exposes
former-ID routes to the desktop. Proven external local Claude/Codex pairs are
supported; live and managed transfers remain refused. [Recorded validation](verification/2026-09-11-identity-repair.json) passed 753 Rust
tests, 54 Python checks and six actual CLI/restart steps. The clean packaged
checkpoint passed 114 native workflow steps, 23 routing steps and 11 updater
cases. The later database-key refusal guard passed the full gate and another
source-pinned CLI trial. CI and actual final-head review remain.

### Notification clicks open Script Editor (September 10)

The user reports that notification clicks repeatedly open a blank Script Editor
window. Add the [notification routing audit](NOTIFICATION-ROUTING-AUDIT.md) as a
high-priority usability defect alongside submitted-input parity. Trace the
installed sender and native-post failure, replace or constrain the AppleScript
fallback, carry stable destination IDs, and implement native activation/navigation
for existing windows and cold launch. Acceptance requires actual notification
clicks to reach the correct project, agent, message or question while preserving
drafts. Verify preview and signed builds separately; developer-program payment
does not implement the missing click handler. Keep signing/notarization and
provider idle wake as distinct gates, and suppress unintended fixture notices.

Implementation now carries destination/origin metadata, removes the macOS
AppleScript fallback, handles native responses, and forwards activation into an
existing window or starts the destination origin. Navigation preserves drafts
and includes an older target in the visible transcript window. The
[audit](NOTIFICATION-ROUTING-AUDIT.md#implementation-in-the-current-change) records
implementation and trial limits; the installed-app defect remains open until
actual native clicks and candidate acceptance pass.

### Active-session defects and coordination trial (September 7, evening)

The user's live desktop trial now includes Codex and two Claude Code sessions
using agentdocker across AgentDocker and memkv. Add these blocking acceptance
cases to L03/L06/L08/L13, alongside the existing delivery work:

- Correct false idle status. A live adopted Codex PID had no activity reports
  since adoption, despite ongoing work. Installed MCP configuration is not an
  active session connection. Add explicit, expiring provider activity reports
  and Codex lifecycle hooks; distinguish unknown activity from observed idle.
- Unify integration identity. Verified duplicate Claude rows have identical
  PIDs and process birth times: hooks plus MCP in AgentDocker, hooks plus
  adoption in memkv. Reuse the same live identity across adapters without
  conflating PID reuse, distinct provider sessions, worktrees or projects.
  Preserve existing inboxes, leases and history; do not stop live providers to
  make duplicate rows disappear. Include MCP disconnect/reconnect and daemon
  restore in regression coverage.
- Exercise real cross-agent delivery with the user's authorized Claude Code
  peer. Record queueing, adapter consumption, reply receipt and acknowledgement
  separately. A successful send or review request is not evidence the model
  read it. Do not drain another live session's inbox as a test.
- Verify channel membership, scoped messages, review requests, comments,
  approval/changes verdicts and explicit closure. Pending lane proposals remain
  pending until the other agent answers; an open channel or contested-path
  count is not proof of accepted work ownership or a resolved conflict.
- Make delivery and review state understandable in the native GUI. Investigate
  how missing Codex wiring, hooks/MCP duplicate identities and pending reviews
  affect routing. The memkv channel cited by the user is read-only diagnostic
  context; implementation changes remain in AgentDocker.
- Complete Codex message injection/acknowledgement at supported lifecycle
  boundaries, distinct from the new activity-only hooks. Test queued proposals
  through actual model consumption and a correlated reply. Never count a
  configured MCP server, activity report or successful queue write as that proof.

Live peer coordination is authorized by the user. Source changes still use
separate worktrees/branches; provider restarts, message loss and arbitrary work
in other projects are not test cleanup. Raw messages/configuration remain
private; publish only sanitized scenario results with exact source evidence.

- Deliver **agentdocker**, a native desktop app that opens onto currently active local agents and their projects. Native execution is the default. Docker and Podman remain optional adapters. No browser, HTTP listener, cloud account or engine is required for native operation.
- Target macOS, Linux and native Windows. Distinguish source builds, cross-compilation, actual platform execution, graphical acceptance and published downloads. A macOS bundle's technical `.app` suffix is not the product name.
- Keep installed CLIs, desktop applications, running processes, registered agents and verified adapter capabilities distinct. Inventory must not imply message consumption, model context access or permission to stop somebody's existing work.
- Preserve the core's synchronous, time-injected coordination model; move host I/O/environment policy into stateless host helpers. Use one synchronous daemon state mutex, never hold it across an await, and keep asynchronous host preparation outside it. Commit durable state and ordered events together. Review every exception explicitly.
- Change protocol, errors, CLI/MCP/hooks/GUI behavior and architecture documentation together. Leases protect cooperative participants; do not describe them as an OS sandbox.
- Preserve private state, credentials, receipts and raw provider evidence. Bencher credentials/configuration stay outside both the repository and GitHub. Test only owned fixtures and fresh provider sessions until the installation gates pass.

## Lightweight native operation and storage cleanup

The user's September 7 requirements make local storage and memory efficiency
release gates. agentdocker runs natively without mandatory engines, SDKs or a
browser server. Source and sanitized test/benchmark evidence belong in GitHub;
compiled intermediates and disposable trials are pruned after their evidence is
published. Credentials, private agent state and personal documents stay private.

The immediate audit includes the remaining macOS System Data and the reported
10 GB Documents category. Measure actual directories and distinguish build
caches, duplicate packages, temporary checkouts, test/engine state, logs,
application support, personal files and OS-managed data. Categorization alone
does not authorize deleting a file. Remove confirmed regenerable or owned
disposable output, preserve active processes and user work, and record allocated
bytes plus actual free space before/after. Do not merely transfer the same
accumulation into another local folder. Retain the report in GitHub and only
small necessary private evidence locally.

Add these checks to the existing T04/T10/T11/T12 and L12/L14/L15 matrices:

- Installed/compressed bytes per platform. The three-binary Mac preview
  at `509f746` occupies 36.9 MiB; its compressed archive is 13.5 MiB. Its
  running-release pin fix passes nine packaged maintenance scenarios, using
  synthetic generations of the same binaries. This is local preview evidence;
  distinct-source updates and release acceptance remain open.
  Start with a 100 MiB per-architecture payload ceiling and measure universal
  artifacts separately; lower budgets when platform evidence permits.
- Idle and loaded daemon/window RSS, CPU, thread and descriptor counts at
  1/10/100 owned agents. Repeat sustained runs, identify leaks and unbounded
  growth, and set measured platform budgets. The cache incident proves a
  development-storage problem; a runtime RAM leak has not been established.
- Limits, retention and recovery for logs, SQLite/event/journal/watcher records,
  snapshots, temporary checkouts, package staging and rollback copies. Cleanup
  must preserve active references and user work and recover space after failures.
- Sanitize exact-source results and all failure evidence before uploading to the
  repository or GitHub Actions artifacts. Exclude credentials, user paths,
  transcripts, screenshots/frames and provider data; keep raw diagnostics private.
  Confirm publication, then prune local generated
  outputs. Preserve the current small package only while acceptance needs it.
  Prefer CI for broad platform matrices; one local campaign runs at a time.

This audit and cleanup precedes further heavy local testing. Correctness, real
integrations, GUI, full platform parity and live-upgrade work continue afterward.

## Delivery sequence

| Step | Work and completion condition | Current state |
|---|---|---|
| 1. Renew the engineering review | Review recent merged work and every open PR, its commits, review threads, tests and docs. Identify overlapping stacks and regressions at their integrated head. Record every finding with a reproducible failure or explicitly label it an unconfirmed lead. | Started; scope and initial findings in the review ledger. Continue for every subsequent commit. |
| 2. Finish correctness and privacy | Restore must wait for durable protection and serving/watcher readiness. Failed persistence must not leave an uncontrolled writer or remove protection prematurely. Children must use their owning daemon. Close the process-spawn/database crash boundary and investigate retained unexplained failures. | Restore/privacy, inbox and launch-context fixes merged in #46/#52/#56. Pre-exec launch gating and atomic native-exit cleanup merged in #60. Its signal/error review fixes remain in the follow-up branch; credential event durability is in #66. Crash/reboot timing still needs acceptance. |
| 3. Complete native install and onboarding | Verify app/archive packaging, final signatures, preview/apply/undo, connection diagnostics, stable provider paths, pinned installation/update/rollback, uninstall and retention. Exercise interrupted activation and schema compatibility with live sessions. | #48/#49/#51 merged; integrated installation passed its local gate and native Mac window. #53/#64 merged; the runtime-attribution and distinct unverified inventory follow-ups are tracked in the review ledger. Public signing/notarization, update distribution and longer upgrade acceptance remain. |
| 4. Verify real integrations and discovery | Run fresh Claude Code hooks and Codex MCP sessions on the integrated candidate; prove actual inbox consumption, observation/staleness, conflicts and journal continuity. Expand desktop identities, installation locations and accurate capability reporting. | Bounded older-source provider trials passed; broad versions, fresh candidate and longer sessions remain. |
| 5. Deliver platform parity | Finish native Windows host/IPC/process/terminal/service/path/installer adapters and runtime CI. Complete Linux desktop inventory/packages and target-distribution GUI/service tests. Repeat the same semantic tests on each OS. | Linux x86-64 graphical CI exists. Windows #54/#55 merged after native core/host and named-pipe runtime CI; full native daemon/GUI/terminal/service/installer work remains. No Windows release claim. |
| 6. Integrate sustained-use features | Review #45/#47/#50 restart/backoff/dependency/policy/retention/reload work, including the reproduced agent termination and log-loss defects. Preserve batch and PTY I/O, process ownership, logs, schema and socket compatibility through replacement. Validate the actual replacement binary and successor readiness before the old daemon exits. | #50 merged, but actual reload still killed batch/PTY fixtures. Merged #59 refuses that unsafe operation and preserves the formerly unmerged #58 restart fix. Full live transfer remains to build. |
| 7. Complete the extensive test program | Execute the testing-standard and local-trial crosswalk below, repair failures, retain original failure evidence, and rerun affected integrated scenarios. | Partial evidence exists; the entire matrix has not passed. |
| 8. Install, trial elsewhere and release | After the preceding blockers pass, install the reviewed candidate on this Mac; then independent second-Mac and platform trials. Publish signed artifacts, checksums and accurate installation instructions for supported channels. | Isolated local previews only. Public v0.1.0 predates this work. Federation remains later. |

Steps can progress independently where their prerequisites allow. A later feature does not waive an earlier correctness or platform gate.

## Commit, PR and documentation review procedure

1. Pin `main`, release source, each open PR's actual head/base, and any relevant uncommitted content identity. Review individual commits and the resulting combined diff; do not infer inclusion from a PR title or closed status. Preserve other active worktrees and use isolated review branches.
2. For each change, trace product intent through implementation, protocol/events/persistence, CLI and adapters, GUI, tests and docs. Check cancellation, authorization, physical resource aliases, stale evidence, error propagation, ownership and crash ordering. Inspect dependency/API assumptions against their primary sources when necessary.
3. Read CI failures, original local failures and CodeRabbit threads, including findings on earlier revisions. Record accepted findings and evidence-backed dispositions. Require checks and review coverage for the final head. A rate limit leaves review pending; it does not count as approval.
4. Check overlapping #45/#47/#50 and #48/#49/#51/#53/#54 stacks against current main before combining them. Preserve schema meaning, startup readiness, lease lifetime, native IPC compatibility and strict test-runner settings during conflict resolution.
5. Review every project document: root README and coding instructions; docs index, product direction, architecture, implementation notes, engine docs, testing standard, local trial, native delivery, desktop distribution, guided setup, integration acceptance and runner/Windows notes and the new user GUIDE when integrating #50; CLI help; container test README; packaging/install/release scripts and workflow claims. Keep dated audits as historical evidence and link their follow-up instead of rewriting their original results.
6. Fix discovered gaps on owned branches with a failing-before/passing-after test where the behavior warrants one. Update the appropriate user-facing docs in the same change. For work requiring unavailable hardware or credentials, record the exact remaining acceptance step; continue all independent implementation.

Each finding records: ID, invariant, source/PR, reproduction and original output, impact, fix commit, focused and full verification, review disposition, platform limits and remaining work. Each evidence record names source SHA plus dirty content if any, OS/architecture/tools, workload and fixture, command, result, artifact location and cleanup outcome. Private logs and credentials are never included in public reports.

## Testing-standard crosswalk

This checklist implements every category in [TESTING-AND-BENCHMARKS.md](TESTING-AND-BENCHMARKS.md). Existing tools/jobs are starting points; each row still needs evidence on the release candidate and applicable platforms.

| ID | Required work | Evidence to retain / current gap |
|---|---|---|
| T01 | Standard gate: formatting, strict Clippy, nextest, separate doctests, installer/package checks, release build | Exact-head local and CI records. Zero retries, flaky failure and 500 ms leak failure remain. The documented Darwin runner workaround must not mask child leaks. |
| T02 | Deterministic correctness and meaningful Proptest reference models | Claim/renew/release/expiry/finish, physical aliases, cancellation, deadlock/fairness, stopping writers, stale read sets, durable ordering and image/source-bound validation. Audit model coverage and commit minimized failure seeds. |
| T03 | Coverage-directed tests | Inspect cancellation, authentication/revocation, migration, restore and error/cleanup branches. Link LCOV and actual added scenarios; coverage percentage alone cannot close a gap. |
| T04 | Criterion and private Bencher comparison | Lease, fingerprint, SQLite write and recovery-query benchmarks against the actual base with verified manifests. Separate machines/engines as testbeds; no GitHub credential setup. Review trend variance before blocking thresholds. |
| T05 | Native IPC contention and load | 1/10/100 clients; disconnects, slow readers, full pipes, contention, restart and handoff. Existing contention/release benchmark includes connection setup. Add missing dedicated stale/restart latency workloads and Windows named-pipe equivalents. |
| T06 | Bounded fuzz campaigns | Protocol/resource/path decoding, metadata parsers and token filters. Current scheduled targets cover only part of this scope. Retain crashes and minimized regressions with nightly/source identity. |
| T07 | Concurrency model selection | Inspect new synchronization algorithms; use Loom only if an extracted in-memory algorithm warrants it. SQLite/OS ordering needs real integration fault tests. k6 remains conditional on an actual supported network transport. |
| T08 | Independent engine acceptance | Docker and Podman builds, lifecycle, crash recovery, scoped auth, image-bound validation, checkout/socket mappings; separate Linux jobs and actual macOS Docker Desktop/Podman VM trials. Record engine/image/source versions and stop only fixture resources. |
| T09 | Crash and failure injection | Before/after SQLite commit and spawn; failed/slow output and storage; lost watcher; expired/revoked token; unavailable engine; timeout and surviving descendant. Assert no unprotected/duplicate writer or invented durable success. |
| T10 | Latency and resource baselines | p50/p95/p99 request/hook latency, throughput, stale-warning delay/misses/false alerts, restart/handoff time, watcher gaps, fingerprint throughput, SQLite latency, RSS/CPU/FD/disk/log growth. Hook coordination/output has a one-second deadline; input and activity have separate phase budgets. |
| T11 | Workload breadth and soaks | 1/10/100 agents; small/medium/large checkouts; cold and warm runs separately; repeated baselines, hours then overnight. Performance initially advisory, correctness blocking. No sustained-use claim from a short test. |
| T12 | Evidence and cleanup | Source/dirty identity, compiler/tools, OS/CPU/architecture, workload/engine/image, originals of failures and every attempted diagnosis, JUnit/coverage/BMF/fuzz artifacts. Verify owned descendants and fixture daemons exit; retain needed failure state privately. |

## Local-trial crosswalk

Execute [LOCAL-TRIAL.md](LOCAL-TRIAL.md) in order. Every Stage 2 row below requires actual scenario evidence, including negative paths; existing unit tests are supporting evidence only.

| ID / stage | Scenario | Acceptance still to record on the integrated candidate |
|---|---|---|
| L01 / 1 | Isolated build and launch | Matching CLI/daemon/GUI binaries from a pinned source, private state/socket/repository, standard gate, safe fixture-only ownership and cleanup. |
| L02 / 2 | Startup and GUI | Normal/CLI/repeated launch, reopen, unavailable socket, long/missing/symlinked home; responsive native window, automatic refresh and no required TCP or engine throughout the trial. |
| L03 / 2 | Inventory and discovery | Idle CLI and desktop app, process start/change/exit/PID reuse, scan failure, app without shell PATH; preserve last good scan and distinguish actual capabilities. |
| L04 / 2 | Managed processes | Batch/PTY input/output, attach/detach/resize, no-newline/noisy output, natural exit and stop/force-stop including descendants; preserve leases until verified exit. |
| L05 / 2 | Coordination | Two writers through physical aliases, shared readers, FIFO waiting, timeout/disconnect and deadlock; actual blocker attribution. Include Windows spelling/case/reparse behavior. |
| L06 / 2 | Working state | Read/observe/edit/stale/reread, watcher outage and gaps, journal cursor/reconnect; stale data cannot be accepted as fresh. |
| L07 / 2 | Handoff and validation | Source/image changes, timeout/survivors, checkpoint, addressed acceptance and lease transfer, wrong recipient and cross-host import; evidence and identity must match. |
| L08 / 2 | Channels and contests | Membership, review/approval, wrong owner/checkout validation, reported metrics, ties/noise floor; no implied automatic merge. |
| L09 / 2 | Human interaction | Questions/answers, timeout/disconnect, missing/denied notifications, GUI answer failures; delivery failure cannot imply the user saw a notification. |
| L10 / 2 | Multiplexers | Owned existing tmux session and new pane, invalid combinations, name collision and immediate exit; exact session and clear ownership/log limits. |
| L11 / 2 | Storage and restart | Graceful stop, forced death, write failure, missing executable/cwd, reboot/sleep/wake; no duplicate/unprotected launch, explicit stop retained. Exercise unresolved spawn/commit gap. |
| L12 / 2 | Installation and update | Move/rename bundle, distinct-source update, rollback/uninstall, service preview, collision/tamper/stale input, schema and interrupted activation; stable references and explicit live-session behavior. |
| L13 / 3 | Fresh actual providers | One Claude Code hooks session then one Codex MCP session; inspect preview, scoped config change/undo, registry, consumed inbox, stale read, lease conflict and journal continuity. Record runtime versions and original failures; configuration health is insufficient. |
| L14 / 4 | Installed sustained use | After blockers and Stage 2 pass, explicit installation, on-demand startup first, hours then overnight; sleep/wake, login/logout, app closure, daemon crash and planned upgrade separately. Vendor context resumption needs separate proof. |
| L15 / 5 | Second Mac and other systems | Same candidate and independent registry on second Mac; Intel hardware acceptance distinct from Rosetta. Linux target distributions/ARM64/x86-64 graphical and service trials. Native Windows equivalent after full implementation; WSL is Linux evidence. |

## Exit criteria

A feature is complete only when its invariant, relevant UI/adapter behavior, docs, tests and final-head review agree. Release readiness additionally requires the integrated candidate to pass the applicable crosswalk, actual supported-platform trials and final artifact verification. Keep unsupported platforms/channels and unresolved failures explicit. Do not turn an unavailable certificate, machine, review quota or missing test into a completed checkbox.


### Identity review and adapter lifecycle checkpoint

Source review found that `project::try_canonical` accepts nonexistent suffixes,
so using its success as proof of an existing checkout was incorrect. Registration
now uses filesystem canonicalization and checks for a directory. A missing path,
ordinary file or symlink cycle is rejected without creating an agent, durable
row or event. The alias test no longer deletes a fixed path beside its temporary
directory; both target and alias are owned fixtures.

The [packaged lifecycle report](verification/2026-09-07-identity-lifecycle.json)
records the earlier MCP-disconnect failure and passing `242cd3b` candidate with
real adapter processes. The fixture is now part of Mac/Linux desktop CI. It
tests transport shutdown/reconnection, retained identity/lease/inbox and hook
delivery/acknowledgement/SessionEnd; broader provider versions, existing duplicate
migration and final-source review remain open.

### Desktop discovery implementation checkpoint

The desktop inventory follow-up separates Codex CLI, Codex desktop and ChatGPT, adds curated Linux desktop-entry inventory with XDG overrides, and adds standard CLI installation locations for native app launches with a minimal PATH. Inventory errors preserve the last GUI rows and connection state; explicit setup is isolated from unrelated invalid launchers. Focused fixtures cover hidden overrides, special/oversized/malformed files, symlink exports, non-executed launcher declarations and distinct integration attribution. Full integrated acceptance and target-distribution trials remain required for L03; this does not close Windows desktop inventory or universal provider support.


### Desktop maintenance implementation checkpoint

Local uninstall/retention now has reviewed CLI and GUI plans, conservative
legacy/service retention, payload identity checks and running-release lifetime
locks. Focused tests cover removal scope, resumability and lock contention.
L12 still requires packaged real-process acceptance and final-source platform
checks; download/update distribution and safe live daemon transfer remain open.

### Disk-pressure incident and build retention

The local review accumulated roughly 220 GiB of temporary Cargo output and left
only 3 GiB free. Cleanup recovered 185 GiB while preserving source, credentials,
reports and release binaries. Build storage preflight and disabled development
incremental compilation address recurrence; one local campaign and at most two
retained debug caches are now required. T12 records disk use and cleanup as well
as process cleanup. The active main checkout belongs to another session and was
preserved. See the testing standard for scope and configurable limits.


Storage audit checkpoint ([pinned verification record](https://github.com/brandopakel/AgentDocker/blob/705677497c08679818b95d27892b5b785942e2ba/docs/verification/2026-09-07-local.json)):
the first temporary-cache pass recovered 185.21 GiB;
once the main checkout's tests finished, idle debug output recovered another
79.43 GiB under its existing Cargo lock. After byte-verifying the sanitized
22-campaign report on GitHub, completed temporary build outputs recovered a
further 20.39 GiB. The measured cleanup sum is **285.03 GiB**. Source changes,
private credentials, reports and running release binaries were preserved.
Project documentation totals **1.6 MiB**, not 10 GB. Other projects' raw recovery
databases, personal files and application state require separate retention
review; Apple storage-category labels alone are not deletion evidence.


### Runtime history and publication follow-up

The window now caps its visible journal at 200 entries and console scrollback
at 256 KiB, preserving a UTF-8 tail and bounding retained string capacity. A
journal snapshot carries its durable project head so a late reply preserves
newer live entries while a post-prune empty snapshot removes older rows. Older
daemon responses retain their previous snapshot semantics. These are window
history bounds; they do not delete the durable project journal.

The main GUI now admits at most 32 queued commands, coalesces duplicate snapshot
refreshes, and buffers at most 64 replies/events. Hidden-window logic drains
bounded batches; a full queue reports rejected user actions and preserves answer
drafts. Command recall retains up to 100 complete commands within 64 KiB.
Terminal input buffering, repeated platform resource baselines and soaks
remain open T10/T11/L14 work. Count/scrollback bounds do not establish a total
runtime memory ceiling, especially for large protocol payloads.
The storage guard's JSON and error output exclude private cache paths; raw
compiler/provider logs still require sanitization before publication.

### Native resource and cleanup checkpoint

The [exact-source report](verification/2026-09-07-native-resources.json) records
`509f746`: 569 Rust and 37 Python tests, nine packaged maintenance scenarios,
the actual Mac window, and two short native resource observations. The first
observed 0/1/10/100 owned sleeping processes; the second observed 100 after a
10-second warmup. Both removed all 100 owned agents and their disposable state.
No TCP sockets were observed on the fixture daemon/window during sampling.

At 100 agents in the second observation, daemon RSS was 20.3–20.7 MiB and
window RSS 108.4–109.2 MiB. Across the 24.5-second sample interval, process CPU
time averaged 0.49% and 5.23% of one core respectively. The first trial's live
state peaked at 4.4 MiB, including SQLite WAL; after clean shutdown the entire
fixture occupied less than 0.7 MiB before removal. The sleeping children add
their own memory; summed RSS is not unique physical memory. These are short
observations on one Mac, not provider-load, leak, CPU-budget or soak acceptance.
GUI CPU tuning, terminal queue/frame bounds and the remaining platform matrix
stay explicit T10/T11/L14 work.

Documentation measured 1.7 MiB and the actual Documents directory 11.3 MiB.
Neither directory measurement is an accounting of Apple's System Data or
Documents category. Previously recorded cleanup recovered 285.03 GiB; later
completed storage/GUI review caches removed another 4.36/5.21 GiB of allocated
output. Those allocation figures are separate from measured volume free-space
changes. This campaign publishes sanitized evidence before pruning its cache
and obsolete package previews. Raw captures and small failure diagnostics stay
private; personal and unrelated application state is preserved.

After byte-verifying the report on GitHub, this campaign removed 6.60 GiB of
allocated build output under the existing Cargo locks and another 248 MiB in
five obsolete package previews. Its worktree's cache is now 8 KiB, consisting
of cache markers and stable lock files. One 50.4 MiB app-and-archive candidate
remains for the next isolated acceptance tests. Private JUnit, failure logs and
package identity records were preserved; no source or personal data was removed.

The review scope also includes newly merged #76 (`7d43ca6`) and open #77
(`5ee565a` at inspection), which are not included in this measured candidate.
Homebrew naming/payload/retention must agree with managed installation before
release. A child-disown foundation does not complete live daemon transfer.

### Terminal resource and lifecycle follow-up

PR #79 adds terminal admission (32 messages / 64 KiB), visible whole-input
rejection, a 256 KiB response-frame bound and a 64 KiB OSC control-string budget
across frames. Close wakes both directions and refuses a late connection after
detach; background workers finish without polling an idle input queue. Original
tests at `6cb3431` reproduced unbounded message/byte admission and a surviving
late connection on both macOS and Linux. The Windows installation-lock fixture
now creates state with the same private-directory API as installation; both
previously failing Windows tests passed in that campaign.

At `8230d4b`, the corrected terminal implementation passed Linux/macOS standard
CI, coverage, Windows foundations, both engines, both desktop package/graphical
checks and benchmarks. Its tested merge tree and artifact hashes were verified;
eight baseline and eight corrected result files reached private Bencher. The
[source-bound report](verification/2026-09-07-terminal-resources.json) records
those outcomes and a 100-agent Mac observation: 0.36%/0.48% of one core and
20.8–21.0/101.9–102.4 MiB RSS for daemon/window over 24.75 seconds. The profile
mostly shows waiting, with some agent-table layout. This short observation does
not explain the older higher CPU reading or establish a sustained budget.

Actual packaged PTY acceptance then exposed two more completion defects:
attached session handles kept their own output sender alive after child exit;
the CLI's blocking stdin read could prevent shutdown even after receiving End.
The follow-up uses weak output handles and independently reopened nonblocking
terminal input. A real CLI/daemon PTY campaign now joins both desktop CI jobs,
covering no-newline/Unicode/ANSI output, initial size, acknowledged SIGWINCH,
noisy output, detach, bounded replay and exit with the keyboard kept open.
These fixes need their own final-source CI and local acceptance before L04 can
advance. This campaign does not automate GUI keystrokes or prove provider use.

At `865b054`, the packaged Mac/Linux PTY campaigns and local Mac trial passed all
eight scenarios. Linux standard CI, coverage, Windows foundations and engines
also passed. An older Mac bulk-adoption fixture inherited nonblocking accepted
sockets; its explicit blocking-mode correction now needs final-source CI.
The separately dispatched `8230d4b` benchmark passed but varied materially from
the first run; retain the full comparison before setting performance thresholds.

Add concurrent #80 (`4354c23`) to the review/integration gate: bound its dedicated
console/setup queues and installation worker count, preserve existing admission
feedback and cleanup, and prove delegated MCP apply/undo preserves an entry
changed after preview or installation. Retain viewport diagnostics while
investigating its occlusion evidence against the original screenshot failure.

Retain the original failures. GUI CPU tuning, transport deadlines
for saturated connection establishment, terminal screen-dimension budgets,
repeated load/slow-reader/soak trials and platform parity remain open. The other
local campaign exceeded the 40 GiB registered-cache budget during this work;
heavy builds for this follow-up run on GitHub, with no new local Cargo target.

### GUI integration and active-session coordination follow-up

The #78 integration preserves the bounded terminal and reply queues from
#79/#81 while incorporating #80's native window and setup work. Console, setup
and installation each have one worker and four queued jobs; command admission
also caps retained allocations at 64 KiB. Closing the window cancels queued
jobs without joining subprocesses on the UI thread. Deterministic fixtures
exercise saturation, independent progress, ordering, cancellation and rejected
controls. Final integrated CI and graphical/resource acceptance remain required.

The activity follow-up in #82 adds explicit provider reports and an unknown
state when observations expire. Real cross-session channel replies demonstrate
routing, but queue acceptance alone does not demonstrate provider delivery,
model consumption, reply or review. The delivery review must distinguish each
step, add native message/channel visibility, and test Codex delivery at supported
hook/tool boundaries. No assertion that an idle model wakes automatically is
supported by the current MCP adapter.

Duplicate identity work in #83 requires known process birth, runtime, physical
checkout and compatible session identity, including MCP-first/hook-A/hook-B
registration and transport reconnection. Empty inboxes and no held leases do
not prove a duplicate record has no live references. Existing transports,
channels, waits and read sets must survive reconciliation, or retirement must
be deferred. A separate verified checkout binding is needed when an agent's
registered launch directory differs from the worktree used by its tools.
The actual live coordination trial remains private; only sanitized findings
and source-bound fixture results belong in GitHub.

The integrated #78 head `57403c2` and #82 head `fa38e1c` passed the full GitHub
standard, coverage, packaged Mac/Linux graphical, maintenance and PTY, engine,
Windows-foundation and performance checks. These are CI gates, not completion
of the provider or sustained-use matrix. The fresh Codex 0.153.4 trials consumed,
echoed and journaled a synthetic MCP message with one identity, but first invoked
no hooks under their configuration isolation. A clean provider home produced
callbacks and exposed an exact-path comparison rejecting macOS checkout aliases.
Preserve those failed trials; canonical checkout comparison and the documented
interrupt timeout are follow-up fixes requiring their own candidate acceptance.

At `8611292`, those fixes passed the full CI matrix and real packaged-provider
trials. Codex 0.153.4 emitted nine working observations and one idle observation,
consumed a synthetic MCP message, replied and journaled it. Claude emitted five
working observations and one idle observation and wrote its hook-delivered
message token to a fixture file. Both retained one identity in their fresh
fixtures, left provider configuration unchanged and removed owned descendants.
These trials do not exercise the still-open #83 duplicate reconciliation or
prove automatic Codex inbox injection. The [sanitized report](verification/2026-09-07-provider-activity.json)
preserves all six preceding Codex attempts, exact package provenance, CI and
eight privately uploaded Bencher results.

Two local graphical attempts on that package connected, inventoried 14 runtimes
and discovered the fixture, but timed out obtaining a frame. Their matching
renderer logs report `Occluded`; the OS reports a locked session. Visible local
acceptance awaits an unlocked display. The driver now preserves explicit
`RUST_LOG` so the documented renderer tracing command works. Keep these failures
separate from the passing CI captures and historical undiagnosed timeouts.

New main work #84/#85 adds channel visibility and distinguishes a person's
queued inbox from a transcript. Their integration and a request-contract fix
are recorded below. #86 defines daemon handover messages and descriptor transfer
tests; actual reload still refuses. Neither that scaffold nor message queue
acceptance completes safe upgrades or model delivery. Review of pushed #83
`853dfd5` still finds separate session-row/event commits and missing physical
workdir comparison at registration; a concrete transaction patch and failure
test were sent to the authorized peer. Approval remains pending actual fixed
source and verification.

Integration of main through `d4fc464` found a broken channel request introduced
by #85: an empty project with no member is rejected by the daemon. The exact
request reproduced `Invalid` on the live daemon without changing state. The GUI
now queries explicit unique projects from its registered-agent snapshots,
preserves other projects when one refreshes, ignores departed-project replies,
and coalesces channel/inbox refreshes. Wire-request, multi-project and burst
regressions are added; integrated CI must validate this source. Channel/inbox
response byte limits and durable message history remain separate open work.

#86 adds handover protocol scaffolding while keeping reload refused. Its
readiness timeout currently bounds individual blocking reads rather than the
whole message, and suppresses timeout-setting errors. Absolute deadlines and
descriptor/identity mapping validation remain required before production use;
these findings were sent to the peer owning handover. No live-upgrade acceptance
is implied by the scaffold's tests.

Source review of #83 `38a047a` confirms the atomic session transition and empty-
label fixes landed. The supplied normalization helper can still fall back to an
unverified path, and a task failure silently discards the supplied directory.
Fallible physical binding, an event-insert rollback test and a real MCP detach/
reattach trial remain required; the peer is implementing the source follow-up.

The integrated #82 head `e008831` passed standard/coverage, graphical packages,
maintenance/PTY, engines and Windows-foundation CI, but its disjoint 100-client
benchmark failed reading a release response with errno 11 at the existing
five-second read timeout. The other five scenarios completed. The [original
failed campaign](verification/2026-09-07-integration-benchmark-failure.json)
retains exact source and partial measurements; no successful rerun or passing
correctness suite diagnoses it. An opt-in timing campaign now separates slow
state-lock acquisition from slow store operations, with bounded diagnostic
output. Keep benchmark acceptance open until the failure is understood.

The separate opt-in campaign at `ef3fd7b` completed all six socket workloads
with no state timing records at or above 250 ms. Its source-bound
[diagnostic report](verification/2026-09-07-state-timing-diagnostic.json) records
matching manifests and eight successful Bencher reports under a distinct
`github-ubuntu-x86_64-state-timing` testbed. This did not reproduce the original
`e008831` timeout and does not close its diagnosis or performance acceptance.

### Delegated setup ownership follow-up

Review found that runtime-name matching allowed undo to remove changed command,
argument or environment settings, malformed JSON was treated as absence, and a
durable `created` flag could claim an unrelated registration after interruption.
New receipts pin the complete planned entry and a per-plan environment marker.
Apply/undo preflight checks delegated state before file writes; recovery and undo
require exact entry ownership. Old receipts without evidence preserve present
entries. Add requires the exact entry afterward; remove requires actual absence.
Regression cases cover changed fields, malformed shapes, interrupted intent,
legacy receipts and misleading provider-command postconditions.

An isolated invocation of the installed Claude CLI verified `mcp add --env`
preserves the marker in a stdio entry and that remove clears it; the user's
configuration hash stayed unchanged and the fixture was removed. This interface
check does not validate the new compiled setup implementation. Final-source CI
and packaged onboarding acceptance remain required. Independent edits racing
inside a provider command and consistent support for `CLAUDE_CONFIG_DIR` across
inventory, health, hooks and saved provider commands remain open.

### Capture recovery after a failed surface acquisition

At `d06a117`, both standard Rust gates, Linux desktop acceptance, benchmark,
engines and Windows foundations passed; macOS graphical acceptance failed at
60 seconds. Its [source-bound failure](verification/2026-09-07-macos-capture-failure.json)
records early occlusion after the capture request and a visible/unoccluded
viewport at timeout. Pinned dependency source shows capture requests are dropped
on failed surface acquisition. The fixture now focuses before capture, requires
known visibility, and permits at most four capture requests, each subsequent
request requiring a newly reported surface failure. It retains the deadline and
real screenshot requirement. Final-source graphical CI must validate recovery;
local acceptance still awaits an unlocked display.

### Consistent Claude profile routing

The alternate-profile gap identified during delegated undo review is implemented:
`CLAUDE_CONFIG_DIR` now selects the same MCP file, hooks file and configuration
inventory in the host helpers used by setup and health checks. Saved delegated
steps retain an absolute profile directory and apply it only to the provider
child; default-profile steps explicitly clear an inherited override. Tests
cover profile setup/health/undo with an unrelated invalid default configuration
and child-only environment changes. Final-source CI and an actual packaged
provider-CLI preview/apply/undo trial remain required. Concurrent writes to the
same provider entry still lack compare-and-swap semantics.

The first profile-routing candidate (`4860e96`) passed Linux standard and both
packaged desktop jobs, but the Mac suite passed 638/639 tests: the older
default-profile assertion expected `/var/...` instead of the newly pinned
physical `/private/var/...` directory. The assertion now resolves the owned
parent directory before appending the provider file name. The new profile
apply/health/undo regression itself passed on Mac. The original CI failure
(run `34195349392`) remains retained; final-source CI is required.

The real packaged provider-CLI [profile setup trial](verification/2026-09-07-claude-profile-setup.json)
passed at `a65d956`, including eleven public CLI invocations with two expected
refusals for changed/malformed configuration. The repeatable driver is
`scripts/claude_setup_smoke.py`. Exact package source/tree, Git-blob input hash,
archive and executable hashes were verified; only the 8.2 MB CLI was extracted.
The fixture profiles were removed and monitored user configuration hashes stayed
unchanged. This closes the bounded configuration trial for that candidate,
while actual model delivery, concurrent provider mutation and final integrated
acceptance remain separate.

The standalone Channels correction #89 was merged as `43b9021` after all CI
checks and CodeRabbit's final review of `7376ed7` reported no remaining concrete
merge-readiness risk. Source merge does not update an already installed app.

### Combined delivery candidate

A single review candidate combines the bounded native desktop stack (#78),
provider activity (#82), retained benchmark failure/diagnostics (#88), delegated
setup ownership (#91), capture recovery (#92), Claude profile routing and its
packaged trial (#93), reviewed main Channels fix (#89), and identity lifecycle
with directory/fixture corrections (#83/#90). Original branch results remain
historical evidence; the combined source must pass fresh applicable CI. No
production daemon or installed GUI is replaced by assembling this candidate.

Integration review found that Claude activity still looked up the hook-derived
name after MCP could own the canonical record name. Activity now uses the
verified session lookup. A named registration fast path also now verifies PID,
process birth, runtime, checkout and session before reusing that record; a stale
name cannot bypass daemon registration. The packaged identity trial additionally
requires prompt Working and Stop Idle on the MCP-first identity, plus Stop and
SessionEnd lease release. Both identity and PTY lifecycle jobs remain in desktop
CI. Actual final integrated model trials, duplicate migration, automatic Codex
inbox delivery, sustained-use bounds, safe live upgrades and platform/release
acceptance remain open. One complete final review of the consolidated source
can cover its component changes; skipped and rate-limited reviews cannot.

The first combined head `2b76e46` failed test compilation because the main merge
retained an identical older Channels regression beside its bounded-queue version.
The obsolete copy is removed, preserving all assertions in the bounded version.
The [original failure](verification/2026-09-08-final-candidate-compile.json) is
retained; fresh corrected-source CI remains required.

### September 8 integrated acceptance and continued input audit

At `10e3067`, all applicable build/test CI jobs passed, including Mac/Linux
packaging, graphics, installation, PTY and joined identity activity. Fresh actual
Codex MCP/activity and Claude hook/activity trials each retained one identity and
consumed a unique fixture message. Eight verified benchmark artifacts were
uploaded to Bencher. [Source-bound evidence](verification/2026-09-08-integrated-provider-trials.json)
retains the first Claude trial's privacy failure: a monitored provider
configuration changed, while the old harness incorrectly returned success.
The corrected trial isolates `CLAUDE_CONFIG_DIR`, retains before/after hashes
privately and fails on changes. It passed without any monitored user configuration
change. No user file was restored. Future real Claude trials must use this
isolation. The original benchmark timeout remains unexplained; this short trial
does not close soaks, live upgrades, platform/signing or the complete test matrix.

Further T09/L13 audit reproduced the Claude hook waiting past 1.5 seconds on an
empty, still-open stdin pipe before its coordination timeout even started. The
[original packaged reproduction](verification/2026-09-08-hook-input-baseline.json)
is retained. Claude now shares Codex's 1 MiB, one-second absolute input reader.
A trickling writer cannot renew its deadline; invalid input fails open with a
diagnostic. Packaged Mac/Linux CI tests held-open and oversized input for both
adapters without a provider or daemon. Corrected-source verification remains
required. This is an input phase bound, not a total bound across all hook phases.


### September 10: pending-question restart recovery

Pending questions now persist their answer route with the original message.
The inbox fanout, sender activity and question open/close events share one
SQLite transaction; failed writes publish no partial inbox, live message or
notification. A disconnected requester can read its answer after restart, and
concurrent replies close a pending question once. Expired routes are removed
durably without withdrawing old inbox messages. Schema 9 prevents an older
daemon from silently ignoring these routes. Restart, duplicate-answer, expiry
and injected storage-failure regressions cover this change. Live child/PTY/log
ownership transfer remains unfinished; `daemon reload` stays unavailable.

### September 10: visible-message receipts and checkout removal

[Checkpoint `796270a`](verification/2026-09-10-bulk-receipts.json) adds explicit batch dismissal of shown messages, retains unseen messages and drafts, removes duplicate pending-question presentation, exposes CLI receipt IDs, and ignores repeated/unknown receipt events. Removed temporary checkouts no longer masquerade as competing edits in the reproduced classifier and actual macOS watcher trials. The full gate passed 710 Rust tests and 48 Python checks; queue/MCP, 114 native control steps and 23 routing steps passed. The first GUI idle-sample exit remains unexplained, and high UI resource use remains under investigation. Supported provider idle-wake adapters are the next implementation task.

### September 11: final review and Codex input acceptance

At `bfa7c7a`, deletion of a reconciled identity commits its canonical record, old-ID
routes, inbox, cursor and removal event together before changing daemon memory.
Fault injection covers each write stage. The Windows identity fixture now uses a
native absolute path. The [review follow-up](verification/2026-09-11-identity-repair.json)
passed 756 Rust tests, 54 Python checks and the full package/release gate. The
preceding Windows CI failure is retained; final-head CI and review remain required.

The [actual Codex app-server probe](verification/2026-09-11-codex-appserver-input.json)
accepted idle peer/human input and mixed steering in one active turn. Replaying
the same client message ID started another turn. The owned input adapter therefore
needs durable attempt tracking and an explicit uncertain-acceptance state before
retries; this prototype does not close queue integration or provider recovery.
