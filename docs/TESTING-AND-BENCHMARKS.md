# Testing and benchmarking

Tool installation and configuration do not constitute test coverage: each addition must exercise a concrete product invariant.

## Selected stack

| Tool/service | Purpose in AgentDocker |
|---|---|
| cargo-nextest + GitHub Actions | Isolated Rust test execution, timeouts, resource groups and JUnit artifacts. Keep doctests separately. Report flaky outcomes; retries must not hide correctness failures. |
| Criterion + Bencher | Measure lease operations, content fingerprints, retained accounting-prefix verification, SQLite writes and recovery queries; retain benchmark trends and compare PRs against their actual base. Criterion measures; Bencher stores and evaluates results. |
| Proptest | Generate claim/renew/release/expire/finish sequences and aliases; compare with a simple reference model. Persist minimized failing seeds. |
| cargo-llvm-cov | Find untested cancellation, authorization, migration and recovery branches. Publish coverage artifacts; use coverage to guide meaningful tests rather than target a vanity percentage. |
| Native Rust Unix-socket load harness | Exercise the real JSON-line protocol under concurrent clients, disconnects, slow readers, lease contention and daemon restart. Export Bencher Metric Format results. |
| cargo-fuzz | Fuzz protocol decoding, resource/path normalization, bounded metadata parsers and token request filtering. Use a separate nightly fuzz job and retain crashing inputs as regressions. |
| Loom | Selective: model extracted in-memory synchronization algorithms if finer-grained concurrency is introduced. It does not model SQLite or filesystem/OS behavior and is not a drop-in replacement for daemon integration tests. |
| k6 | Transport-dependent: use for supported network endpoints when present. Evaluate a maintained extension before using the native Unix-socket protocol; a bridge benchmark measures the bridge too. Do not add a production HTTP API solely to accommodate k6. |
| Real Docker and Podman jobs | Shared engine contract scenarios and separate real-engine results for builds, lifecycle, mount translation and scoped authentication. Linux CI first, explicit macOS VM checks. |

The host `fingerprint` benchmark includes `usage_prefix/verify_16_mib`: it prepares a complete retained cursor outside the timed loop, then verifies its entire 16 MiB prefix through the collector's bounded `Session` API. Both fixture scanning and prefix verification abort after ten consecutive attempts without progress, instead of retrying indefinitely under sustained host load. ARM uses the runtime-detected SHA-256 backend; `--features sha2/force-soft` provides an explicit software control for a local comparison. Keep those feature sets labeled and preserve each run's samples before a subsequent Criterion baseline replaces them. This is a prefix-verification benchmark, not a multi-project daemon or installed idle-CPU measurement.

Bencher credentials and configuration stay outside the repository and GitHub. Local runs and downloadable CI artifacts work without credentials. k6 is optional for network transports; it is not a reason to introduce a production HTTP endpoint.

## Behavioral gates

Every PR runs formatting, strict Clippy, unit/integration tests and installer/package checks. New tests target: no overlapping exclusive physical leases; no post-cancellation/exit acquisition; stopping writers retain protection; durable effects have correct event ordering; checksum failure preserves installation; observed stale input requires reread; accepted recovery survives restart; source or image changes invalidate matching validation evidence. Exercise crash points before/after SQLite commits, full/slow output pipes, lost watchers, expired/revoked credentials and engine unavailability using test-owned processes and fixtures. The Codex input loops run end to end in `crates/cli/tests/codex_bridge.rs` (`cargo test -p agentdocker --test codex_bridge`), which is its own harness so the test binary can also be the mock app-server the bridge and the receiver spawn: a fixture daemon, a managed bridge whose message becomes a turn with a receipt proved from the user item, a refused turn, an app-server older than the floor, and an existing session the receiver feeds through the native queue. It is a contract test against the protocol subset the loops use, not a trial on Codex.

The terminal writer-failure regression prints test-only phase markers around
socket setup, injected write failure, reader completion and cleanup/join.
These diagnostics do not change terminal behavior or weaken the three-second
completion assertion. The writer-failure test and its two socket-reader
siblings also arm a test-only watchdog: after 20 seconds it prints thread
diagnostics and aborts, so a failure names the blocked call instead of ending
at nextest's 90-second kill. On macOS that is every thread's stack from
`sample`, run in a private temporary directory and ended after 30 seconds with
any partial output kept; Linux `/proc/self/task` gives task wait states, not
stacks.

The Windows daemon smoke's cleanup rescue starts one private daemon and retains
all readiness probes within its ten-second budget, each capped at three
seconds or the remaining allowance. A timed-out probe does not discard the rest
of that budget; a successful probe returning after the deadline is retained as
an observation but still fails readiness. Failure to obtain readiness within the
budget fails cleanup. The report keeps probe errors/timings, the rescue PID/exit
and up to 16 KiB of its startup log. Functional assertion failures remain failed
even when cleanup succeeds; no trial is replayed.

The scheduled workflow runs bounded protocol, resource-key, engine-metadata and token-filter fuzz campaigns. Docker/Podman protocol jobs run on PRs and main pushes. Failed seeds, logs, JUnit, coverage and benchmark outputs are retained with the exact commit and platform. CodeRabbit reviews implementation and test changes; green automated checks and disposition of valid review findings are required before integration.

## Measurements and thresholds

For an actual Codex lifecycle delivery trial, run
`python3 scripts/codex_delivery_smoke.py --binary-dir <package>/bin --codex <complete-provider-bundle>/bin/codex --provider-home <private-authenticated-profile> --output <new-private-directory>`.
Authenticate the dedicated mode-0700 profile through normal Codex login first;
the driver never copies credentials. It refuses existing user config and hook
sources before invoking the provider. Account-synced plugin caches can exist in a
fresh profile, so both plugin loaders are disabled for this invocation with
`--disable plugins --disable remote_plugin`; cached files and saved preferences
are left unchanged. One-invocation hook trust is for the fixture's reviewed inline
hooks. A rejected profile records a failure before any child launches. The fixture does not pass
`--ignore-user-config`: it keeps the hook configuration layer
active, only in the checked profile. This follows the [official hook-source and
trust contract](https://learn.chatgpt.com/docs/hooks). Managed policy remains in
force; the preflight checks the dedicated profile and fresh trial project, not
system configuration or provider-managed settings. Those host layers can affect
a trial and remain in force; a private profile is not proof that they are absent.
Existing provider configuration/authentication files are checked unchanged.
The test requires correlated replies and automatic receipts at all three lifecycle
boundaries; it does not infer idle wake from hooks or MCP contact.

Record p50/p95/p99 request and hook latency, throughput, stale-warning delay, missed stale detections and false alerts, restart/handoff recovery time, watcher queue gaps, fingerprint throughput, SQLite write latency and process memory. Use workloads with 1/10/100 concurrent agents and small/medium/large fixture checkouts, with cold and warm runs separated.

Every result includes commit SHA, dirty-content identity if applicable, Rust/tool versions, OS/architecture, CPU, workload parameters and container engine/image identity when used. Different machines and engines are different Bencher testbeds. Establish repeated baselines before selecting regression thresholds; shared-runner timing is advisory. Correctness invariants are immediate hard failures. Promote performance checks to blocking only once measured variance supports the threshold; the hook's coordination/output phase retains its one-second deadline. Input and activity have separate budgets; do not label the complete invocation a one-second operation.

References: [nextest configuration](https://nexte.st/docs/configuring-nextest/), [Criterion](https://bheisler.github.io/criterion.rs/book/), [Bencher GitHub Actions](https://bencher.dev/docs/how-to/github-actions/), [Proptest](https://proptest-rs.github.io/proptest/), [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html), [Loom](https://github.com/tokio-rs/loom), [k6 protocols](https://grafana.com/docs/k6/latest/using-k6/protocols/).

## Repository commands and installation

`bash scripts/verify.sh check` runs the PR gate. `test` runs nextest and doctests; `coverage` writes `artifacts/coverage.lcov`; `bench` runs Criterion and the native socket workload at 1/10/100 clients; `fuzz` runs four bounded nightly campaigns (`FUZZ_SECONDS`, default 60 per target). The native load workload runs shared-path contention and disjoint per-client paths separately. The `socket_v2` series separates successful claim/release cycles (two requests) from claim conflicts (one request), including connection setup. Each outcome records sample count and throughput over the same campaign duration; empty outcomes omit latency percentiles. Attempts, elapsed seconds and conflict ratio accompany each workload. Retained `socket_claim_release` results are not comparable with `socket_v2`: the older series mixed successes and conflicts, which `socket_v2` splits into different request counts. Stale detection/restart scenarios are correctness integration tests, not latency workloads. Custom counts use [Bencher Metric Format measures](https://bencher.dev/docs/reference/bencher-metric-format/).

Install tools with `cargo install --locked cargo-nextest --version 0.9.143`, `cargo install --locked cargo-llvm-cov --version 0.9.0`, and `cargo install --locked cargo-fuzz --version 0.13.2`. Add `rustup component add llvm-tools-preview` and `rustup toolchain install nightly --profile minimal`. Criterion and Proptest are workspace development dependencies, pinned transitively by Cargo.lock. Benchmarks use a stable MSRV-compatible Criterion 0.5 harness. Keep workspace and fuzz lockfiles checked in. Fuzz campaigns first require locked dependency resolution and retain before/after source, lockfile, nightly compiler and cargo-fuzz manifests, including when a target fails. A campaign fails if these change during execution. Tools are development-only and do not ship in the release binaries.

CI stores JUnit, coverage, benchmark provenance/results, and fuzz reproducers. Socket campaigns select the executable paths from Cargo's JSON build output, including custom build directories and configured target triples. `benchmark-binaries.json` and `benchmark-binaries-after.json` record the daemon and workload paths and hashes; changing either executable fails the campaign. Proptest automatically records minimized failing seeds beside its source tests; commit those regressions. Do not disable or retry away a failing coordination invariant. Fuzzing complements the deterministic suite and requires nightly; nightly results are tracked separately from stable builds.

`python3 scripts/sustained_use.py --binary /path/to/agentd --output /new/private/report --seconds 600 --files 10000` runs separate 1/10/100-agent populations for ten minutes each. It snapshots the daemon executable before launch so a later build cannot change the running campaign. Each population uses real supervised children, concurrent message/peek/ack and claim/release cycles, resource samples, empty final inbox/lease checks and owned-child cleanup. The final result verifies the snapshot and driver hashes. These task-key workloads do not replace shared/disjoint physical-path benchmarks, actual provider sessions, terminal workloads or overnight growth testing.

`python3 scripts/retention_sustained.py --binary-dir target/release --output /new/private/report --seconds 1200 --retention 120s --agents 10` is the storage-maintenance counterpart: a private daemon with `[journal] retention` set, registered agents cycling leases, journal entries, messages, checkpoints and cursor reads, half of them leaving at half time, checkpoints of finished agents pruned every minute. It asserts that retention pruned by itself within its window, that a pre-prune cursor read in order, that only finished agents' checkpoints went, and that database and memory growth stayed bounded once pruning had taken hold; it records build provenance from `--build-info`, samples and daemon log warnings in `result.json`.

`python3 scripts/restart_smoke.py --binary /path/to/new/agentd --previous-binary /path/to/old/agentd --output /new/private/report` snapshots both builds, creates a pre-upgrade state backup, and checks queued messages, original lease expiries, identity and durable question routing across actual daemon crashes. It verifies incompatible downgrade refusal without state changes. The children are externally owned fixtures; this does not prove transfer of supervised child/PTY ownership during live reload. Reports and fixture databases remain private by default.

The performance workflow contains an optional trusted-main reporting job that needs Bencher credentials configured; GitHub holds none. Its report job may be skipped while benchmark generation and artifact checks pass. Download artifacts, verify before/after manifests and exact source identity, then upload with the privately configured helper. Never copy the key into a tracked file, release artifact or GitHub secret. Set thresholds after baseline calibration.

The native Codex queue fixture uses unbuffered app-server pipes for its timed
read-only receipt queries: Python read-ahead must not hide a complete queued
response from the next file-descriptor readiness check. Its model endpoint is a
loopback fixture, so it does not establish real-account acceptance.

## Retaining failure diagnostics

A managed-workspace launch failure keeps the original daemon response even when no agent record exists. The fixture attempts ownership collection for partially created containers separately; inspection or log-capture errors cannot replace the primary failure. Both workspace and relay runs write a failed result plus at most 2 MiB of daemon log beside the requested result path, where CI retains them. Authentication directories and databases are not copied.

Native graphical failures record connection state, inventory count, whether the expected fixture was discovered, screenshot-request state, update ticks and elapsed time. These fields help distinguish discovery/connection failures from rendering failures without recording discovered command lines. A passing rerun does not diagnose a prior failure.

Native Windows package installation acceptance registers three uniquely named,
disabled Task Scheduler fixtures without triggers and never starts their actions.
Trigger checks distinguish a null trigger property from a one-element array.
An opaque wrapper and an exact retained-version reference must protect the old
payload and invalidate a prior removal plan. A stable-bootstrap reference must
permit inactive-build pruning while still blocking launcher uninstall. Each
fixture is removed only if its description and complete action still match;
the original maintenance plan must return after removal. Reports retain command
outcomes and cleanup state. This runs the extracted CLI on Windows even though
the CLI Rust test suite contains Unix-only fixtures.

The native transport check retains its refused observation in `capture/transport-failure.json`: process index/PID/exit status, `lsof` return code and stdout/stderr capped at 2,048 characters each, including partial timeout output. If that file cannot be written, the observation and capture error remain in the transport exception. The workflow result records the original exception before cleanup; cleanup and reporting failures are retained separately and cannot replace it, including a closed or unencodable diagnostic stream. A cleanup failure also invalidates an otherwise passing result. Any unexpected inspector result still fails acceptance.

**Keep the graphical fixture visible and retain renderer diagnostics.**
The current desktop uses Iced with tiny-skia. Run `scripts/desktop_smoke.py` for
native window/discovery acceptance and `scripts/iced_workflow_smoke.py` for the
rendered action and restoration scenarios. External macOS accessibility inspection needs an unlocked desktop, and in-process smoke does not certify screen-reader support. These explicit fixture modes require
isolated home/socket settings, fresh output directories and a bounded deadline.
The workflow report identifies completed steps, native accessibility checks and
binary hashes. Private progress files help diagnose a stalled step.

```sh
RUST_LOG=warn,iced_winit=debug python3 scripts/desktop_smoke.py \
  --binary-dir <dir> --output <fresh-dir>
```

CI and local compositor conditions differ. Retain each
run's renderer and source provenance, and keep local captures private because
discovery may include other sessions on the computer.

Socket load reports connect/write/read/decode failures by operation and retains a bounded fixture-daemon log tail. All agents register before workers start; failure to create a worker cancels already-created waiting workers. Criterion stores each campaign's samples in a fresh `artifacts/criterion.*` directory, alongside source manifests, so cached baseline metadata without its samples cannot become an implicit comparison.

Managed workspace and relay campaigns use a private `0077` umask, so the relay
image recipe explicitly makes its embedded source readable (`0444`) by the
workspace UID; host fixtures and credentials remain private.

The engine-metadata target mutates real inspection JSON, verifies exit evidence
and requires foreign identity/ownership changes to be refused. Both unmounted
and scoped read-only workspace records are exercised; changing or removing
required mount evidence must fail. The token-filter
target creates an isolated daemon registry and tests the actual restricted
request filter with valid, altered and revoked credentials, another agent,
an outside-project peer and symlink escapes. The oracle covers owner-only role,
delivery-queue, provider/activity-report and emitted-byte operations. Seeds cover
each operation with the owner, another agent and a revoked credential; a
deterministic daemon test additionally checks invalid credentials and owner
name/full-ID/unique-prefix canonicalization without changing payloads. The fuzz
oracle checks the original requested actor as well as the returned canonical ID,
so a filter that silently rewrites a foreign actor cannot pass. It never dispatches
generated requests, starts user commands or contacts a provider. New restricted
operations must update this independent oracle and its credential seeds. Its private database is
removed by the campaign driver on success or failure. Direct token-target runs
must set `AGENTDOCKER_FUZZ_ROOT` to a new private disposable directory and remove
it afterwards. The driver sets an explicit 10-second per-input timeout and
1024 MiB RSS limit, in addition to the campaign duration, and retains per-target
logs and final statistics. Limits follow [libFuzzer's documented options](https://llvm.org/docs/LibFuzzer.html#options).

CodeRabbit automatic review includes every base branch, including stacked
fix/test/docs branches. A skipped or rate-limited review remains pending even
when its status context is green. Draft PRs can run preliminary CI; final review
and all applicable checks on the published head are required before integration.

Controller-binding fixtures resolve the intended `sleep` executable before
spawning, then wait at most five seconds for the child's kernel executable path
to match it, since immediately after spawn the path can still be the parent's.
A pipe-gated exec regression covers this setup boundary; production executable checks remain
strict, and failed coordination assertions are never retried.


Failed benchmark campaigns retain a final source manifest and `benchmark-status.tsv` with the exit status of each attempted socket scenario. Each 1/10/100-client shared/disjoint scenario runs once even if an earlier socket scenario fails; the campaign remains failed. Previous outcome files are removed before a campaign. A timeout is never converted into a successful latency sample or retried by the campaign.


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

## Development disk budget

`verify.sh` and `build_native.py` run a read-only storage preflight before
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
line-table debug information. Release builds optimize the complete linked
dependency graph for size, use one codegen unit with full LTO and strip symbols.
Panic unwinding remains enabled; release benchmarks guard runtime performance. Direct
Cargo commands do not invoke the storage preflight; run
`python3 scripts/build_storage.py` first. This bounds the campaign workflow,
not all disk use by arbitrary programs.

### Download size gates

The desktop packager refuses payloads above 100 MiB and each compressed download
above 40 MiB per architecture (universal builds allow twice these totals). Its
manifest records logical payload and archive bytes, a hard-linked file counted
once. The release workflow also limits the combined CLI/daemon payload to
30 MiB: `agentd` is a hard link to `agentdocker`, one file, counted once. The same reusable CLI archive
workflow runs all four release targets on PRs and main, so a protected tag is
not the first size check. Failures report each executable and archive size.
CLI tarballs contain just those
two commands, `agentd` stored as a hard link to `agentdocker`; desktop ZIPs
contain one self-contained app with all three executables, `agentd` stored whole
because a ZIP holds no links. Build caches and compiler dependencies are never download inputs.

### Linux graphical transport observation

Linux native-window smoke tests classify the owned process's socket inodes from
its open `/proc/<pid>` directory. This avoids unrelated mount-stat failures from
`lsof`. Both TCP tables
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
status, not counted as a successful live socket observation. Kernel-table
fixtures and actual owned UNIX and delayed-TCP child checks exercise the Linux
observer.

The Linux observer makes at most eight bracketed samples with a 1 ms yield
between unclassified samples, within the existing 5 s helper deadline. This
bounds, but does not guarantee, detection of transient sockets at arbitrary
load. Persistent unknown sockets refuse, and a successful helper report
includes a bounded socket count.

For local graphical checks while someone is using the desktop,
`python3 scripts/iced_workflow_smoke.py --binary-dir <built-binaries> --output <new-directory> --skip-idle-measurement`
runs the rendered workflows without opening the ordinary foreground window used
for the idle CPU/RSS sample. The report marks `idle_resources` as `not_run`;
this mode supplies no idle performance evidence. The default command and CI
continue to run that measurement. Smoke workflow windows remain behind the
ordinary app window.
