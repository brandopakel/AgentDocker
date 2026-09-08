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

Every result includes commit SHA, dirty-content identity if applicable, Rust/tool versions, OS/architecture, CPU, workload parameters and container engine/image identity when used. Different machines and engines are different Bencher testbeds. Establish repeated baselines before selecting regression thresholds; shared-runner timing is initially advisory. Correctness invariants are immediate hard failures. Promote performance checks to blocking only once measured variance supports the threshold; the hook's explicit one-second delivery budget remains an existing functional contract.

## Rollout

1. Add nextest configuration, meaningful Proptest models and coverage reporting to the reviewed foundation.
2. Add Criterion and the native protocol load harness; emit local artifacts and Bencher-compatible metrics.
3. Upload source-verified benchmark artifacts through private Bencher configuration; keep credentials outside GitHub as requested.
4. Add fuzz campaigns and Docker/Podman E2E jobs, then calibrate performance gates from collected baselines.

References: [nextest configuration](https://nexte.st/docs/configuring-nextest/), [Criterion](https://bheisler.github.io/criterion.rs/book/), [Bencher GitHub Actions](https://bencher.dev/docs/how-to/github-actions/), [Proptest](https://proptest-rs.github.io/proptest/), [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html), [Loom](https://github.com/tokio-rs/loom), [k6 protocols](https://grafana.com/docs/k6/latest/using-k6/protocols/).

## Repository commands and installation

`bash scripts/verify.sh check` runs the PR gate. `test` runs nextest and doctests; `coverage` writes `artifacts/coverage.lcov`; `bench` runs Criterion and the native socket workload at 1/10/100 clients; `fuzz` runs four bounded nightly campaigns (`FUZZ_SECONDS`, default 60 per target). The native load workload runs shared-path contention and disjoint per-client paths separately. The `socket_v2` series separates successful claim/release cycles (two requests) from claim conflicts (one request), including connection setup. Each outcome records sample count and throughput over the same campaign duration; empty outcomes omit latency percentiles. Attempts, elapsed seconds and conflict ratio accompany each workload. These series must not be compared as continuations of the older `socket_claim_release` series, which mixed both outcomes. Stale detection/restart scenarios remain correctness integration tests until dedicated latency workloads are added. Custom counts use [Bencher Metric Format measures](https://bencher.dev/docs/reference/bencher-metric-format/).

Install tools with `cargo install --locked cargo-nextest --version 0.9.143`, `cargo install --locked cargo-llvm-cov --version 0.9.0`, and `cargo install --locked cargo-fuzz --version 0.13.2`. Add `rustup component add llvm-tools-preview` and `rustup toolchain install nightly --profile minimal`. Criterion and Proptest are workspace development dependencies, pinned transitively by Cargo.lock. Benchmarks use a stable MSRV-compatible Criterion 0.5 harness. Keep workspace and fuzz lockfiles checked in. Fuzz campaigns first require locked dependency resolution and retain before/after source, lockfile, nightly compiler and cargo-fuzz manifests, including when a target fails. A campaign fails if these change during execution. Tools are development-only and do not ship in the release binaries.

CI stores JUnit, coverage, benchmark provenance/results, and fuzz reproducers. Proptest automatically records minimized failing seeds beside its source tests; commit those regressions. Do not disable or retry away a failing coordination invariant. Fuzzing complements the deterministic suite and requires nightly; nightly results are tracked separately from stable builds.

The performance workflow contains an optional trusted-main reporting job, but this project intentionally leaves GitHub Bencher credentials unset. Its report job may be skipped while benchmark generation and artifact checks pass. Download artifacts, verify before/after manifests and exact source identity, then upload with the privately configured helper. Never copy the key into a tracked file, release artifact or GitHub secret. Set thresholds after baseline calibration. See [LOCAL-TRIAL.md](LOCAL-TRIAL.md) for desktop, failure and real-agent acceptance work still needed.

## Retaining failed acceptance evidence

A managed-workspace launch failure keeps the original daemon response even when no agent record exists. The fixture attempts ownership collection for partially created containers separately; inspection or log-capture errors cannot replace the primary failure. Both workspace and relay runs write a failed result plus at most 2 MiB of daemon log beside the requested result path, where CI retains them. Authentication directories and databases are not copied.

Native graphical failures record connection state, inventory count, whether the expected fixture was discovered, screenshot-request state, frames and elapsed time. These fields help distinguish discovery/connection failures from rendering failures without recording discovered command lines. A passing rerun does not diagnose a prior failure.

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
full debug information and unchanged release/benchmark optimization. Direct
Cargo commands do not invoke the storage preflight; run
`python3 scripts/build_storage.py` first. This bounds the campaign workflow,
not all disk use by arbitrary programs.
