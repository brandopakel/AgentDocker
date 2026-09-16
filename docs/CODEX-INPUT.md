# Codex input delivery (experimental)

## Existing Codex terminals: native queue candidate (September 15)

The current implementation connects an existing Codex CLI conversation to
AgentDocker's ordinary human/peer queue. A verified Codex hook starts one detached
receiver automatically. It binds the provider PID and birth, thread, profile,
checkout and daemon endpoint; it neither types into the terminal nor starts or
resumes a conversation. Codex's native `thread/queue/add` route schedules the next
ordinary input and preserves the terminal's unfinished draft and permission UI.

This candidate requires the schema-20 controller binding and answer migration and Codex's experimental
native queue API (tested with CLI 0.154.0). It is under integration and acceptance
test. The verified `cf64ca3` package is now installed with a schema-20 daemon;
the existing Codex session auto-started its receiver and offered a queued peer
message. That message automatically started the next ordinary turn without
another human prompt, with its exact provider receipt and queue acknowledgement.
This is one installed existing-session acceptance, recorded in the
[native queue evidence](verification/2026-09-15-native-codex-queue.json). An accepted hook
configuration is needed to start a receiver for an existing terminal. Setup now
includes `SessionStart`, which verifies identity and starts the receiver without
asserting working/idle activity or consuming hook context. Actual CLI 0.154.0
delays this hook until a turn starts, even after reopening an existing thread.
The initial-turn hook bootstrap passes; **startup/reopen with no prompt remains
an observed gap**. MCP startup supplies no thread/profile identity in its
environment, so it cannot safely infer that binding. The provider still requires review/trust
of new hook definitions through `/hooks`. Merely
installing MCP or receiving a hook event does not prove idle wake.
The receiver probes the read-only queue/history APIs before taking ownership.
Hooks keep their normal delivery while that probe is pending or unsupported;
only an accepted daemon binding suppresses their competing reads. An incompatible
provider version on an existing binding keeps the queue and reports a pause.

The private `AGENTDOCKER_HOME/codex-queue/<agent-id>` record stores the controller
token before binding and the exact input before offering it. An enqueue reply is
only an offer. The receiver records the matching provider thread/turn/user-item
before acknowledging the original AgentDocker message. Restart recovers that
receipt or the exact native queue entry; absence of either pauses delivery and
never authorizes blind resubmission. Competing legacy consumers are fenced by
the daemon. A registered
launch descriptor lets the daemon restart an ended receiver with bounded backoff,
without restarting the provider or waiting for a hook. It pins the receiver's
installed release and contains paths and arguments, not authentication. Delivered
message bodies are discarded from the private ledger; only 128 receipt records
are retained. `inbox --peek` provides administrative inspection of a bound queue.

When a person explicitly reopens the same Codex thread, the verified hook finds
its prior binding. A detached helper waits for the old receiver to exit and asks
the daemon to resume the exact thread/profile/checkout into its original agent
record. The newly registered ID becomes an alias; queued input, token, receipts
and provider limits remain on the original record. The daemon starts the updated
receiver descriptor, whose locked ledger accepts the new process only after
checking the daemon's matching generation and retained token. A live prior
process, different conversation or missing ledger refuses the handoff.

A real Codex terminal with a private profile and loopback Responses fixture has
passed idle wake, draft preservation, mixed human/peer busy ordering, new
asynchronous MCP questions, old synchronous MCP answers without duplicate input,
a typed HTTP 429 hold with explicit resumption, lost queue replies and retained
ambiguous submissions. These bounded fixtures do not establish paid-account,
long-duration or every-provider acceptance. Combined `7133023` passed the full
962-Rust/70-Python gate and six release-binary trials: disconnected questions,
generic legacy replies, canonical process resume, new MCP questions, rate limits
and ambiguous-submission recovery. The schema-20 historical-answer migration passed the release-binary fixture at
`8831524`: the consumed synchronous answer was reconciled without another turn.
The later `2a7656c` binary source passed the full
964-Rust/70-Python gate. Zero-prompt reopen, final CI and installation remain open;
see the
[delivery audit](MESSAGE-DELIVERY-AUDIT.md). Repeat the trials with
`python3 scripts/native_codex_queue_smoke.py --help` for the required binary paths
and scenario choices. Each run saves a sanitized result beside private traces.

The daemon holds a synchronous question's answer until its route is settled.
An answer handed to that tool stays queued as uncertain until the receiver finds
its exact provider tool receipt. An unoffered answer from a CLI-posted question
or disconnected ask uses ordinary input when the daemon confirms answer routing.
Unknown historical offers still require reconciliation. The fixture includes
`posted-question`, `disconnected-question` and explicit TUI `resume` scenarios;
those passed on the recorded sources. Startup/reopen without a prompt failed the actual lifecycle trial and remains
open; historical schema-19 answer migration passed its bounded fixture.
The candidate's former 45-second idle guard caused a real long-busy failure.
At `5f72f37`, a direct user turn held open for over 45 seconds retained peer input
in the native queue, but the receiver incorrectly classified the turn as idle
and paused delivery. Its historical turn query does not establish live TUI
idleness. Live diagnostic trial 27 reproduced it: the sidecar reported
`notLoaded` and reconstructed the active turn as `interrupted`, without a
completion time. The correction removes the inferred idle deadline while the
exact native queue entry remains present. One outstanding offer, exact receipts,
provider-generation checks and the missing-entry reconciliation deadline remain.
A queue offer still does not prove provider consumption or task completion.
Fixed `5aeb651` passed the full 967-Rust/70-Python gate and actual 65-second
long-busy, idle-wake, draft/FIFO and receiver-crash acceptance. A separate
rate-limit hold/resume also passed. The recovery trial passed its functional
checks but failed process cleanup; its raw pass label was incorrect and is not
accepted. The driver now marks every exception failed and rechecks child exit
after a process-group signal error. A deliberate late cleanup exception then
correctly failed with exit 1. The next recovery repeat exposed the fixture
competing with automatic receiver restart; fault injection now takes the
receiver lock and leaves replacement solely to the daemon. PRs #148/#149 are
merged after final evidence, review and CI. Corrected recovery34 at
`bc0ea04` passed both lost-enqueue-reply reconciliation to the original native
queue ID and uncertain-input retention without resubmission, using the real
daemon supervisor. The long-busy defect is fixed and has bounded acceptance;
installed-session verification still gates delivery. Trial 25 was a
separate fixture input failure; trial 26 is the application defect. Both are
retained in the [source-specific evidence](verification/2026-09-15-native-codex-queue.json).
Use the existing driver's `--scenario long-busy` to hold a direct user turn for
65 seconds, require both human/peer inputs to remain queued without a receipt or
pause, then verify their ordered consumption and receiver crash recovery.

## New managed conversations

New Codex sessions can receive human and peer messages while idle. In New session,
choose Codex and tick **Receive messages while idle (experimental)**, or run:

```sh
agentdocker run --runtime codex --codex-input --tty -- codex
```

This starts an owned Codex app-server conversation under the native supervisor.
It needs a matching schema-16 daemon and CLI, and a Codex version supporting
`hooks/list` and paginated thread history. It does not attach to an existing Codex
TUI. The option is off by default and applies only to the new session. Codex's
app-server interface remains experimental.

Send through the selected session's message composer, `send_message`, or the
terminal input. All three use the daemon's ordinary `Send` queue. The bridge polls
while idle and starts one ordinary input turn. Once that input has an exact
receipt, busy arrivals use `turn/steer` with the owned active turn's ID. Terminal,
CLI, UI and peer input all follow the same daemon queue; a pending provider
question retains ordinary input until its answer is resolved. This route applies
to the owned bridge, not an independently running native TUI. Peer content carries its original sender and message ID in an
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

## Questions and approvals

In Inbox, command requests show the command, folder and reason with **Allow once**
and **Deny** controls. Multiple-choice questions offer buttons and a text field
for a different answer. These choices use the existing human answer queue, retain
other question drafts and become unavailable when the question closes or expires.
The structured presentation is checked against the complete fallback question
text, so the native app and CLI describe the same request.

Local command approvals also include the requested connection host/protocol and
concrete additional filesystem/network permissions in that same checked review
text. Read, write and excluded paths are shown completely. The parser uses the
installed Codex 0.154.0 [app-server contract](https://learn.chatgpt.com/docs/app-server):
omitted/null or `local` environments are accepted; other environments and
unsupported permission selectors are refused. When Codex supplies
`availableDecisions`, it must offer `accept` and either `decline` or `cancel`. The controller
uses Codex 0.154.0's [default decision behavior](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/approval_events.rs#L56)
when the optional list is absent/null: `accept` and `cancel`, with the cancellation
explanation retained in the human question. Existing saved reviews retain their
original response semantics. The controller
never selects a proposed policy amendment or session-wide approval. Only exact
human `Allow` approves the reviewed operation; other answers deny it. This uses
the existing shared command presentation, with no daemon schema change. If Codex
offers cancellation instead of decline, the card explains that Deny cancels the
request. Private delivery-record version 8 retains that exact negative response;
older records cannot claim the new cancellation meaning. Recovery preserves the
saved response without automatically sending it again.
Managed-network requests with an exact host/protocol now use the existing choice
controls whenever `networkApprovalContext` is present. Optional command and
folder metadata remain context; absent fields are not invented. Display controls, including bidi controls and line injection, are refused in these fields before publishing a question. The question
shows the destination and any supplied access details, and explains that approval
can cover multiple pending connections to that destination, as the
[provider contract specifies](https://learn.chatgpt.com/docs/app-server#command-execution-approvals).
Allow selects `accept` for the pending request, never a session or policy grant.
Private ledger version 9 retains this separate review kind and refuses it in an
older-version record without rewriting bytes. Restored command records cannot carry a network choice presentation; legacy command records without a presentation remain supported.
The implementation passed 61 focused input tests and the full 974-Rust/70-Python
release gate. A private-profile actual Codex trial denied the connection before
emitting an approval callback; actual managed-network provider, native UI and
final integration acceptance remain open. See the [retained trial](verification/2026-09-11-codex-input-review.json). `writeStdin`,
broader permission forms and elicitation still need their own handling.

Schema 15 also supports bounded file-change approval. Inbox lists the complete
file operations and offers **Review changes**, **Allow once** and **Deny**.
Allow becomes available after opening the complete diff; Deny remains available
without that extra step. The fallback question retains the same paths, rename
destination, complete diffs, directory and reason. No diff is silently truncated.

The controller correlates the request's item ID with an earlier file-change item
in the same active thread and turn, following the [documented approval sequence](https://developers.openai.com/codex/app-server#file-change-approvals)
and the installed Codex 0.153.4 schema. It retains at most 64 items of 32,000 bytes,
at most 16 files per review, and a 16,000-byte complete question. Missing,
completed, reused, ambiguous, unsupported or oversized changes are refused.
Repeated details after review publication pause the controller, shut down its
owned Codex process and cancel its pending human routes. The ledger keeps the
uncertain review; it does not invent a confirmed cancellation or discard a
possibly transmitted approval. Restart continues to require review of that
retained uncertainty. Turn completion discards old item snapshots. Non-null `grantRoot`
is refused because it can describe session-wide write authority. Allow sends
only `accept`; it never sends `acceptForSession`. Empty diffs are currently
refused. File presentations and their receipts require delivery-record version
6; an older record cannot claim to contain them. The [file-review trial](verification/2026-09-12-file-change-review.json)
passed 848 Rust tests, 65 Python checks and 19 native review/draft/restart steps.
Actual Codex Allow created exactly the reviewed fixture file; Deny left it absent.
Each completed three ordered peer/human/peer inputs and retained one provider
identity. The final compact UI rebuild has byte-identical CLI/daemon binaries to
those provider trials. PR #112 passed final CI and actual source review and merged
as `dd0665a`; review corrected schema history and confirmed the intentional
retention of uncertain approval records.

Schema 16 adds concrete permission requests to the same Inbox review. The card
shows the directory, reason, each read/write/excluded path and requested network
access. **Allow for this turn** sends exactly the reviewed permission profile with
`scope: "turn"`; **Deny** sends an empty profile with the same scope. Only exact
human `Allow` grants access. Neither choice requests session scope or overrides
Codex's configured automatic review policy.

The bounded parser accepts concrete absolute paths in legacy `read`/`write`
lists or typed `entries`, including explicit deny entries. It rejects unknown
fields, conflicting or repeated selectors within one representation, relative
paths, control characters, Unicode bidirectional formatting marks, empty/no-op
grants, more than 16 distinct paths and questions exceeding 16,000 bytes. Matching
legacy/typed mirrors emitted by Codex are retained in the grant and shown once.
Dot segments, repeated separators and trailing separators are refused rather
than resolved against a filesystem. Windows drive/UNC comparison keys normalize
ASCII case and separator spelling, so equivalent paths cannot bypass duplicate
or conflicting-access checks; the approved wire profile remains unchanged.
The portable Windows subset requires ASCII names and a complete UNC server/share,
and refuses device namespaces, reserved device names, alternate streams and
trailing dots/spaces. Broader host-specific path forms need a separate review.
Glob/special selectors, scan-depth settings and remote environments require a richer review
and remain unsupported. The omitted/null environment ID and Codex's reserved
`local` ID select this local flow; other IDs are refused. Permission receipts
require delivery-record version 7;
older records cannot claim this review meaning. Codex 0.153.4, installed at trial time, exposed
the request-permissions tool as a disabled feature under development. The
[permission trial](verification/2026-09-12-permission-review.json) at `7051471`
passed 856 Rust tests, 65 Python checks and actual Codex Allow/Deny with three
ordered peer/human/peer inputs each,
one exact human/provider resolution and one controller/conversation. Allow
created the expected private fixture file; Deny left it absent. The corresponding
native trial passed 18 review/draft/schema-upgrade/restart steps, including
matching permission mirrors shown once. Initial environment/mirror failures and
diagnostic setup failures are retained. PR #115 merged on September 13 after CI and review.

Command/file/permission approvals and nonsecret provider questions use AgentDocker's registered
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

A transient loss of the question event connection resumes from its last checked
schema-14 cursor. Replayed closures remain private until the daemon's checked
replay-complete marker arrives; incomplete replay is retried from the previous
cursor. The worker retains at most 128 pending question events and attempts at
most three reconnects with 100 ms backoff. That allowance resets only after a
connection remains caught up for 30 seconds. Unknown event kinds, invalid or
missing history and buffer overflow pause immediately; no fresh subscription
replaces a lost cursor. The existing five-second connection/replay/frame bounds
apply to each attempt, and shutdown cancels the worker and its socket.

Read-only `provider_inbox` calls with an empty acknowledgement list now also
retry up to three times after transient I/O failures or the existing five-second
request timeout, with 100 ms between attempts. Retries cannot start a replacement
daemon. Protocol errors and explicit daemon refusals stop immediately. Each retry
reads the retained queue again; it neither submits a provider turn nor acknowledges
a message. Socket tests verify a discarded read response, bounded exhaustion and
no retry for an uncertain acknowledgement or malformed response. The
[read-reconnect trial](verification/2026-09-12-queue-read-reconnect.json) passed
850 Rust tests, 65 Python checks and an actual Codex read-response cut while file
approval was pending. The previous controller stopped; the correction kept the
same controller/conversation, resolved the approval once and completed three
ordered peer/human inputs. Other connections and the daemon remained live.

Periodic readiness refresh is bounded diagnostic metadata: a refused or timed-out refresh leaves the receiver running and its UI evidence expires after 90 seconds. This does not relax receipt or acknowledgement durability.

Queue acknowledgements, question publication, activity/receipt writes and other
failed RPCs still pause delivery;
a lost write response cannot prove whether the daemon accepted that operation.
The [687e57f reconnect trial](verification/2026-09-12-provider-event-reconnect.json)
passed 839 Rust tests, 65 Python checks and an actual Codex event-connection cut
while command approval was pending. Checked replay resolved that answer once,
kept the same controller/conversation and completed three ordered peer/human
inputs. The daemon and other RPC connections stayed live during this trial. A restarted controller cancels its known pending human routes and
requires recovery; it never automatically resends an approval. The private
version-9 record preserves version-3/4/5/6/7/8 records and accepts version-1/2 records only without recorded question
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
137 native workflow steps. PR #106 merged as `90c9e24` after final CI and
actual source inspection of `1d76a88`; its parent integration gate passed
814 Rust tests and 65 Python checks.

Unknown callbacks,
session-wide file grants, unsupported permission selectors, MCP elicitation, secret inputs
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
`cb17213` checkpoint. PR #105 merged as `7110670` after its 809-test review
correction, final CI and actual source inspection.

The [verified implementation](verification/2026-09-11-codex-input-bridge.json)
passed fourteen targeted tests, the full gate with 784 Rust tests and 65 Python
checks, and 123 native workflow steps. An actual
Codex 0.153.4 trial delivered peer/human/peer input in order, received three
correlated replies and retained one Codex record. The trial used an explicitly
authorized `send_message` tool in its private profile; it does not establish
general approval acceptance. Actual cuts also covered a completed provider turn
before controller completion, an uncertain input before provider submission,
and an unused conversation. Both queue recovery and refusal to replay passed.
Broader crash cuts and sustained use remain gates. Schema 13 also persists
ordinary-input receipts and bounded pause reasons for the desktop. The selected
session shows queue count and the latest receipt; **Review delivery** opens the
reason and recent saved logs without resending input, restarting a provider or
changing the draft. Paused exited sessions remain in **Needs input**. The latest
UI receipt covers ordinary inputs; native question and MCP answer receipts keep
their existing ledger paths. The [status checkpoint](verification/2026-09-12-input-delivery-status.json)
records actual Codex receipt/late-answer acceptance and native restart checks.
PR #108 merged after final CI and actual source inspection at `5778212`. The [delivery audit](MESSAGE-DELIVERY-AUDIT.md)
retains the other open review surfaces and acceptance cases.
The existing installation and active sessions have not been switched.

The provider transport and MCP policy reference are documented by OpenAI in
[App server](https://learn.chatgpt.com/docs/app-server) and
[MCP configuration](https://learn.chatgpt.com/docs/extend/mcp).

### September 14 command access and offered decisions

At clean `8ff5665`, actual Codex 0.154.0 exercised separate Allow and Deny
conversations. Both provider requests offered `cancel` as the negative choice.
Each trial completed three FIFO peer/human/peer inputs with three correlated
model replies, one exact human/provider approval receipt and no fourth input
turn. Original provider configuration and binary hashes stayed unchanged and
owned fixture processes were cleaned up. The full gate at `7d683cc` (identical
Rust code) passed 890 Rust tests, 67 Python checks, lint, packaging and release
build; the native workflow at `8ff5665` passed 170 steps.

The preceding actual Allow attempt at `7f87fe4` was refused before a question
because its decision check required `decline`. A separate wrapper probe failed
managed MCP identity initialization and is not counted as provider acceptance.
Both failures are retained privately. The corrected trials do not complete
provider-limit recovery, unsupported review forms or installed-app acceptance.

The September 15 30-message mixed human/peer burst at `2fa897b`, using the
`2a7656c` release binaries, delivered all 30 turns once and in order after the
baseline and receiver-restart checks (38 total model requests, 36 queue
receipts). The first burst input took 9.03 seconds and the last 299.15 seconds;
the current one-outstanding-offer receiver follows Codex's queue polling cadence.
This proves bounded durability/order, not low-latency burst handling or paid-model
throughput. The retained [native trial record](verification/2026-09-15-native-codex-queue.json)
includes the exact source, driver and binary hashes.

Final review corrections preserve a live bootstrap marker but allow a dead
receiver generation's marker to be replaced after a successful launch; the
private ledger still validates the provider generation independently. Both
receipt-history and provider-queue scans now have a one-minute total bound.
The receiver flags document their verified identity/path inputs. The fixture
serializes bootstrap/question claims and response numbering, including auxiliary
requests. The preceding `153fad3` integration gate passed 965 Rust tests (six
skipped), 70 Python checks and release packaging; the corrected `bb1e50c` source then passed 967 Rust tests (six skipped), 70 Python
checks and the same full gate. The final release-TUI legacy-reply trial at
`b2c3938` then passed idle wake, preserved drafts, mixed-origin FIFO, automatic
receiver restart and exact legacy human-answer consumption without another turn
(nine model requests). Source and executable hashes stayed fixed.

## Active-turn steering acceptance (September 16)

The owned bridge now retains one additional steering attempt independently of
the message that started the turn. Its complete input is persisted before the
provider call; only an exact item receipt permits queue acknowledgement. Lost
replies retain the attempt for history reconciliation. A definite active-turn
precondition refusal leaves the message eligible for a later ordinary turn.
Other errors do not permit resubmission. The local bridge ledger is version 10;
existing version 1–9 records retain their inputs when upgraded.

The installed Codex 0.154.0 API passed an isolated local-model trial: the second
input reached the same active turn, wrong-turn and idle steering were refused,
and no production profile or conversation changed. Source `13c3e40` passed 1,010 Rust tests, 77 Python checks and private actual-Codex
bridge trials for CLI human/peer/broadcast input in one busy turn and supervised
lost-reply recovery without resubmission. Fixture failures and passing reruns are
retained in [input evidence](verification/2026-09-11-codex-input-review.json).
Final integration `7fbb8c4` passed 1,015 Rust tests and 77 Python checks,
including legacy-record migration and duplicate-item receipt rejection. Both
actual-client scenarios passed again on its immutable release binaries; the
lost-reply trial recovered the same agent/thread without repeating input.

Run `python3 scripts/codex_steering_smoke.py --binary-dir target/release
--output /tmp/steering-trial` (add `--scenario lost-reply` for recovery).
Hosted-model and broader provider acceptance remain open. Native TUI
queue delivery still waits for idle; this change does not establish active-input
parity for that existing-session route.
