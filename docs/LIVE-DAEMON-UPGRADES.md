# Safe daemon replacement

Implementation work in progress. The first source prerequisite provides checked
event cursors, bounded replay and an explicit replay-complete boundary. Targeted
store/client/socket tests pass; full candidate acceptance and provider integration
remain pending. Production `reload` continues to refuse without
changing the running daemon or agents. This document records the concrete
ownership and recovery requirements behind the open item in
[Remaining work](REMAINING-WORK.md).

## Current boundaries

`supervisor.rs` owns each native child, terminal, stdout/stderr reader, log writer
and scrollback. Dropping an unreaped `OwnedChild` kills and reaps its group.
`lib.rs` stops managed agents when serving ends. Passing the listening socket and
PTY descriptor does not preserve the other owners or their buffered work.

The existing `OwnedChild::disown` prevents its drop from killing the child, but
does not make the successor its parent. PID monitoring cannot recover its exit
status or replace parent reaping. Replacement must retain a supervising owner
until each existing process exits, including descendant cleanup and exact exit
reporting. The coordinator may retire while that owner finishes its sessions;
the old installation must remain pinned while any such process still uses it.

The Codex controller also treats loss of its question event stream as a delivery
failure and shuts its provider down. Child/descriptor transfer alone therefore
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
3. **Coordinator fencing.** Quiesce mutations and background writers before
   releasing database authority. Exclude an unrelated autostart during transfer.
   Only one coordinator may write. New requests must either complete under a
   known owner or receive explicit retry/recovery semantics; accepted input
   cannot be silently replayed after a lost response.
4. **Successor readiness and recovery.** Validate the intended immutable
   executable and compatible state before transfer. Require the successor's
   serving loop, watcher and session routes to be usable before reporting
   success. A lost readiness response requires inspecting the committed transfer
   identity; timeout alone cannot authorize two coordinators to resume writing.
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
  status and clean descendants before releasing leases.
- Human and peer messages queued before/during transfer retain order and exact
  provider receipts; pending approval answers follow their original route once.
- Active Claude and Codex conversations survive, including an idle wake, a busy
  input, a question and an attached terminal with an unsubmitted draft.
- Wrong/incompatible candidates, unavailable state, lost/trickled readiness,
  candidate death and failed ownership transfer leave one identifiable serving
  coordinator or an explicit recoverable state with protection retained.
- Repeat under log pressure, replay retention limits, concurrent send/stop/launch,
  installation rollback and multiple successive replacements. Test supported
  Unix platforms independently; Windows needs its own ownership/IPC acceptance.

An old installed daemon that lacks this protocol cannot gain live transfer from
an updated launcher. Its first switch still waits for active sessions to finish.
