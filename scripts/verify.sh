#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
python3 scripts/build_storage.py

# One local build campaign at a time. The agents on a shared machine hold
# the exclusive lease `task:local-cargo-campaign` while their cargo runs;
# this script takes it for the caller so a run never starts on top of
# another's. It asks only the machine's own daemon: with AGENTDOCKER_HOME
# pointing elsewhere (a private or test daemon), on CI, or with no daemon
# answering at all, it runs without the lease. AGENTDOCKER_CAMPAIGN_LEASE=off
# skips it too, for a caller that already holds the lease through its own
# tools. Where a daemon answers, the run holds the lease or does not run:
# a lease somebody else holds past the wait, or a run the daemon will not
# let hold one, stops before any cargo process exists, and says why.
#
# The lease needs an agent to hold it. A session's own identity is used
# when the daemon knows it (AGENTDOCKER_AGENT_ID, or the process the CLI
# can tell it is); otherwise this run registers itself as an agent for its
# duration — `verify-<pid>`, external, ended when the run ends. A lease the
# caller already holds is kept and never released here. Whichever it is,
# the lease is renewed while the run lasts, since a cold gate outlives a
# TTL; a renewal that fails means the slot is no longer this run's, and
# the run stops rather than build on top of whoever has it now.
#
# The session variables a managed launch exports (identity, home, socket,
# channel) are taken here and then dropped from the environment: the
# suites run in the environment CI has, where none of them is set.
campaign_dir="$(mktemp -d "${TMPDIR:-/tmp}/verify-campaign.XXXXXX")"
campaign_lease=""
campaign_agent="${AGENTDOCKER_AGENT_ID:-}"
campaign_socket="${AGENTDOCKER_SOCKET:-}"
campaign_home="${AGENTDOCKER_HOME:-}"
campaign_transient=""
campaign_renew_pid=""
unset AGENTDOCKER_AGENT_ID AGENTDOCKER_AGENT_NAME AGENTDOCKER_SOCKET AGENTDOCKER_CLAUDE_CHANNEL_INPUT AGENTDOCKER_HOME
campaign_cli() {
  if [ -n "$campaign_socket" ]; then
    AGENTDOCKER_NO_AUTOSTART=1 agentdocker "$@" --socket "$campaign_socket"
  else
    AGENTDOCKER_NO_AUTOSTART=1 agentdocker "$@"
  fi
}
campaign_holders() {
  campaign_cli leases --resource task:local-cargo-campaign 2>/dev/null | tail -n +2 || true
}
campaign_start() {
  local mode="$1"
  [ "${AGENTDOCKER_CAMPAIGN_LEASE:-on}" = "off" ] && return 0
  # Whatever this run decides covers the scripts it runs in turn (the
  # Python suite drives fixture campaigns through this file); a nested
  # run must not negotiate the slot against its own parent.
  export AGENTDOCKER_CAMPAIGN_LEASE=off
  if [ -n "$campaign_home" ] && [ "${campaign_home%/}" != "${HOME%/}/.agentdocker" ]; then
    return 0
  fi
  [ -n "${CI:-}" ] && return 0
  command -v agentdocker >/dev/null 2>&1 || return 0
  campaign_cli ping >/dev/null 2>&1 || return 0
  local ttl=1800
  case "$mode" in bench|coverage|fuzz) ttl=3600 ;; esac
  # The ids on the resource before asking: a claim by an agent that
  # already holds it renews that lease and answers with the same id, and
  # a lease that was the caller's before this run stays the caller's.
  local before
  before="$(campaign_holders | awk '{print $1}')"
  # `as` is `--as <agent>` when an identity is known, empty otherwise;
  # the expansion below is the form macOS's bash 3.2 accepts under set -u.
  local output as=()
  [ -n "$campaign_agent" ] && as=(--as "$campaign_agent")
  if ! output="$(campaign_cli claim task:local-cargo-campaign ${as[@]+"${as[@]}"} \
      --ttl "$ttl" --wait "${AGENTDOCKER_CAMPAIGN_WAIT:-600}" \
      --note "scripts/verify.sh $mode in $PWD" 2>&1)"; then
    case "$output" in
      *"give --as"*|*"specify the sender"*|*"--as <AGENT>"*|*"no agent matches"*)
        # Nobody to claim as: this run becomes an agent of its own for as
        # long as it lasts, and holds the lease itself. A daemon that will
        # not have it is a daemon that will not have this run.
        local registered
        if ! registered="$(campaign_cli register --name "verify-$$" --runtime custom \
            --pid "$$" --workdir "$PWD" --label campaign=verify.sh 2>&1)"; then
          echo "verify.sh: cannot tell which agent this is and could not register as one (${registered%%$'\n'*}); not starting cargo unheld (if the lease is yours through other means, run with AGENTDOCKER_CAMPAIGN_LEASE=off)." >&2
          campaign_holders >&2
          exit 75
        fi
        campaign_agent="$registered"
        campaign_transient="$campaign_agent"
        echo "verify.sh: registered as verify-$$ ($campaign_agent) for this run" >&2
        as=(--as "$campaign_agent")
        if ! output="$(campaign_cli claim task:local-cargo-campaign ${as[@]+"${as[@]}"} \
            --ttl "$ttl" --wait "${AGENTDOCKER_CAMPAIGN_WAIT:-600}" \
            --note "scripts/verify.sh $mode in $PWD" 2>&1)"; then
          echo "verify.sh: task:local-cargo-campaign is held by another campaign; not starting cargo on top of it." >&2
          echo "$output" >&2
          campaign_holders >&2
          exit 75
        fi ;;
      *)
        echo "verify.sh: task:local-cargo-campaign is held by another campaign; not starting cargo on top of it." >&2
        echo "$output" >&2
        campaign_holders >&2
        exit 75 ;;
    esac
  fi
  local renewing="$output"
  if grep -qx "$output" <<<"$before"; then
    echo "verify.sh: task:local-cargo-campaign was already yours ($output); renewed, not released here" >&2
  else
    campaign_lease="$output"
    echo "verify.sh: holding task:local-cargo-campaign ($campaign_lease) for this $mode run" >&2
  fi
  # Renewed at a third of the TTL, so a long run never lets it lapse under
  # a running cargo. A renewal that fails means the slot is somebody
  # else's now: the helper records the loss and ends the step in flight
  # with everything it started, and the run stops at that step, or
  # before its next, through `step`. The helper holds none of this
  # shell's descriptors and takes its sleep down with it, so nothing
  # waits on it.
  local every="${AGENTDOCKER_CAMPAIGN_RENEW_SECS:-$((ttl / 3))}"
  (
    trap 'kill "$nap" 2>/dev/null; exit 0' TERM
    while :; do
      sleep "$every" & nap=$!
      wait "$nap" || exit 0
      if ! campaign_cli renew "$renewing" ${as[@]+"${as[@]}"} --ttl "$ttl" >/dev/null 2>&1; then
        # The loss is written down, so the run sees it before its next
        # step whatever is running now; the step in flight, if any, is
        # ended with everything it started.
        touch "$campaign_dir/lost"
        if child="$(cat "$campaign_dir/step" 2>/dev/null)" && [ -n "$child" ]; then
          campaign_kill_tree "$child"
        fi
        exit 0
      fi
    done
  ) >/dev/null 2>&1 </dev/null &
  campaign_renew_pid=$!
}
campaign_end() {
  if [ -n "$campaign_renew_pid" ]; then
    kill "$campaign_renew_pid" 2>/dev/null || true
  fi
  local as=()
  [ -n "$campaign_agent" ] && as=(--as "$campaign_agent")
  if [ -n "$campaign_lease" ]; then
    campaign_cli release "$campaign_lease" ${as[@]+"${as[@]}"} \
      --summary "scripts/verify.sh finished (exit $1)" >/dev/null 2>&1 || true
  fi
  if [ -n "$campaign_transient" ]; then
    campaign_cli deregister --as "$campaign_transient" >/dev/null 2>&1 || true
  fi
  rm -rf "$campaign_dir"
}
# This campaign's own process tree, ended from the leaves: the step child
# and whatever it started (cargo's rustc, a linker, a test binary), never
# a process this run did not start.
campaign_kill_tree() {
  local pid="$1" child
  for child in $(pgrep -P "$pid" 2>/dev/null); do
    campaign_kill_tree "$child"
  done
  kill -TERM "$pid" 2>/dev/null || true
}
# One step of the run. A lost slot is a failure here — before the step,
# so nothing starts on another's campaign, and after it, so a step the
# helper ended reads as the loss it was — with the step's pid on record
# meanwhile for the helper to end.
step() {
  if [ -e "$campaign_dir/lost" ]; then
    echo "verify.sh: the build slot was lost; not starting: $*" >&2
    return 75
  fi
  "$@" &
  local child=$! status=0
  echo "$child" > "$campaign_dir/step"
  wait "$child" || status=$?
  rm -f "$campaign_dir/step"
  if [ -e "$campaign_dir/lost" ]; then
    echo "verify.sh: the build slot was lost during: $*" >&2
    return 75
  fi
  return "$status"
}
trap 'campaign_end $?' EXIT
campaign_start "${1:-check}"

case "${1:-check}" in
  check)
    # The documentation is part of what is verified: the index is complete,
    # links resolve, the verification index is current, and a code change
    # against main came with a documentation change or a commit saying why.
    if base="$(git merge-base HEAD origin/main 2>/dev/null)"; then
      step python3 scripts/docs_check.py --base "$base"
    else
      step python3 scripts/docs_check.py
    fi
    step cargo fmt --all --check
    step cargo clippy --locked --workspace --all-targets -- -D warnings
    step cargo nextest run --locked --workspace --profile ci
    step cargo test --locked --workspace --doc
    step python3 -m unittest discover -s tests
    step cargo package --locked --workspace --no-verify --allow-dirty
    step cargo build --locked --workspace --release
    ;;
  test)
    step cargo nextest run --locked --workspace --profile ci
    step cargo test --locked --workspace --doc
    ;;
  coverage)
    mkdir -p artifacts
    step cargo llvm-cov nextest --locked --workspace --profile ci --lcov --output-path artifacts/coverage.lcov
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
    step cargo bench --locked -p agentdocker-core --bench leases -- --noplot 2>&1 | tee artifacts/criterion-leases.txt
    step cargo bench --locked -p agentdocker-host --bench fingerprint -- --noplot 2>&1 | tee artifacts/criterion-fingerprint.txt
    step cargo build --locked --release -p agentdocker --bin agentd --message-format=json-render-diagnostics > artifacts/benchmark-agentd-build.jsonl
    step cargo build --locked --release -p agentd --example socket_load --message-format=json-render-diagnostics > artifacts/benchmark-socket-build.jsonl
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
    step cargo +nightly metadata --locked --manifest-path fuzz/Cargo.toml --format-version 1 > /dev/null
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
      step cargo +nightly fuzz run "$target" -- -max_total_time="$seconds" -max_len="$max_len" -timeout=10 -rss_limit_mb=1024 -print_final_stats=1 -verbosity=0 2>&1 | tee "artifacts/fuzz-$target.log"
    done
    ;;
  *) echo 'usage: bash scripts/verify.sh [check|test|coverage|bench|fuzz]' >&2; exit 2 ;;
esac
