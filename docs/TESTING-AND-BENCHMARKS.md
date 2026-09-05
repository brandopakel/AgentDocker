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

Bencher and k6 were reported installed by the user. Neither executable was visible on this session's PATH or at the usual Homebrew binary paths during the initial check; locate the existing installation before duplicating it. Hosted Bencher/Grafana account setup and repository credentials have not been verified. Local runs and downloadable CI artifacts must work without service credentials. No paid service enrollment is required for the initial plan.

## Behavioral gates

Every PR runs formatting, strict Clippy, unit/integration tests and installer/package checks. New tests target: no overlapping exclusive physical leases; no post-cancellation/exit acquisition; stopping writers retain protection; durable effects have correct event ordering; checksum failure preserves installation; observed stale input requires reread; accepted recovery survives restart; source or image changes invalidate matching validation evidence. Exercise crash points before/after SQLite commits, full/slow output pipes, lost watchers, expired/revoked credentials and engine unavailability using test-owned processes and fixtures.

Nightly/scheduled jobs run bounded fuzz campaigns, repeated concurrency scenarios, large-checkout workloads and both container engines. Failed seeds, logs, JUnit, coverage and benchmark outputs are retained with the exact commit and platform. CodeRabbit reviews implementation and test changes; green automated checks and disposition of valid review findings are required before integration.

## Measurements and thresholds

Record p50/p95/p99 request and hook latency, throughput, stale-warning delay, missed stale detections and false alerts, restart/handoff recovery time, watcher queue gaps, fingerprint throughput, SQLite write latency and process memory. Use workloads with 1/10/100 concurrent agents and small/medium/large fixture checkouts, with cold and warm runs separated.

Every result includes commit SHA, dirty-content identity if applicable, Rust/tool versions, OS/architecture, CPU, workload parameters and container engine/image identity when used. Different machines and engines are different Bencher testbeds. Establish repeated baselines before selecting regression thresholds; shared-runner timing is initially advisory. Correctness invariants are immediate hard failures. Promote performance checks to blocking only once measured variance supports the threshold; the hook's explicit one-second delivery budget remains an existing functional contract.

## Rollout

1. Add nextest configuration, meaningful Proptest models and coverage reporting to the reviewed foundation.
2. Add Criterion and the native protocol load harness; emit local artifacts and Bencher-compatible metrics.
3. Connect Bencher CI reporting when project/key configuration is available, with credentials isolated from untrusted PR execution.
4. Add fuzz campaigns and Docker/Podman E2E jobs, then calibrate performance gates from collected baselines.

References: [nextest configuration](https://nexte.st/docs/configuring-nextest/), [Criterion](https://bheisler.github.io/criterion.rs/book/), [Bencher GitHub Actions](https://bencher.dev/docs/how-to/github-actions/), [Proptest](https://proptest-rs.github.io/proptest/), [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html), [Loom](https://github.com/tokio-rs/loom), [k6 protocols](https://grafana.com/docs/k6/latest/using-k6/protocols/).

## Repository commands and installation

`bash scripts/verify.sh check` runs the PR gate. `test` runs nextest and doctests; `coverage` writes `artifacts/coverage.lcov`; `bench` runs Criterion and the native socket workload at 1/10/100 clients; `fuzz` runs two bounded nightly campaigns (`FUZZ_SECONDS`, default 60 per target). The initial load workload measures contention and successful release, including connection setup; stale detection/restart scenarios remain correctness integration tests until dedicated latency workloads are added.

Install tools with `cargo install --locked cargo-nextest --version 0.9.143`, `cargo install --locked cargo-llvm-cov --version 0.9.0`, and `cargo install --locked cargo-fuzz --version 0.13.2`. Add `rustup component add llvm-tools-preview` and `rustup toolchain install nightly --profile minimal`. Criterion and Proptest are workspace development dependencies, pinned transitively by Cargo.lock. Benchmarks use a stable MSRV-compatible Criterion 0.5 harness. Keep workspace and fuzz lockfiles checked in. Tools are development-only and do not ship in the release binaries.

CI stores JUnit, coverage, benchmark provenance/results, and fuzz reproducers. Proptest automatically records minimized failing seeds beside its source tests; commit those regressions. Do not disable or retry away a failing coordination invariant. Fuzzing complements the deterministic suite and requires nightly; nightly results are tracked separately from stable builds.

Optional Bencher integration uses repository variable `BENCHER_PROJECT` and secret `BENCHER_API_KEY`. Reporting uploads already-produced results only on trusted `main` pushes; ordinary PRs require neither a key nor a service account. See the performance workflow for the exact reporting commands. Set thresholds after baseline calibration. k6 remains part of the documented toolkit for future supported network transports, with no synthetic production endpoint introduced for testing.
