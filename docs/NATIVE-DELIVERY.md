# Native delivery record

The target is an installed agentdocker desktop app that discovers and coordinates local agents on macOS, Linux and Windows, with optional engines. This record distinguishes implementation, acceptance evidence and release availability. The [September 6 audit](AUDIT-2026-09-06.md) is the original f66cd3f baseline; [LOCAL-TRIAL.md](LOCAL-TRIAL.md) defines the broader acceptance matrix.

## Restore and private state

Implemented after the audit:

- Durable restore preparation commits the starting identity, required leases, retained restore point and ordered events together. A missing/conflicting lease or failed database read/write prevents launch. An interrupted prepared restore can be retried under the same identity. Naturally completed commands stay completed, and expired restore points are not relaunched.
- The host listener is bound before restoration, serving runs alongside it, and a restored managed checkout waits for verified watcher attachment before spawning. Storage, cancellation and required protection are rechecked after asynchronous launch preparation.
- A failed post-spawn completion forces the owned child to stop. Supervision reaps it and waits for its descendants; uncertain exit retains protection. Initial native launches use the same failed-persistence cleanup. This does not make process spawn and SQLite one atomic operation: a child can execute briefly before a post-spawn failure is detected and stopped.
- State/log directories are created as 0700; SQLite/worker/daemon logs as 0600. Existing owned 0755/0644 state is narrowed in place without recursive chmod or truncation. Internal symlinks, hard-linked files, foreign ownership and paths writable by others are refused. CLI/GUI autostart and service activation protect the daemon log before launching agentd. Service status/install previews no longer create state. Restored worker output appends to the existing log.
- State schema 8 records restore intent semantics. Schemas 1–7 upgrade on open; incompatible versions are rejected before schema DDL or journal-mode changes. Keep matching binary/state backups for downgrades.

Regression coverage includes lease/identity/event preparation faults with complete rollback, post-spawn failure cleanup, failed watcher attachment before the first edit, natural completion, conflicting leases, malformed recovery evidence, expired protection, and a crash between restore preparation and spawn. A real daemon/CLI restart test checks first-instruction communication, first-edit observation and private database/WAL/SHM/log modes.

The local standard gate passed 390 Rust tests, five installer tests, doctests, formatting, strict Clippy, package checks and the release build on Apple Silicon macOS, with zero retries and the existing leak-failure threshold. One explicit manual benchmark test was skipped. Historical intermittent leak reports remain recorded in the audit; a passing run does not diagnose them.

These changes do not provide a preserved terminal across daemon replacement, vendor conversation resumption, or enforcement against arbitrary same-user programs. The full trial and review gates still apply; v0.1.0 does not contain these fixes.

## Delivery work still required

1. Current Mac application packaging, verified install/update/rollback, signing/notarization, and Linux desktop packages with graphical acceptance.
2. Guided setup preview/apply/undo, per-tool capabilities and connection diagnostics; broader installation paths and distinct desktop-host identities.
3. Fresh Claude Code hooks and Codex MCP provider round trips, followed by versioned adapter trials; no bulk adoption of active user work.
4. Windows IPC, access control, process/terminal/service/path adapters, installers and native Windows CI/runtime acceptance.
5. Sustained-use restart/dependency/retention and planned-upgrade behavior, then longer controlled soaks and the independent second-Mac trial. Federation remains a later delivery.

The development Mac currently has no Developer ID Application signing identity installed. Build and local preview verification can proceed; public signing/notarization requires the user's developer identity and credentials through local secure configuration. Bencher credentials remain outside the repository and GitHub.
