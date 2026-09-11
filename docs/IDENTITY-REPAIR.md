# Repairing proven legacy identities

This maintenance command reconciles two records of the same external local
Claude or Codex session. Matching names are insufficient: both records must have
the same known PID and process birth, runtime, project and physical checkout;
at least one must identify the provider session. A third matching candidate,
conflicting session labels or incompatible ownership stops the repair.

The implementation passed local validation; final-head CI and review remain.
Production records have not been migrated.
It does not start, stop or signal a daemon or provider. Live/managed sessions,
restorable ownership, containers, scoped credentials and ambiguous identities
need separate resolution; they are not silently retired.

## Preview and apply

Use the intended existing state home and complete IDs from `ps --all --json`:

```sh
agentdocker identity-repair --home /absolute/state/home --keep COMPLETE_ID --retire COMPLETE_OLD_ID
```

Preview reads one database snapshot, reports affected counts and returns a
`plan_sha256`. It leaves records, queues, history, schema and permissions intact.
No installation or provider settings are changed.

After all recorded nonhuman sessions have ended and the daemon has stopped,
repeat the command with that exact digest:

```sh
agentdocker identity-repair --home /absolute/state/home --keep COMPLETE_ID --retire COMPLETE_OLD_ID --apply PLAN_SHA256
```

Apply also checks recorded process births against live processes. A finished
status alone does not authorize modification while its process is alive.
Exclusive SQLite ownership is required before reading the plan for apply;
even an idle daemon connection under a different socket blocks it. Changed
inputs or planned effects invalidate the digest. A failed write rolls back the
entire transaction, including its event and schema change. Repeating the same
completed repair returns its receipt without another move or event.

## What is preserved

- The canonical record retains its ID; the retired ID becomes an exact durable
  alias. Prefixes of retired IDs are not guessed. Both IDs address the retained
  agent after restart, and desktop snapshots carry aliases for current routing.
- Inboxes combine in original arrival order. Only equivalent parsed
  envelopes with the same message ID are deduplicated; conflicting copies or a
  combined queue above 1,000 messages / 4 MiB stop the repair. Original message
  bodies, attribution and destinations remain unchanged.
- Lease holders and typed operational references in channels, questions,
  checkpoints, validation, handoffs and contests move atomically. Overlapping
  protection, newly self-addressed questions/handoffs/reviews, duplicate contest
  entries and unsupported document references stop the repair.
- Read observations combine only when the same path has matching content and
  HEAD. Journal cursors use the lesser position; a missing cursor means unread.
- Journal, change and event history keep original attribution. Agent-filtered
  history queries include both exact identities. Before-images of both records
  and affected operational state remain in `identity_reconciliation` documents.
  Removing the canonical record later removes its current aliases while keeping
  the historical repair archive.

Schema 11 protects these meanings from older binaries. A malformed alias must
fail recovery before agent, event, lease or question recovery changes. Inspection
is bounded to 50,000 rows per operational table, 64 MiB of source data and a
16 MiB before-image archive; a larger installation needs a separately reviewed
migration, not an unbounded in-memory rewrite.

## Acceptance still required

The [recorded standard gate](verification/2026-09-11-identity-repair.json) passed
753 Rust tests (six skipped), 54 Python checks, lint,
packaging and release compilation. The actual CLI/daemon trial passed six steps,
including unchanged preview bytes, stale-digest refusal, idle-connection refusal,
idempotent apply, restarted alias/FIFO access and an acknowledgement through the
retired ID. All owned processes exited. UI tests passed 108 checks, including
former-ID notification navigation and draft preservation. The clean packaged checkpoint passed 114 native workflow steps, 23 notification
routing steps and 11 updater cases. CI and final-head review remain before this
item can close. Applying repair to the
user's actual legacy database remains a separate maintenance operation.
