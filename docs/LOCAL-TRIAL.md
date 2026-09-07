# Local native trial

Yes: use this development Mac for a controlled alpha trial now. Begin with the audited code in private state and a disposable repository; fix the [restore and privacy findings](AUDIT-2026-09-06.md#blocking-findings) before enabling automatic restore or integrating all existing sessions. The test tools and local preview already allow useful native testing without a global service install.

## Stage 1 — Isolated candidate

Select a reviewed commit and record its SHA. Build `agentdocker`, `agentd` and `agentdocker-ui` from that same source; a package version alone is insufficient. Run `bash scripts/verify.sh check` and keep its output. macOS/Linux stable CI and a local pass do not establish Windows support.

From the candidate checkout, create a private trial root and route every test client/app to it:

```sh
trial_dir="$(mktemp -d /tmp/agentdocker-trial.XXXXXX)"
chmod 700 "$trial_dir"
mkdir -m 700 "$trial_dir/state" "$trial_dir/repo"
git -C "$trial_dir/repo" init
export AGENTDOCKER_HOME="$trial_dir/state"
export AGENTDOCKER_SOCKET="$trial_dir/host.sock"
export AGENTDOCKER_NO_AUTOSTART=0
trial_cli="$PWD/target/release/agentdocker"
"$trial_cli" --version
"$trial_cli" runtimes
"$trial_cli" discover
"$trial_cli" ui
```

Keep the current shell and `trial_dir` value for cleanup. Add a small fixture file and commit it before worktree/validation tests. Discovery can list real processes; adopt only deliberately created fixture agents in this stage. Do not run `adopt --all` against an active development machine as a substitute for integration testing. Close the trial window and run `"$trial_cli" daemon stop` in the same environment to stop this daemon; verify fixture writers exited before removing trial files.

The private parent protects the trial even while state-file defaults need hardening. Do not copy credentials into test fixtures, command lines or reports. Do not enable `--restore` outside the dedicated defect probes until R1/R2 are fixed. Test-owned restore probes may intentionally inject failure into a disposable database.

## Stage 2 — Native acceptance scenarios

| Area | Exercise | Pass condition |
|---|---|---|
| Startup and GUI | Open normally and through the CLI, repeat launch, close/reopen, missing/long/symlinked home and unavailable socket | One serving daemon per endpoint; responsive native window; explicit failures; automatic refresh; no required TCP listener or engine |
| Inventory/discovery | Idle CLI, desktop bundle, process start/change/exit, PID reuse, failed scan, app launch without shell PATH | Accurate distinctions and capability labels; failures preserve last known state; no fabricated exits or integration health |
| Managed process | Batch and PTY commands, attach/detach/resize, output without newline, noisy child, natural exit, stop and force-stop with descendants | Input/output work, UI remains responsive, verified owned processes stop and leases survive until confirmed exit |
| Coordination | Two writers on physical aliases, shared readers, FIFO waiting, timeout/disconnect and deadlock | No incompatible admission, cancelled wait disappears, deadlock is explicit, activity names actual blockers |
| Working state | Observe/read, external edit, stale check, reread, watcher outage, journal cursor/reconnect | Changed content cannot be accepted as fresh; gaps are visible; digest/replay boundaries match retained evidence |
| Handoff and validation | Unchanged/changed source, timeout/surviving child, checkpoint, addressed acceptance, lease transfer, wrong recipient and cross-host import | Only matching passing evidence accepted; no fabricated cross-host validation or lease transfer |
| Channels/contests | Membership routing, review changes/approval, current validation versus wrong owner/checkout, reported metric, tie/noise floor | Permission and provenance checks hold; rankings identify reported evidence and never imply an automatic merge |
| Human interaction | Fixture questions/answers, timeout/disconnect, notification permission/tool missing, GUI answer errors | Request lifecycle is accurate; notification failure never becomes a claim that a person saw the message |
| Multiplexers | Existing owned tmux session and new `--in-pane`, invalid flag combinations, session-name collision and immediate exit | Exact session recorded; no unrelated session replaced; terminal ownership and stop/log limits clear |
| Storage/restart | Graceful shutdown, forced daemon death, SQLite write failures, missing executable/cwd, reboot/sleep/wake | No unprotected writer, lost durable transition or duplicate launch; restored writers wait for readiness; explicit stop stays stopped |
| Installation/update | Install current bundle, move/rename it, update between pinned versions, service dry-run, uninstall/rollback | Config references stay valid; live sessions are handled explicitly; old data is backed up and schema compatibility respected |

The existing suite covers many protocol/core cases; a checkbox here becomes complete only when its actual platform/integration evidence is saved. Do not infer graphical runtime or notifications from compilation.

## Stage 3 — One real integration at a time

Use a disposable project and a fresh test session, not all current work. First preview `setup claude-code --dry-run` and `setup codex --dry-run`, inspect only the relevant planned entries, and preserve configuration backups privately. An isolated `AGENTDOCKER_HOME` does **not** relocate the vendor's configuration files: setup uses the actual user/vendor configuration roots. The GUI Set up button currently applies changes directly.

Apply only the chosen adapter when the trial is ready. Ensure the launched hook/MCP server targets the trial endpoint; setting the environment only in a testing terminal does not prove a desktop vendor host inherits it. Verify registry identity, inbox delivery, read observation/stale detection, lease conflict behavior and journal continuity end to end. A `wired` inventory flag alone is not acceptance. Preserve existing unrelated MCP servers/hooks, validate the vendor's actual tool/version, and inspect the diff before restoring a backup so later user changes are not overwritten.

Agent API calls can incur the user's normal provider usage. Keep test prompts short and bounded; recording source/runtime versions and outcomes is more useful than a long uncontrolled session. Do not copy provider credentials or complete private transcripts into GitHub evidence.

## Stage 4 — Install and sustained use

After the blockers and Stage 2 pass, install the verified **agentdocker** bundle in the user's Applications folder and verify its CLI/daemon companions and stable setup paths. Prefer on-demand startup for the first trial. A launchd service install stops an existing managed daemon and can terminate its managed agents, so review the concrete service dry-run and arrange a controlled transition after the current trial is quiescent.

Run a few hours of supervised work, then an overnight soak with 1/10/100 synthetic agents. Record CPU/RSS/descriptor counts, disk/log growth, GUI responsiveness, p50/p95/p99 request/hook latency, stale-warning delay, reconnect and restart outcomes, and watcher gaps. Correctness is blocking; performance thresholds follow repeated baselines. The current socket benchmark measures contention/release, not all these latency scenarios.

Test sleep/wake, logout/login, app closure, daemon crash and planned upgrade separately. Snapshot command relaunch does not restore model context or terminal state; each vendor conversation-resume integration needs its own acceptance test. Keep Bencher uploads private, with matching before/after manifests and exact source identity.

## Stage 5 — Other machines and systems

Transfer the same verified candidate to the second Mac and repeat startup/inventory, one Claude hooks or Codex MCP trial and shutdown. Record architecture and OS version. It has its own registry; do not expect automatic federation. Execute on Intel hardware before claiming Intel runtime validation.

Repeat the graphical and user-service scenarios on target Linux distributions and ARM64/x86-64. Build Windows support before offering a Windows install; then run the same semantic scenarios against Windows IPC/process/terminal/service adapters. WSL running Linux code is not evidence of native Windows desktop support.
