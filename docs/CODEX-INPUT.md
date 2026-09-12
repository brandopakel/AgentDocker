# Managed Codex input (experimental)

New Codex sessions can receive human and peer messages while idle. In New session,
choose Codex and tick **Receive messages while idle (experimental)**, or run:

```sh
agentdocker run --runtime codex --codex-input --tty -- codex
```

This starts an owned Codex app-server conversation under the native supervisor.
It needs a matching schema-12 daemon and CLI, and a Codex version supporting
`hooks/list` and paginated thread history. It does not attach to an existing Codex
TUI. The option is off by default and applies only to the new session. Codex's
app-server interface remains experimental.

Send through the selected session's message composer, `send_message`, or the
terminal input. All three use the daemon's ordinary `Send` queue. The bridge polls
while idle, starts one ordinary input turn, and leaves busy arrivals queued in
order. Peer content carries its original sender and message ID in an
`agentdocker_message` envelope. Model text is shown in the session terminal;
AgentDocker MCP `send_message` supplies a correlated peer reply.

Terminal lines are limited to 16,000 UTF-8 bytes. Invalid or oversized lines
produce a local error and are skipped; the controller keeps running and accepts
the next complete line. The bounded reader retains its place when provider
events interrupt a partial read. The [review trial](verification/2026-09-11-codex-input-review.json)
reproduced the old controller exit and verified the correction with actual
Codex and an owned raw PTY, followed by terminal, human and peer queue receipts.

The provider profile, authentication, hooks, trust and approval policy are
inherited. No profile is rewritten. The session's AgentDocker MCP entry is bound
explicitly to the matching CLI, managed identity and daemon socket through leaf
configuration overrides. Other MCP settings, approval modes and disabled entries
remain unchanged. This is required because Codex filters the MCP environment;
plain inheritance sent early trial tools to the wrong daemon. `-c`/`--config`, `--enable`,
`--disable` and `--strict-config` are preserved; `-m`/`--model` has an exact config
equivalent. Other arguments, including initial prompts, are refused rather than
dropped. Send the first prompt through AgentDocker. Provider restart remains the
launch's existing explicit restart policy (`--restart on-failure:2`, for example).

## Delivery and recovery

One private, locked record under `AGENTDOCKER_HOME/codex-input/<agent-id>` binds
the agent, daemon socket, physical checkout, provider profile and conversation.
The controller durably records a complete input before submitting it. Queue
acknowledgement requires the exact provider thread, turn, item and complete text;
a successful pipe write or a client message ID is not sufficient.

After a supervised restart, paginated provider history can recover an exact
receipt and acknowledge it without starting another turn. An uncertain input,
ambiguous match, changed binding or nonterminal previous turn pauses delivery.
Codex may not have saved an unused conversation yet. A controller that has never
prepared input may replace that unused handle after restart. Prepared or
previously completed input always forbids this reset.
Do not delete the record to make a paused controller retry: that discards its
duplicate-work protection. History, frames, pending requests and retained receipts
are bounded; exceeding a bound reports an error and preserves the pending input.

Schema 12 reserves these queues for `provider_inbox`. Legacy inbox reads,
acknowledgements and receiver subscriptions are refused for that mode. Current
hooks skip their competing delivery path after checking the actual provider's
ownership. Current MCP hides/refuses its three inbox-consumer tools. The daemon
joins registration from the owned app-server child to its existing controller;
it does not merge ordinary nested Codex sessions by ancestry alone. These are
cooperative local delivery semantics, not an authentication boundary against
other programs running as the same OS user.

## Questions and command approvals

In Inbox, command requests show the command, folder and reason with **Allow once**
and **Deny** controls. Multiple-choice questions offer buttons and a text field
for a different answer. These choices use the existing human answer queue, retain
other question drafts and become unavailable when the question closes or expires.
The structured presentation is checked against the complete fallback question
text, so the native app and CLI describe the same request.

Command approvals and nonsecret provider questions use AgentDocker's registered
human question route and the same retained inbox as ordinary input. The controller
records the provider request before publishing its questions. Only the exact
answer accepted by the daemon for that question can resolve it; an explicit
human **Allow** approves one command. Peer answers and ordinary input messages
cannot grant it. The response is recorded before writing to Codex, and its human
answer is acknowledged only after Codex reports that request resolved. Resolution
does not by itself establish that the approved command executed.

Human answers resolve their active provider request without becoming another
ordinary input turn. Busy human and peer messages keep their relative order.
Cancellation and expiry reject later replies. Recent extra replies are retained
as unapplied responses; after detailed receipts rotate, a reply naming an older
question pauses delivery with that message still queued. Question IDs survive
restart and are never silently discarded to make room.

Loss of the question event stream or an uncertain publication/response pauses
delivery. A restarted controller cancels its known pending human routes and
requires recovery; it never automatically resends an approval. The private
version-5 record preserves version-3/4 records and accepts version-1/2 records only without recorded question
history. Version 2 could already have discarded older question IDs; those records
are refused without rewriting the file. The current record retains eight detailed
closed requests and up to 10,000 older question IDs, and has an 8 MiB total bound.
Reaching a bound preserves the previous record and reports an error.

## Acceptance still required

The separate MCP `ask_human` path now records an exact completed tool result
before acknowledging its human answer. Each submitted input retains the MCP
server bindings actually installed for that turn. Recovery matches the result's
message ID, sender and complete text to the queued answer and retains its
question route. Current configuration cannot prove an older turn's MCP origin;
unmatched human answers stay queued and pause input instead of becoming new turns.
Up to 128 detailed MCP receipts rotate only after acknowledgement and a later
turn; their older question routes share the 10,000-ID bound above.

The [MCP receipt trial](verification/2026-09-11-mcp-answer-receipts.json) retains
the failed extra-input baseline and passing actual Codex normal/crash recovery
cases at `f42a8b0`. The crash cut let Codex consume the tool answer while its
controller was stopped, then recovered the receipt from provider history after
one controller restart. Both cases kept three ordered ordinary inputs and one
provider record. The local gate passed 813 Rust tests, 65 Python checks and
137 native workflow steps. Final CI and source review remain required.

Unknown callbacks,
file/permission approvals without a complete review presentation, secret inputs
and oversized requests currently return an explicit provider error. Complete
those review surfaces before treating the adapter as a general replacement for
the provider terminal. Automatic provider review and configured approval policy
are not overridden.

The [provider-question trial](verification/2026-09-11-provider-question-receipts.json)
at `de9d6b2` passed 803 Rust tests, 65 Python checks, 123 native workflow steps and
actual Codex Allow, Deny, cancelled-reply and queued-answer crash cases. Approval
answers did not become extra ordinary turns, and cancellation did not authorize
a later reply. PR #104 merged as `8103a0e` after final CI and source inspection.
The [structured Iced controls](verification/2026-09-11-structured-questions.json)
passed 808 Rust tests, 65 Python checks and 137 native workflow steps; actual
Codex Allow and Deny each passed six rendered control steps at the earlier
`cb17213` checkpoint. Final PR #105 review and CI remain pending.

The [verified implementation](verification/2026-09-11-codex-input-bridge.json)
passed fourteen targeted tests, the full gate with 784 Rust tests and 65 Python
checks, and 123 native workflow steps. An actual
Codex 0.153.4 trial delivered peer/human/peer input in order, received three
correlated replies and retained one Codex record. The trial used an explicitly
authorized `send_message` tool in its private profile; it does not establish
general approval acceptance. Actual cuts also covered a completed provider turn
before controller completion, an uncertain input before provider submission,
and an unused conversation. Both queue recovery and refusal to replay passed.
Broader crash cuts, sustained use and source review remain gates. Delivery and
paused state are visible in the terminal; a compact durable status and
guided recovery surface remain part of the [delivery audit](MESSAGE-DELIVERY-AUDIT.md).
The existing installation and active sessions have not been switched.

The provider transport and MCP policy reference are documented by OpenAI in
[App server](https://learn.chatgpt.com/docs/app-server) and
[MCP configuration](https://learn.chatgpt.com/docs/extend/mcp).
