# Safe daemon replacement

Implementation work in progress. The first source prerequisite provides checked
event cursors, bounded replay and an explicit replay-complete boundary. Targeted
store/client/socket tests pass. The [9dda825 checkpoint](verification/2026-09-12-event-continuation.json)
passed 837 Rust tests, 65 Python checks and an actual immutable CLI/daemon
restart, question-answer replay and schema-upgrade trial. PR #109 passed final
CI and source review and merged as `ebaba5be`. The provider event worker's
bounded reconnect is implemented with an actual pending-approval cut trial;
PR #110 passed final CI/source inspection and merged as `d89a85c`. Production `reload` refuses without
changing the running daemon or agents unless the daemon was started with
`AGENTDOCKER_EXPERIMENTAL_RELOAD=1`; the gate stays until the acceptance list
below is recorded. This document records the concrete
ownership and recovery requirements behind the open item in
[Remaining work](REMAINING-WORK.md).

## Current boundaries

PR #130 merged independent session owners as `b28d24c`. The reviewed source
`5120f03` passed 911 Rust tests (six skipped), 70 Python checks and all five CI
workflows after final source inspection. Each `agentd --session-owner` keeps
its child/process group, terminal or pipes, logs, scrollback, exact exit status
and release pin across coordinator crashes. Normal explicit daemon shutdown
still stops managed sessions through `stop_all`; live replacement must preserve
them through a separate transfer path. Successor attachment verifies owner and child birth,
fences stale controllers and preserves live leases while contact is uncertain.

Actual private trials covered batch/PTY continuity across a daemon crash,
a verified owner stopped for 12 seconds during successor startup, 80 controller
replacements, installation-lock pinning and durable exit/report retirement.
Disk cleanup completed 28 ms after owner retirement. An earlier test stopped
its successor 13 ms after launch and asserted cleanup prematurely; the original
failure is retained, followed by bounded completion at the same binary. The later distinct-source `880e111` → `5120f03` trial also passed: batch/PTY
children survived a 12-second owner outage with unchanged identities/lease,
40 FIFO messages, 100 ordered log lines per child and exact exit codes 7/3.
All owners and fixture daemons exited. The full coordinated reload sequence
below remains separate work.

The earlier [output-drain checkpoint](verification/2026-09-12-output-drain.json)
remains evidence for the predecessor implementation's pipe/PTY EOF and final
flush behavior, not a current cross-process ownership gap.

The Codex question-event worker now has bounded checked reconnect with
[actual event-only cut evidence](verification/2026-09-12-provider-event-reconnect.json). Other daemon RPC failures still pause delivery and shut its provider
down; a reconnecting event stream cannot reconcile an uncertain accepted write. Child/descriptor transfer alone therefore
cannot preserve a connected conversation. Reconnection needs verifiable durable
event continuity, not just a new socket or a readiness marker.

## Required implementation boundaries

1. **Verifiable event continuation.** Establish a durable state identity and
   sequence cursor before publishing questions. Resume after that exact cursor,
   preserve order across replay/live overlap, and refuse missing history or a
   different database. A readiness marker precedes replay and does not prove
   replay completion. Missing or ambiguous approval events must still pause.
2. **Independent process ownership.** Keep an owner for each existing child,
   its group, terminal, pipes, log and pending I/O. It must report exact exit
   status once and accept identity-bound stop/resize/input commands after the
   coordinator changes. A crash or failed handover must not disarm ownership.
   *Implemented and merged in PR #130:* the session owner (`agentd --session-owner`, see
   [ARCHITECTURE.md](ARCHITECTURE.md#sessions-and-persistence)); bounded actual
   distinct-source batch/PTY continuity passed from `880e111` to `5120f03`,
   including delayed owner contact, queue/lease retention and exact exits.
   Disk exit recovery now requires a durable record before acknowledgement,
   validates the responding agent/owner/child, and cleans the matching report
   under the stable owner lock after retirement. Regression cases cover both
   earlier storage failure and failure during the exit write, a different agent
   answering on the socket, and a newer generation's report. Actual same-binary restart and final integration checks passed at `5120f03`;
   the distinct-source trial above also passed. This does not enable reload.
3. **Coordinator fencing.** Quiesce mutations and background writers before
   releasing database authority. Exclude an unrelated autostart during transfer.
   Only one coordinator may write. New requests must either complete under a
   known owner or receive explicit retry/recovery semantics; accepted input
   cannot be silently replayed after a lost response.
   *In source:* `offer_transfer` / `abort_transfer` / `accept_transfer` on the
   daemon, the single-row `coordinator` table settled by compare-and-set, the
   `Transferring` error for refused mutations, the fence inside every
   write path (`persist`, `store_op`) so tick writers skip too and a
   skipped write is never mistaken for a commit, an offer that waits for
   admitted mutations to finish (a waiting `ask` or `claim --wait` gives its
   place up and takes one back before writing), and a fenced startup that defers recovery
   writes until the successor has accepted; see
   [ARCHITECTURE.md](ARCHITECTURE.md#sessions-and-persistence). Autostart
   exclusion during a transfer rides on the daemon lock the successor will
   inherit in the next phase; until then nothing calls `offer_transfer`
   outside tests.
4. **Successor readiness and recovery.** Validate the intended immutable
   executable and compatible state before transfer. Require the successor's
   serving loop, watcher and session routes to be usable before reporting
   success. A lost readiness response requires inspecting the committed transfer
   identity; timeout alone cannot authorize two coordinators to resume writing.
   *In source, gated (`AGENTDOCKER_EXPERIMENTAL_RELOAD=1`, exactly):*
   `reload` reads the candidate's `--build-info` within 10 s and refuses
   another host or an older state schema before any offer; it offers the
   transfer (the offer's own `backpressure` or `conflict` refusal is passed
   through), spawns the candidate with `--take-over` in its own session,
   sends a FORMAT 2 handover (listening socket, daemon lock, container
   endpoint) over `SCM_RIGHTS`, and waits up to 30 s for *serving*. The
   successor opens the database pending (schema forward, recorded version
   not), reattaches every session owner while still fenced, and only then
   accepts the transfer as its first write, which also records the new
   schema version; it answers *serving* at once, so acceptance and
   readiness are the same moment. A fenced predecessor does not reconnect
   to owners, so the successor's attachment is never superseded. On any
   other outcome the predecessor kills the successor's whole session and
   aborts the offer, and the database is as the predecessor left it; if
   the store says the successor accepted, it was serving, and a death
   after that is a crashed daemon for the service manager. The watcher
   and session routes come up with the successor's normal startup.
   See [ARCHITECTURE.md](ARCHITECTURE.md#sessions-and-persistence).
   Real-binary coverage: `enabled_reload_hands_real_processes_to_a_successor_and_leaves`
   reloads three daemons in a row with a batch and a PTY agent keeping their
   processes and logs. The [successor-readiness record](verification/2026-09-15-successor-readiness.json)
   repeats that chain on release binaries and adds the failing candidates:
   an older-schema candidate refused before any offer (no coordinator row),
   a candidate that died (offer aborted in 13 ms, same daemon serving,
   writes resumed) and one that never answered (aborted at the 30 s
   deadline, same daemon serving, nothing of its session left behind).
5. **Connected clients.** Preserve or resume terminal and question/event streams,
   provider input polls, pending questions and leases across the transition.
   Reconnection must retain drafts, receipts and original question expiry.
6. **Installation integration.** Keep the predecessor/session-owner pins until
   their work ends. Activate only a reviewed candidate, preserve rollback where
   schema compatibility permits it, and report the actual serving version.

## Acceptance before enabling reload

Use immutable distinct-source binaries, private state and owned fixture groups.
The actual predecessor process must retire from coordination during the test;
an in-process Tokio test cannot establish this boundary.

- Batch and PTY children continue through replacement with the same PID/birth,
  complete ordered output and retained logs/scrollback, then report exact exit
  status and clean descendants before releasing leases. *Passed* in the
  [successor-readiness record](verification/2026-09-15-successor-readiness.json):
  two successive reloads with a batch and a PTY agent keeping their child
  pids, exact exits 7/3 under the third daemon, the container endpoint
  inherited; the CI test above repeats the chain.
- Human and peer messages queued before/during transfer retain order and exact
  provider receipts; pending approval answers follow their original route once.
- Active Claude and Codex conversations survive, including an idle wake, a busy
  input, a question and an attached terminal with an unsubmitted draft.
- Wrong/incompatible candidates, unavailable state, lost/trickled readiness,
  candidate death and failed ownership transfer leave one identifiable serving
  coordinator or an explicit recoverable state with protection retained.
  *Passed* for an older-schema candidate, a dying candidate and a silent one;
  trickled readiness and a failed store are covered by unit tests. Wrong-host
  candidates are refused by the same `--build-info` check but have not been
  trialled with a real foreign binary.
- Repeat under log pressure, replay retention limits, concurrent send/stop/launch,
  installation rollback and multiple successive replacements. Test supported
  Unix platforms independently; Windows needs its own ownership/IPC acceptance.

An old installed daemon that lacks this protocol cannot gain live transfer from
an updated launcher. Its first switch still waits for active sessions to finish.
