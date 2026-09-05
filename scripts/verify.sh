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
    python3 scripts/benchmark_manifest.py > artifacts/benchmark-manifest.json
    cargo bench --locked -p agentdocker-core --bench leases -- --noplot 2>&1 | tee artifacts/criterion-leases.txt
    cargo bench --locked -p agentdocker-host --bench fingerprint -- --noplot 2>&1 | tee artifacts/criterion-fingerprint.txt
    cargo build --locked --release -p agentdocker --bin agentd
    cargo build --locked --release -p agentd --example socket_load
    for clients in 1 10 100; do
      target/release/examples/socket_load "$(pwd)/target/release/agentd" "$clients" 100 > "artifacts/socket-${clients}.json"
    done
    python3 scripts/benchmark_manifest.py > artifacts/benchmark-manifest-after.json
    python3 -c 'import json; a=json.load(open("artifacts/benchmark-manifest.json")); b=json.load(open("artifacts/benchmark-manifest-after.json")); assert a == b, "source or environment changed during benchmarks"'
    ;;
  fuzz)
    seconds="${FUZZ_SECONDS:-60}"
    [[ "$seconds" =~ ^[0-9]+$ ]] && (( seconds > 0 && seconds <= 3600 )) || { echo 'FUZZ_SECONDS must be 1–3600' >&2; exit 2; }
    mkdir -p fuzz/corpus/protocol
    cp fuzz/seeds/protocol/*.json fuzz/corpus/protocol/
    cargo +nightly fuzz run protocol -- -max_total_time="$seconds" -max_len=65536 -verbosity=0
    cargo +nightly fuzz run resource-keys -- -max_total_time="$seconds" -max_len=4096 -verbosity=0
    ;;
  *) echo 'usage: bash scripts/verify.sh [check|test|coverage|bench|fuzz]' >&2; exit 2 ;;
esac
