# Native delivery and verification plan

Updated September 7, 2026. This is the active delivery plan requested by the user, including a renewed review of recent commits, PRs and all project documentation. The [product direction](PRODUCT-DIRECTION.md) defines the intended product; [the delivery record](NATIVE-DELIVERY.md) records implementation progress. The [review ledger](REVIEW-2026-09-07.md) pins the initial review scope and evidence. This plan is unfinished work, not release certification.

## Product and engineering requirements

- Deliver **agentdocker**, a native desktop app that opens onto currently active local agents and their projects. Native execution is the default. Docker and Podman remain optional adapters. No browser, HTTP listener, cloud account or engine is required for native operation.
- Target macOS, Linux and native Windows. Distinguish source builds, cross-compilation, actual platform execution, graphical acceptance and published downloads. A macOS bundle's technical `.app` suffix is not the product name.
- Keep installed CLIs, desktop applications, running processes, registered agents and verified adapter capabilities distinct. Inventory must not imply message consumption, model context access or permission to stop somebody's existing work.
- Preserve the core's synchronous, time-injected coordination model; move host I/O/environment policy into stateless host helpers. Use one synchronous daemon state mutex, never hold it across an await, and keep asynchronous host preparation outside it. Commit durable state and ordered events together. Review every exception explicitly.
- Change protocol, errors, CLI/MCP/hooks/GUI behavior and architecture documentation together. Leases protect cooperative participants; do not describe them as an OS sandbox.
- Preserve private state, credentials, receipts and raw provider evidence. Bencher credentials/configuration stay outside both the repository and GitHub. Test only owned fixtures and fresh provider sessions until the installation gates pass.

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
| T10 | Latency and resource baselines | p50/p95/p99 request/hook latency, throughput, stale-warning delay/misses/false alerts, restart/handoff time, watcher gaps, fingerprint throughput, SQLite latency, RSS/CPU/FD/disk/log growth. Hook one-second delivery is an existing functional contract. |
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


### Desktop discovery implementation checkpoint

The desktop inventory follow-up separates Codex CLI, Codex desktop and ChatGPT, adds curated Linux desktop-entry inventory with XDG overrides, and adds standard CLI installation locations for native app launches with a minimal PATH. Inventory errors preserve the last GUI rows and connection state; explicit setup is isolated from unrelated invalid launchers. Focused fixtures cover hidden overrides, special/oversized/malformed files, symlink exports, non-executed launcher declarations and distinct integration attribution. Full integrated acceptance and target-distribution trials remain required for L03; this does not close Windows desktop inventory or universal provider support.


### Desktop maintenance implementation checkpoint

Local uninstall/retention now has reviewed CLI and GUI plans, conservative
legacy/service retention, payload identity checks and running-release lifetime
locks. Focused tests cover removal scope, resumability and lock contention.
L12 still requires packaged real-process acceptance and final-source platform
checks; download/update distribution and safe live daemon transfer remain open.
