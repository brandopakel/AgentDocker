# Coordinate native agents and resume work

These commands use the local daemon. Start with [installation and adapter setup](GETTING-STARTED.md); the examples assume registered agent names such as `writer`, `reader` and `worker`.

## What it solves

**Race conditions.** A lease is an exclusive or shared claim on a *resource key* such as `path:/repo/src`, `branch:feature/x`, or `task:ISSUE-42`. Path keys are hierarchical, so a lease on a directory covers every file beneath it, and file protection uses canonical physical paths, so aliases and agents from different projects cannot obtain separate exclusive claims on one checkout. Separate worktrees can edit independently; logical project-relative paths support cross-worktree overlap analysis. Every lease has a TTL, so a crashed agent can never wedge the system, and the daemon releases held leases when exit is observed. A stop request reports `stopping` and retains protection until then. A refused claim tells the requester exactly who holds what and the note they left.

**Lost context.** The registry makes participating agents visible; leases carry notes about their work. The daemon records best-effort file-change attribution through unexpired exclusive physical leases, otherwise marks a change external. Durable read sets let supported hooks and explicit MCP calls detect changed content, including uncommitted edits, and require rereading before an edit. Generic adopted processes are not automatically observed.

**No common channel.** Messaging is direct (`--to writer`), project-wide (`--to project` reaches everyone working in the same repository), topic-based (`--to topic:repo/reviews`, subscribed with MQTT-style patterns like `repo/#`), or broadcast (`--to all`). Direct and broadcast messages to an agent without a live subscription queue in its inbox, so polling agents (hooks, cron-style loops) and streaming agents both work. Payloads are JSON with a free-form `kind` (`chat`, `task`, `handoff`, `question`, `answer`, `notice`), so agents on different models can agree on a vocabulary without the daemon caring.

## Detect stale context

Before reading files, record what you are about to inspect:

```sh
agentdocker observe --as reader src/lib.rs
# Read src/lib.rs with your normal tool.
agentdocker stale --as reader src/lib.rs
agentdocker reads --as reader
```

`stale` exits unsuccessfully when recorded content changed, including uncommitted edits that leave HEAD unchanged. Observe and reread the reported paths before editing. Claude hooks automate these steps for supported tools after rerunning `agentdocker hook install claude-code`; MCP clients use `observe_paths` and `check_stale` explicitly. Read sets persist across daemon restarts and stay specific to the agent's physical checkout.

## Resume with verified context

```sh
agentdocker validate --as worker -- cargo test --workspace
agentdocker checkpoint --as worker parser-step --task "Fix the parser" \
  --assumption "Inputs use UTF-8" --next "Review boundary cases" --release-leases
agentdocker checkpoints
agentdocker resume --as replacement <checkpoint-id>
agentdocker resume --as replacement <checkpoint-id> --acknowledge
```

Review the returned assumptions, stale paths, and matching validation evidence before accepting. Changed content blocks acceptance; re-establish the affected context and save a new checkpoint. Acceptance persists across restart and binds the handoff to one replacement session. A plain checkpoint never transfers file leases. Validation records identify the code before and after execution and retain the command's log; changed code, failed checks, timeouts, and surviving subprocesses do not count as passing evidence.

## Hand work to another agent

```sh
agentdocker handoff reviewer --as worker --task "Finish the parser" --note "tests are in src/parser.rs" --transfer-leases
agentdocker handoffs --as reviewer
agentdocker resume --as reviewer <handoff-id> --acknowledge
agentdocker export --as worker > bundle.json      # carry it to another host by hand
agentdocker import --as replacement < bundle.json
```

A handoff is a checkpoint addressed to someone, with everything the daemon already knows about the sender bundled around it: the leases it holds, what it read and at which versions, the changes it made, its uncommitted diff when it worked in a worktree, the messages it never read, and its journal entries. The recipient is told by message and accepts with `resume --acknowledge`; that is when ownership moves — leases transfer if the sender asked, the read set is seeded so staleness carries over, and the recipient continues reading the project journal where the sender stopped. An exported bundle imported on another host is accepted the same way, once the content matches; leases never cross hosts.

## Independent worktrees

Use `agentdocker run --isolate --name writer -- codex …` to launch an agent in a linked worktree of its own (branch `agent/writer`, beside the daemon state directory), or use `agentdocker worktree-create --as writer ../agent-work --branch agent/work` to create an independent checkout by hand. `agentdocker overlap` lists the paths that more than one checkout has changed — merge conflicts before they happen — and `--as writer` narrows it to one agent's checkout. Register the source session there, commit its changes and run `validate`; `integrate --as writer ../agent-work --validation <id>` previews integration. Add `--apply` to prepare an uncommitted merge. Review and commit/abort with Git, then release the target lease.

For optional image execution and authenticated checkout mounts, see [container engines](CONTAINER-ENGINES.md).
