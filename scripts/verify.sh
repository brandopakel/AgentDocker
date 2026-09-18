#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
python3 scripts/build_storage.py

# One local build campaign at a time. The agents on a shared machine hold
# the exclusive lease `task:local-cargo-campaign` while their cargo runs;
# this script takes it for the caller so a run never starts on top of
# another's. It asks only the machine's own daemon: with AGENTDOCKER_HOME
# set (a private or test daemon), on CI, or with no daemon answering, it
# runs without the lease. AGENTDOCKER_CAMPAIGN_LEASE=off skips it too.
# A lease the caller already holds is kept and never released here; a
# lease somebody else holds past the wait stops the run before any cargo
# process exists, and says whose it is.
campaign_lease=""
campaign_start() {
  local mode="$1"
  [ "${AGENTDOCKER_CAMPAIGN_LEASE:-on}" = "off" ] && return 0
  [ -n "${AGENTDOCKER_HOME:-}" ] && return 0
  [ -n "${CI:-}" ] && return 0
  command -v agentdocker >/dev/null 2>&1 || return 0
  AGENTDOCKER_NO_AUTOSTART=1 agentdocker ping >/dev/null 2>&1 || return 0
  # The ids on the resource before asking: a claim by an agent that
  # already holds it renews that lease and answers with the same id, and
  # a lease that was the caller's before this run stays the caller's.
  local before
  before="$(AGENTDOCKER_NO_AUTOSTART=1 agentdocker leases --resource task:local-cargo-campaign 2>/dev/null | tail -n +2 | awk '{print $1}' || true)"
  local ttl=1800
  case "$mode" in bench|coverage|fuzz) ttl=3600 ;; esac
  local output
  if output="$(AGENTDOCKER_NO_AUTOSTART=1 agentdocker claim task:local-cargo-campaign \
      --ttl "$ttl" --wait "${AGENTDOCKER_CAMPAIGN_WAIT:-600}" \
      --note "scripts/verify.sh $mode in $PWD" 2>&1)"; then
    if grep -qx "$output" <<<"$before"; then
      echo "verify.sh: task:local-cargo-campaign was already yours ($output); renewed, not released here" >&2
      return 0
    fi
    campaign_lease="$output"
    echo "verify.sh: holding task:local-cargo-campaign ($campaign_lease) for this $mode run" >&2
    return 0
  fi
  case "$output" in
    *"give --as"*|*"specify the sender"*|*"--as <AGENT>"*)
      # No identity to claim with. With nobody holding the lease that is
      # only a missing courtesy; with a holder it may be somebody else's
      # campaign, and not knowing is no licence to run on top of it.
      if [ -n "$before" ]; then
        echo "verify.sh: cannot tell which agent this is, and task:local-cargo-campaign is held; not starting cargo on top of it (if that lease is yours, run with AGENTDOCKER_CAMPAIGN_LEASE=off or AGENTDOCKER_AGENT_ID set)." >&2
        AGENTDOCKER_NO_AUTOSTART=1 agentdocker leases --resource task:local-cargo-campaign >&2 || true
        exit 75
      fi
      echo "verify.sh: cannot tell which agent this is (${output%%$'\n'*}); nobody holds the campaign lease, running without it" >&2
      return 0 ;;
  esac
  echo "verify.sh: task:local-cargo-campaign is held by another campaign; not starting cargo on top of it." >&2
  echo "$output" >&2
  AGENTDOCKER_NO_AUTOSTART=1 agentdocker leases --resource task:local-cargo-campaign >&2 || true
  exit 75
}
campaign_end() {
  [ -n "$campaign_lease" ] || return 0
  AGENTDOCKER_NO_AUTOSTART=1 agentdocker release "$campaign_lease" \
    --summary "scripts/verify.sh finished (exit $1)" >/dev/null 2>&1 || true
}
trap 'campaign_end $?' EXIT
campaign_start "${1:-check}"

case "${1:-check}" in
  check)
    # The documentation is part of what is verified: the index is complete,
    # links resolve, the verification index is current, and a code change
    # against main came with a documentation change or a commit saying why.
    if base="$(git merge-base HEAD origin/main 2>/dev/null)"; then
      python3 scripts/docs_check.py --base "$base"
    else
      python3 scripts/docs_check.py
    fi
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
    rm -f artifacts/benchmark-manifest-after.json artifacts/benchmark-binaries{,-after}.json artifacts/socket-{shared,disjoint}-{1,10,100}.{json,log}
    : > artifacts/benchmark-status.tsv
    benchmark_executable() {
      python3 - "$1" "$2" <<'PY'
import json, pathlib, sys
messages = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
paths = {m['executable'] for m in messages if m.get('reason') == 'compiler-artifact'
         and m.get('target', {}).get('name') == sys.argv[2] and m.get('executable')}
if len(paths) != 1:
    raise SystemExit('expected one Cargo executable for ' + sys.argv[2])
path = pathlib.Path(paths.pop()).resolve(strict=True)
if not path.is_file():
    raise SystemExit('Cargo executable is not a file')
print(path)
PY
    }
    benchmark_binaries() {
      python3 - "$bench_daemon" "$bench_workload" <<'PY'
import hashlib, json, pathlib, sys
print(json.dumps([{ 'path': name, 'sha256': hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest() }
                  for name in sys.argv[1:]], indent=2))
PY
    }
    finish_bench() {
      bench_exit=$?
      trap - EXIT
      python3 scripts/benchmark_manifest.py > artifacts/benchmark-manifest-after.json || bench_exit=1
      python3 -c 'import json; a=json.load(open("artifacts/benchmark-manifest.json")); b=json.load(open("artifacts/benchmark-manifest-after.json")); assert a == b, "source or environment changed during benchmarks"' || bench_exit=1
      if [[ -f artifacts/benchmark-binaries.json ]]; then
        benchmark_binaries > artifacts/benchmark-binaries-after.json || bench_exit=1
        cmp -s artifacts/benchmark-binaries.json artifacts/benchmark-binaries-after.json || { echo 'benchmark binaries changed during workloads' >&2; bench_exit=1; }
      fi
      exit "$bench_exit"
    }
    trap finish_bench EXIT
    cargo bench --locked -p agentdocker-core --bench leases -- --noplot 2>&1 | tee artifacts/criterion-leases.txt
    cargo bench --locked -p agentdocker-host --bench fingerprint -- --noplot 2>&1 | tee artifacts/criterion-fingerprint.txt
    cargo build --locked --release -p agentdocker --bin agentd --message-format=json-render-diagnostics > artifacts/benchmark-agentd-build.jsonl
    cargo build --locked --release -p agentd --example socket_load --message-format=json-render-diagnostics > artifacts/benchmark-socket-build.jsonl
    # Cargo may use a custom target directory or a configured target triple.
    # Run the artifacts Cargo actually emitted, never a stale ./target binary.
    bench_daemon="$(benchmark_executable artifacts/benchmark-agentd-build.jsonl agentd)"
    bench_workload="$(benchmark_executable artifacts/benchmark-socket-build.jsonl socket_load)"
    benchmark_binaries > artifacts/benchmark-binaries.json
    bench_status=0
    for clients in 1 10 100; do
      for workload in shared disjoint; do
        workload_status=0
        "$bench_workload" "$bench_daemon" "$clients" 100 "$workload" > "artifacts/socket-${workload}-${clients}.json" 2> "artifacts/socket-${workload}-${clients}.log" || workload_status=$?
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
