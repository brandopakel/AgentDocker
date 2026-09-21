# Testing and benchmarking delivery plan

Standardized September 5, 2026 at the user's request. Apply throughout the correctness, working-state, recovery and Docker/Podman workstreams. Tool installation and configuration do not constitute test coverage: each addition must exercise a concrete product invariant.

## Selected stack

| Priority | Tool/service | Purpose in AgentDocker |
|---|---|---|
| First | cargo-nextest + GitHub Actions | Isolated Rust test execution, timeouts, resource groups and JUnit artifacts. Keep doctests separately. Report flaky outcomes; retries must not hide correctness failures. |
| First | Criterion + Bencher | Measure lease operations, content fingerprints, SQLite writes and recovery queries; retain benchmark trends and compare PRs against their actual base. Criterion measures; Bencher stores and evaluates results. |
| First | Proptest | Generate claim/renew/release/expire/finish sequences and aliases; compare with a simple reference model. Persist minimized failing seeds. |
| First | cargo-llvm-cov | Find untested cancellation, authorization, migration and recovery branches. Publish coverage artifacts; use coverage to guide meaningful tests rather than target a vanity percentage. |
| First | Native Rust Unix-socket load harness | Exercise the real JSON-line protocol under concurrent clients, disconnects, slow readers, lease contention and daemon restart. Export Bencher Metric Format results. |
| Next | cargo-fuzz | Fuzz protocol decoding, resource/path normalization, bounded metadata parsers and token request filtering. Use a separate nightly fuzz job and retain crashing inputs as regressions. |
| Selective | Loom | Model extracted in-memory synchronization algorithms if finer-grained concurrency is introduced. It does not model SQLite or filesystem/OS behavior and is not a drop-in replacement for daemon integration tests. |
| Transport-dependent | k6 | Use for supported network endpoints when present. Evaluate a maintained extension before using the native Unix-socket protocol; a bridge benchmark measures the bridge too. Do not add a production HTTP API solely to accommodate k6. |
| Integration | Real Docker and Podman jobs | Shared engine contract scenarios and separate real-engine results for builds, lifecycle, mount translation and scoped authentication. Linux CI first, explicit macOS VM checks. |

Bencher reporting is configured privately for this project, and verified main benchmark artifacts have been uploaded. The user requires its key and configuration to remain outside the repository and GitHub. Local runs and downloadable CI artifacts work without credentials. k6 remains optional for future network transports; it is not a reason to introduce a production HTTP endpoint.

## Behavioral gates

Every PR runs formatting, strict Clippy, unit/integration tests and installer/package checks. New tests target: no overlapping exclusive physical leases; no post-cancellation/exit acquisition; stopping writers retain protection; durable effects have correct event ordering; checksum failure preserves installation; observed stale input requires reread; accepted recovery survives restart; source or image changes invalidate matching validation evidence. Exercise crash points before/after SQLite commits, full/slow output pipes, lost watchers, expired/revoked credentials and engine unavailability using test-owned processes and fixtures.

The scheduled workflow runs bounded protocol, resource-key, engine-metadata and token-filter fuzz campaigns. Docker/Podman protocol jobs run on PRs and main pushes. Repeated concurrency soaks, large-checkout latency workloads and the full desktop/OS lifecycle matrix are still required trial work, not existing scheduled coverage. Failed seeds, logs, JUnit, coverage and benchmark outputs are retained with the exact commit and platform. CodeRabbit reviews implementation and test changes; green automated checks and disposition of valid review findings are required before integration.

## Measurements and thresholds

Record p50/p95/p99 request and hook latency, throughput, stale-warning delay, missed stale detections and false alerts, restart/handoff recovery time, watcher queue gaps, fingerprint throughput, SQLite write latency and process memory. Use workloads with 1/10/100 concurrent agents and small/medium/large fixture checkouts, with cold and warm runs separated.

Every result includes commit SHA, dirty-content identity if applicable, Rust/tool versions, OS/architecture, CPU, workload parameters and container engine/image identity when used. Different machines and engines are different Bencher testbeds. Establish repeated baselines before selecting regression thresholds; shared-runner timing is initially advisory. Correctness invariants are immediate hard failures. Promote performance checks to blocking only once measured variance supports the threshold; the hook's coordination/output phase retains its one-second deadline. Input and activity have separate budgets; do not label the complete invocation a one-second operation.

## Rollout

1. Implemented: nextest configuration, Proptest scenarios and coverage reporting. Broader reference-model and uncovered failure-branch work remains in T02–T03 of the delivery crosswalk.
2. Implemented: Criterion, the native socket harness, provenance and Bencher-compatible metrics. Dedicated stale/restart latency and wider workloads remain.
3. Completed for recorded campaigns: source-verified benchmark artifacts uploaded through private Bencher configuration. Keep credentials outside GitHub; each new campaign still needs its own provenance.
4. Implemented: bounded fuzz and separate Docker/Podman jobs. Repeated final-candidate/platform baseline calibration remains before performance thresholds become blocking.

References: [nextest configuration](https://nexte.st/docs/configuring-nextest/), [Criterion](https://bheisler.github.io/criterion.rs/book/), [Bencher GitHub Actions](https://bencher.dev/docs/how-to/github-actions/), [Proptest](https://proptest-rs.github.io/proptest/), [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html), [Loom](https://github.com/tokio-rs/loom), [k6 protocols](https://grafana.com/docs/k6/latest/using-k6/protocols/).

## September 19 local baseline

The clean `72942b0e` documentation audit (runtime source `14f1c519`) passed
`verify.sh bench` on macOS ARM64: Criterion lease/fingerprint samples and all
six shared/disjoint 1/10/100-client socket cases. The 100-client disjoint case
completed 10,000 claim/release cycles with p95 29.17 ms and p99 34.90 ms.
Before/after source and executable manifests matched. The
[existing integrated record](verification/2026-09-12-integrated-desktop.json)
retains every case's counts, timing, provenance and the private artifact hash.

The build campaign had an exclusive lease; actual provider sessions remained
running. This is one local baseline, not model-message latency, repeated
calibration, overnight acceptance or validation of later integration commits.
The earlier socket timeout/errno-35 failures below remain unexplained.

## September 21 desktop refresh cost

The installed `ff4efc61` shared-chat window consumed 10.22 CPU seconds over
60.07 seconds with no UI input (17.01% of one core). The window was 1180×792;
the live coordinator and provider sessions remained active, so this is an
observed populated-project baseline, not an isolated-machine benchmark.
Sampling showed periodic full-window presentation work while worker threads
mostly waited.

Snapshot-only batching did not improve that observation (17.66% on `e609a334`)
and was removed. The smaller follow-up publishes routine accessibility snapshots
through the task stream directly, preserving the native accessibility tree without
sending a second application message that would rebuild/redraw the window. Explicit
focus/reveal completion and the smoke snapshot path remain unchanged. Native
accessibility and before/after CPU checks are required before claiming improvement.

## Repository commands and installation

`bash scripts/verify.sh check` runs the PR gate. `test` runs nextest and doctests; `coverage` writes `artifacts/coverage.lcov`; `bench` runs Criterion and the native socket workload at 1/10/100 clients; `fuzz` runs four bounded nightly campaigns (`FUZZ_SECONDS`, default 60 per target). The native load workload runs shared-path contention and disjoint per-client paths separately. The `socket_v2` series separates successful claim/release cycles (two requests) from claim conflicts (one request), including connection setup. Each outcome records sample count and throughput over the same campaign duration; empty outcomes omit latency percentiles. Attempts, elapsed seconds and conflict ratio accompany each workload. These series must not be compared as continuations of the older `socket_claim_release` series, which mixed both outcomes. Stale detection/restart scenarios remain correctness integration tests until dedicated latency workloads are added. Custom counts use [Bencher Metric Format measures](https://bencher.dev/docs/reference/bencher-metric-format/).

Install tools with `cargo install --locked cargo-nextest --version 0.9.143`, `cargo install --locked cargo-llvm-cov --version 0.9.0`, and `cargo install --locked cargo-fuzz --version 0.13.2`. Add `rustup component add llvm-tools-preview` and `rustup toolchain install nightly --profile minimal`. Criterion and Proptest are workspace development dependencies, pinned transitively by Cargo.lock. Benchmarks use a stable MSRV-compatible Criterion 0.5 harness. Keep workspace and fuzz lockfiles checked in. Fuzz campaigns first require locked dependency resolution and retain before/after source, lockfile, nightly compiler and cargo-fuzz manifests, including when a target fails. A campaign fails if these change during execution. Tools are development-only and do not ship in the release binaries.

CI stores JUnit, coverage, benchmark provenance/results, and fuzz reproducers. Socket campaigns select the executable paths from Cargo's JSON build output, including custom build directories and configured target triples. `benchmark-binaries.json` and `benchmark-binaries-after.json` record the daemon and workload paths and hashes; changing either executable fails the campaign. Proptest automatically records minimized failing seeds beside its source tests; commit those regressions. Do not disable or retry away a failing coordination invariant. Fuzzing complements the deterministic suite and requires nightly; nightly results are tracked separately from stable builds.

`python3 scripts/sustained_use.py --binary /path/to/agentd --output /new/private/report --seconds 600 --files 10000` runs separate 1/10/100-agent populations for ten minutes each. It snapshots the daemon executable before launch so a later build cannot change the running campaign. Each population uses real supervised children, concurrent message/peek/ack and claim/release cycles, resource samples, empty final inbox/lease checks and owned-child cleanup. The final result verifies the snapshot and driver hashes. These task-key workloads do not replace shared/disjoint physical-path benchmarks, actual provider sessions, terminal workloads or overnight growth testing.

`python3 scripts/retention_sustained.py --binary-dir target/release --output /new/private/report --seconds 1200 --retention 120s --agents 10` is the storage-maintenance counterpart: a private daemon with `[journal] retention` set, registered agents cycling leases, journal entries, messages, checkpoints and cursor reads, half of them leaving at half time, checkpoints of finished agents pruned every minute. It asserts that retention pruned by itself within its window, that a pre-prune cursor read in order, that only finished agents' checkpoints went, and that database and memory growth stayed bounded once pruning had taken hold; it records build provenance from `--build-info`, samples and daemon log warnings in `result.json`.

`python3 scripts/restart_smoke.py --binary /path/to/new/agentd --previous-binary /path/to/old/agentd --output /new/private/report` snapshots both builds, creates a pre-upgrade state backup, and checks queued messages, original lease expiries, identity and durable question routing across actual daemon crashes. It verifies incompatible downgrade refusal without state changes. The children are externally owned fixtures; this does not prove transfer of supervised child/PTY ownership during live reload. Reports and fixture databases remain private by default.

The performance workflow contains an optional trusted-main reporting job, but this project intentionally leaves GitHub Bencher credentials unset. Its report job may be skipped while benchmark generation and artifact checks pass. Download artifacts, verify before/after manifests and exact source identity, then upload with the privately configured helper. Never copy the key into a tracked file, release artifact or GitHub secret. Set thresholds after baseline calibration. See [LOCAL-TRIAL.md](LOCAL-TRIAL.md) for desktop, failure and real-agent acceptance work still needed.

## Retaining failed acceptance evidence

A managed-workspace launch failure keeps the original daemon response even when no agent record exists. The fixture attempts ownership collection for partially created containers separately; inspection or log-capture errors cannot replace the primary failure. Both workspace and relay runs write a failed result plus at most 2 MiB of daemon log beside the requested result path, where CI retains them. Authentication directories and databases are not copied.

Native graphical failures record connection state, inventory count, whether the expected fixture was discovered, screenshot-request state, update ticks and elapsed time. These fields help distinguish discovery/connection failures from rendering failures without recording discovered command lines. A passing rerun does not diagnose a prior failure.

The native transport check retains its refused observation in `capture/transport-failure.json`: process index/PID/exit status, `lsof` return code and stdout/stderr capped at 2,048 characters each, including partial timeout output. If that file cannot be written, the observation and capture error remain in the transport exception. The workflow result records the original exception before cleanup; cleanup and reporting failures are retained separately and cannot replace it, including a closed or unencodable diagnostic stream. A cleanup failure also invalidates an otherwise passing result. Any unexpected inspector result still fails acceptance. The [September 15 Linux ARM refusal](verification/2026-09-12-integrated-desktop.json) predates these diagnostics and remains unexplained; the original helper discarded the evidence needed to distinguish an observed TCP socket from an inspection error.

**Keep the graphical fixture visible and retain renderer diagnostics.**
The current desktop uses Iced with tiny-skia. Run `scripts/desktop_smoke.py` for
native window/discovery acceptance and `scripts/iced_workflow_smoke.py` for the
rendered action and restoration scenarios. These explicit fixture modes require
isolated home/socket settings, fresh output directories and a bounded deadline.
The workflow report identifies completed steps, native accessibility checks and
binary hashes. Private progress files help diagnose a stalled step.

```sh
RUST_LOG=warn,iced_winit=debug python3 scripts/desktop_smoke.py \
  --binary-dir <dir> --output <fresh-dir>
```

The earlier egui surface-failure investigations below remain historical evidence,
not claims about Iced. CI and local compositor conditions differ. Retain each
run's renderer and source provenance, and keep local captures private because
discovery may include other sessions on the computer.

Socket load reports connect/write/read/decode failures by operation and retains a bounded fixture-daemon log tail. All agents register before workers start; failure to create a worker cancels already-created waiting workers. Criterion stores each campaign's samples in a fresh `artifacts/criterion.*` directory, alongside source manifests, so cached baseline metadata without its samples cannot become an implicit comparison. The original 100-client Linux failure remains open until the labeled failure is reproduced and explained.

Managed workspace and relay campaigns use a private `0077` umask. This reproduced a helper-image defect: copied relay source retained root-owned `0600`, preventing the workspace UID from reading it. The image recipe now explicitly makes its embedded source readable (`0444`); host fixtures and credentials remain private. The original recipe failed with permission denied and the corrected recipe reported readiness in a real Podman VM before the fix was applied. Both engine CI relay jobs must pass on the final source.

The engine-metadata target mutates real inspection JSON, verifies exit evidence
and requires foreign identity/ownership changes to be refused. Both unmounted
and scoped read-only workspace records are exercised; changing or removing
required mount evidence must fail. The token-filter
target creates an isolated daemon registry and tests the actual restricted
request filter with valid, altered and revoked credentials, another agent,
an outside-project peer and symlink escapes. It never dispatches generated
requests, starts user commands or contacts a provider. Its private database is
removed by the campaign driver on success or failure. Direct token-target runs
must set `AGENTDOCKER_FUZZ_ROOT` to a new private disposable directory and remove
it afterwards. The driver sets an explicit 10-second per-input timeout and
1024 MiB RSS limit, in addition to the campaign duration, and retains per-target
logs and final statistics. Limits follow [libFuzzer's documented options](https://llvm.org/docs/LibFuzzer.html#options).

CodeRabbit automatic review includes every base branch, including stacked
fix/test/docs branches. A skipped or rate-limited review remains pending even
when its status context is green. Draft PRs can run preliminary CI; final review
and all applicable checks on the published head are required before integration.


Failed benchmark campaigns retain a final source manifest and `benchmark-status.tsv` with the exit status of each attempted socket scenario. Each 1/10/100-client shared/disjoint scenario runs once even if an earlier socket scenario fails; the campaign remains failed. Previous outcome files are removed before a campaign. A timeout is never converted into a successful latency sample or retried by the campaign. On this Mac, the first schema-2 campaign at `1410d1b` failed during the shared 100-client claim response (errno 35 with the existing five-second request deadline); disjoint 100 was not attempted by that older script. Other-worktree fuzz/build activity overlapped that campaign. Root cause and a quiet-host acceptance campaign remain outstanding; passing Linux CI does not explain this failure.


## Diagnosing slow state operations

For a separately identified diagnostic campaign, set
`AGENTDOCKER_BENCH_DIAGNOSTICS=1 bash scripts/verify.sh bench`, or dispatch the
Performance workflow with `diagnostics=true`. The fixture daemon enables only
the `agentd_state_timing` debug target. It records at most 256 lock-wait/store
samples of 250 ms or more per daemon, with operation names and durations, and
retains at most an 8 KiB log tail for each scenario. The benchmark manifest
records this mode. Normal campaigns leave it disabled. Instrumented results
are diagnostic evidence: logging can affect timings, and the five-second read
timeout and no-retry policy are unchanged.

The integrated `e008831` disjoint 100-client campaign failed at release-response
read after its other five workloads completed. Its [source-bound failure record](verification/2026-09-07-integration-benchmark-failure.json)
remains open; empty daemon stderr did not distinguish state contention from
storage or host scheduling delays. Capture new evidence before assigning a
cause or treating a later successful run as a resolution.

## Development disk budget

The September 7 local campaign exhausted disk space by retaining debug outputs
in 31 isolated worktrees. About 187 GiB of allocated regenerable caches were
removed; source, credentials and reports were preserved. The installed desktop
app was not the cause. Treat build storage as part of T12 cleanup evidence.

`verify.sh` and `build_native.py` now run a read-only storage preflight before
compilation. It checks the Cargo-reported active target directory, registered
worktrees' default targets and fuzz targets. Defaults require 20 GiB free locally
(5 GiB on CI), limit the current target to 12 GiB, and limit their aggregate to
40 GiB. Override the positive GiB limits with `AGENTDOCKER_BUILD_MIN_FREE_GIB`,
`AGENTDOCKER_BUILD_MAX_CURRENT_GIB` and `AGENTDOCKER_BUILD_MAX_TOTAL_GIB` for the
machine's capacity. A preflight is not a filesystem quota or a reservation;
concurrent tools can still consume space. Nonstandard targets belonging to other
worktrees are outside this inventory.

Keep only one local build campaign active and at most two debug caches. Retain
JUnit, coverage, benchmark manifests/results and failure logs before clearing
inactive caches with [Cargo clean](https://doc.rust-lang.org/cargo/commands/cargo-clean.html).
`cargo clean --profile dev` removes generated development output; inspect
`--dry-run` first. Do not clean an active build/test directory, running binaries,
or another session's cache. Each worktree keeps its own target directory.
Workspace development builds disable incremental compilation, while keeping
line-table debug information. Release builds retain the default optimization
level, enable thin LTO and strip symbols. Direct
Cargo commands do not invoke the storage preflight; run
`python3 scripts/build_storage.py` first. This bounds the campaign workflow,
not all disk use by arbitrary programs.

The [first state timing diagnostic campaign](verification/2026-09-07-state-timing-diagnostic.json)
passed at `ef3fd7b` without reaching the 250 ms logging threshold. Its eight
Bencher reports use a separate diagnostic testbed. The earlier integrated
timeout remains unresolved; compare this campaign only with its recorded mode
and shared-runner limitations in mind.

The [d06a117 macOS failure](verification/2026-09-07-macos-capture-failure.json)
records a capture request followed by failed `Occluded` surface acquisitions,
then a visible, focused, unoccluded viewport at timeout. The pinned eframe
0.36.1 source drains capture commands before acquisition, and egui-wgpu drops
those commands when acquisition fails. A single request can therefore be lost
even when the window later becomes visible. The inspected sources match their
registry archives and Cargo.lock checksums.

The previous egui graphical acceptance requested focus first, waited for known
visibility and retried capture only after a newly reported surface failure,
with at most four requests in the original 60-second deadline. Waiting alone
did not issue additional requests. Its results retained capture-attempt and surface-
failure counts. A real renderer screenshot with the connected fixture remains
mandatory; repeated failures still fail acceptance. This recovery path does not
assign the same cause to earlier runs without matching evidence.

### Download size gates

The desktop packager refuses payloads above 100 MiB and each compressed download
above 40 MiB per architecture (universal builds allow twice these totals). Its
manifest records logical payload and archive bytes. The release workflow also
limits the combined CLI/daemon payload to 30 MiB. CLI tarballs contain just those
two commands; desktop ZIPs contain one self-contained app with all three
executables. Build caches and compiler dependencies are never download inputs.

### Linux graphical transport observation

Linux native-window smoke tests classify the owned process's socket inodes from
its open `/proc/<pid>` directory. This avoids unrelated mount-stat failures from
`lsof` (captured in the September 17 Linux x86 graphical job). Both TCP tables
are required and checked; a kernel without a readable TCP6 table cannot pass
this observer. Remaining socket inodes must be present in known non-TCP tables.
Kernel tables bracket each descriptor sample so normal Unix RPC descriptor
churn does not require an idle process. Every sampled inode must be classified
on at least one side; TCP evidence from either side fails. Unknown sockets,
changed network namespaces, unreadable or malformed
tables and budget exhaustion refuse the observation. The helper is bounded to
4,096 descriptors, 1 MiB per table, eight bracketed samples and an outer five-second
subprocess deadline. A process generation is anchored by its open proc directory.
The [kernel proc contract](https://docs.kernel.org/filesystems/proc.html#process-specific-subdirectories)
describes those process-specific descriptors.

macOS retains the strict `lsof` observer. Both methods poll; a socket opened and
closed between samples can be missed. An exited child is checked by its exit
status, not counted as a successful live socket observation. The earlier Linux
ARM refusal did not retain the same diagnostic and is not explained by this
later captured mount-stat warning. Kernel-table fixtures and actual owned UNIX
and delayed-TCP child checks exercise the Linux observer; final graphical CI
is still required.

The Linux observer makes at most eight bracketed samples with a 1 ms yield
between unclassified samples, within the existing 5 s helper deadline. A local
concurrent-RPC reproduction reduced refusals from 23/100 with identical-FD
requirements to 1/100 with three bracketed samples; the eight-sample version
then completed 100/100. These are recorded trials, not a guarantee of detecting
transient sockets or succeeding at arbitrary load. Persistent unknown sockets
still refuse, and a successful helper report includes a bounded socket count.

For local graphical checks while someone is using the desktop,
`python3 scripts/iced_workflow_smoke.py --binary-dir <built-binaries> --output <new-directory> --skip-idle-measurement`
runs the rendered workflows without opening the ordinary foreground window used
for the idle CPU/RSS sample. The report marks `idle_resources` as `not_run`;
this mode supplies no idle performance evidence. The default command and CI
continue to run that measurement. Smoke workflow windows remain behind the
user's real app.

The September 21 snapshot-only candidate `e609a334` passed the full gate and
580 rendered workflow steps, but its installed 60-second observation was 17.66%
of one core versus the prior 17.01%. Keep that unsuccessful comparison in the
existing [integrated record](verification/2026-09-12-integrated-desktop.json);
passing correctness tests does not establish a CPU improvement.
