#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-check}" in
  check)
    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets -- -D warnings
    cargo nextest run --locked --workspace --profile ci
    cargo test --locked --workspace --doc
    python3 -m unittest discover -s tests
    cargo package --locked --workspace --no-verify --allow-dirty
    cargo build --locked --workspace --release
    ;;
  test)
    cargo nextest run --locked --workspace --profile ci
    cargo test --locked --workspace --doc
    ;;
  coverage)
    mkdir -p artifacts
    cargo llvm-cov nextest --locked --workspace --profile ci --lcov --output-path artifacts/coverage.lcov
    ;;
  bench)
    mkdir -p artifacts
    # Criterion cache metadata alone is not a valid comparison baseline. Keep
    # each campaign's complete samples with its source manifest and artifacts.
    export CRITERION_HOME="$(mktemp -d "$PWD/artifacts/criterion.XXXXXX")"
    python3 scripts/benchmark_manifest.py > artifacts/benchmark-manifest.json
    # A failed workload still needs a matching final manifest. Remove stale
    # outcomes so an interrupted campaign cannot appear to have run them.
    rm -f artifacts/benchmark-manifest-after.json artifacts/socket-{shared,disjoint}-{1,10,100}.{json,log}
    : > artifacts/benchmark-status.tsv
    finish_bench() {
      bench_exit=$?
      trap - EXIT
      python3 scripts/benchmark_manifest.py > artifacts/benchmark-manifest-after.json || bench_exit=1
      python3 -c 'import json; a=json.load(open("artifacts/benchmark-manifest.json")); b=json.load(open("artifacts/benchmark-manifest-after.json")); assert a == b, "source or environment changed during benchmarks"' || bench_exit=1
      exit "$bench_exit"
    }
    trap finish_bench EXIT
    cargo bench --locked -p agentdocker-core --bench leases -- --noplot 2>&1 | tee artifacts/criterion-leases.txt
    cargo bench --locked -p agentdocker-host --bench fingerprint -- --noplot 2>&1 | tee artifacts/criterion-fingerprint.txt
    cargo build --locked --release -p agentdocker --bin agentd
    cargo build --locked --release -p agentd --example socket_load
    bench_status=0
    for clients in 1 10 100; do
      for workload in shared disjoint; do
        workload_status=0
        target/release/examples/socket_load "$(pwd)/target/release/agentd" "$clients" 100 "$workload" > "artifacts/socket-${workload}-${clients}.json" 2> "artifacts/socket-${workload}-${clients}.log" || workload_status=$?
        printf '%s\t%s\t%s\n' "$workload" "$clients" "$workload_status" >> artifacts/benchmark-status.tsv
        if (( workload_status != 0 )); then
          bench_status=1
          cat "artifacts/socket-${workload}-${clients}.log" >&2
        fi
      done
    done
    exit "$bench_status"
    ;;
  fuzz)
    seconds="${FUZZ_SECONDS:-60}"
    [[ "$seconds" =~ ^[0-9]+$ ]] && (( seconds > 0 && seconds <= 3600 )) || { echo 'FUZZ_SECONDS must be 1–3600' >&2; exit 2; }
    mkdir -p artifacts
    # cargo-fuzz does not expose --locked. Refuse a stale lock before the
    # campaign and verify every tracked source byte again even on failure.
    cargo +nightly metadata --locked --manifest-path fuzz/Cargo.toml --format-version 1 > /dev/null
    python3 scripts/benchmark_manifest.py fuzz "$seconds" > artifacts/fuzz-manifest.json
    # A single disposable root belongs to this campaign, including the
    # token-filter database. Abort/crash inputs remain in fuzz/artifacts/.
    fuzz_fixture_root="$(mktemp -d /tmp/ad-fuzz.XXXXXX)"
    export AGENTDOCKER_FUZZ_ROOT="$fuzz_fixture_root"
    finish_fuzz() {
      fuzz_status=$?
      trap - EXIT
      python3 scripts/benchmark_manifest.py fuzz "$seconds" > artifacts/fuzz-manifest-after.json || fuzz_status=1
      python3 -c 'import json; a=json.load(open("artifacts/fuzz-manifest.json")); b=json.load(open("artifacts/fuzz-manifest-after.json")); assert a == b, "source, lockfile or toolchain changed during fuzzing"' || fuzz_status=1
      rm -rf -- "$fuzz_fixture_root" || fuzz_status=1
      exit "$fuzz_status"
    }
    trap finish_fuzz EXIT
    for target in protocol resource-keys engine-metadata token-filter; do
      mkdir -p "fuzz/corpus/$target"
      if [[ -d "fuzz/seeds/$target" ]]; then
        cp "fuzz/seeds/$target/"* "fuzz/corpus/$target/"
      fi
      max_len=65536
      [[ "$target" != resource-keys ]] || max_len=4096
      cargo +nightly fuzz run "$target" -- -max_total_time="$seconds" -max_len="$max_len" -timeout=10 -rss_limit_mb=1024 -print_final_stats=1 -verbosity=0 2>&1 | tee "artifacts/fuzz-$target.log"
    done
    ;;
  *) echo 'usage: bash scripts/verify.sh [check|test|coverage|bench|fuzz]' >&2; exit 2 ;;
esac
