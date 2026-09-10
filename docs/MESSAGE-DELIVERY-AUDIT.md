# Agent input queues and idle wake audit

Added September 10, 2026 at the user's request. This is a top-priority requirement
in the [active delivery plan](DELIVERY-PLAN.md) and [remaining work](REMAINING-WORK.md).
It is planned work, not a claim that the current adapters already satisfy it.

## Required behavior

An agent-to-agent message must enter the same provider input workflow and queue
as a message the user submits. An idle connected agent must be notified and begin
a turn without waiting for the user to type something or for another hook/tool
event. A busy agent must receive the message according to that provider's normal
queued-input behavior. Messages must retain their sender, destination, message ID
and reply relationship through the entire path.

The same scheduling path must preserve peer attribution and provider permission
rules. Peer text must not become system/developer instructions. Delivery must not
steal focus, submit another person's unfinished draft, or type into an unrelated
terminal. Repeated pings must not create duplicate turns or unbounded reply loops.

## Current evidence and gap

The daemon has inbox queues and live subscriptions. A successful send establishes
routing/queue acceptance, not provider input acceptance or model consumption.
Claude and Codex hooks can inject queued messages at supported lifecycle
boundaries. The actual Codex trial proves prompt/tool/Stop context consumption
and correlated replies for its tested version. Neither hook adapter, by itself,
wakes a provider that is already idle. MCP configuration alone does not cause an
agent to poll, and a waiting inbox tool is a separate delivery path that must be
audited rather than assumed equivalent to normal submitted input.

Current acknowledgements after hook output establish adapter output, not a
receipt from the provider's input queue. This requirement therefore remains open
despite the passing lifecycle-hook tests.

## Audit and implementation sequence

| Order | Work | Reviewable result |
| --- | --- | --- |
| 1 | Trace user submission and peer delivery separately for Claude Code and Codex: desktop terminal input, native provider input, CLI/MCP sends, channels, daemon inbox/live routing, hooks, reads and acknowledgements. | Source-linked sequence diagrams showing exactly where paths join or diverge; runtime/version and managed/external/desktop-host differences. |
| 2 | Inspect each provider's supported submitted-input and wake mechanisms, including what happens when busy, idle, disconnected, awaiting approval or exiting. | Capability matrix backed by local code and official provider interfaces; reproduce the current idle-delivery gap with owned fixtures. Unsupported routes stay explicit. |
| 3 | Design one per-agent submitted-input adapter using the provider's normal input queue. Connect the durable AgentDocker outbox to that adapter and wake an idle connected provider. | Reviewed state machine and prototype demonstrating an idle turn from a peer message, with no user input or incidental hook needed. Do not treat a desktop notification as a provider wake. |
| 4 | Define ordering, admission/backpressure, expiry/cancellation, retries, deduplication and receipts across daemon and provider queues. Preserve queued work through reconnect/restart and identity reconciliation. | Protocol/events/storage/CLI/MCP/GUI changes together. No silent eviction of accepted work; uncertain provider acceptance is visible rather than blindly replayed. |
| 5 | Make delivery state understandable. Distinguish queued locally, accepted by the provider, processing, and failed/expired where evidence supports each state. | Compact UI status and actionable failures. No “delivered” or “read” claim based only on enqueue, subscriber count, hook stdout or a ping. |
| 6 | Exercise actual Claude and Codex conversations, including the user's authorized live Claude peer when available. | Sanitized exact-source results with message IDs, queue transitions, correlated model replies and fixture cleanup; raw prompts/configuration stay private. |

Audit any separate handling of human questions/answers and channel messages as
well as direct peer messages. A bridge must target the canonical session, not a
launcher process or a duplicate transport record. Full Windows and desktop-host
adapters need their own input/wake acceptance; a successful Unix terminal trial
does not establish their behavior.

## Acceptance cases

- **Idle:** send a fresh nonce to an agent waiting for input. Without typing,
  polling from a test prompt or triggering a hook, it starts a turn and returns
  a correlated reply through its normal provider input path.
- **Busy:** send several peer messages while a long turn/tool is running. Verify
  provider queue order and turn boundaries against equivalent manually submitted
  messages; preserve the running operation and outstanding approval state.
- **Mixed input:** interleave user and peer submissions. Verify the documented
  ordering, sender attribution and message IDs, with no starvation or lost draft.
- **Retry/reconnect:** retry the same ID, lose the delivery connection around
  provider acceptance, reconnect MCP/hooks and restart the daemon. Assert no
  lost accepted work or duplicate turns; ambiguous acceptance must remain visible.
- **Unavailable recipient:** disconnected, exited, unsupported and PID-reused
  recipients must not receive invented wake/delivery success or another session's
  message. Reconnection follows the documented expiry/retry policy.
- **Queue pressure:** test bounded queues, slow consumers, message size limits,
  cancellation and expiry. Surface backpressure without silently dropping an
  accepted message.
- **Channels and replies:** fanout wakes only intended members; reply IDs and
  question routes remain correct after reconnect and canonical-identity lookup.
- **Sustained conversations:** alternate and concurrently send between actual
  Claude and Codex sessions, repeatedly entering idle. Measure enqueue-to-provider
  acceptance and acceptance-to-first-turn latency separately; check loop bounds,
  queue/resource growth and cleanup.

This audit and the implemented queue/wake acceptance are required before marking
incoming-message delivery complete. The existing hook bridge and successful
round trips are supporting evidence, not completion of this requirement.
