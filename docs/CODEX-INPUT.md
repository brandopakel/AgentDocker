# Codex input delivery (experimental)

The managed bridge retains a turn's most recent structured failure when later
retry or completion events provide only an unknown error. A recognized newer
failure replaces it; other-turn and unbound events cannot supply the reason.
The turn-local detail is discarded at completion. This preserves an actionable
sign-in or limit message without replaying input or declaring recovery.

## Existing Codex terminals: native queue

AgentDocker connects an existing Codex CLI conversation to its ordinary
human/peer queue. A verified Codex hook starts one detached receiver
automatically. It binds the provider PID and birth, thread, profile, checkout and
daemon endpoint; it neither types into the terminal nor starts or resumes a
conversation. Codex's native `thread/queue/add` route schedules the next
ordinary input and preserves the terminal's unfinished draft and permission UI.

The receiver requires a daemon whose schema includes the controller binding and
answer routing, and Codex's experimental native queue API. An accepted hook
configuration is needed to start a receiver for an existing terminal. Setup
includes `SessionStart`, which verifies identity and starts the receiver without
asserting working/idle activity or consuming hook context. Codex delays this
hook until a turn starts, even after reopening an existing thread, so the
receiver binds at the first turn. A session that starts or reopens with no
prompt emits no hook and stays unbound until its first turn; the owned
`codex-native` launcher (below) binds before the first prompt. MCP startup
supplies no thread/profile identity in its environment, so it cannot safely
infer that binding. The provider requires review and trust of new hook
definitions through `/hooks`. Merely installing MCP or receiving a hook event
does not prove idle wake. An explicit bootstrap-prompt resume binds with the same
logical identity and drains the retained queue.

Codex can host MCP in a detached app-server instead of the TUI process. Each
detached-host tool call is resolved using Codex's `threadId` and `sessionId`
metadata against one existing live native binding in the same canonical provider
profile and checkout. Older Codex versions supply the root session as
`x-codex-turn-metadata.session_id`; when top-level `sessionId` is absent, both
nested `session_id` and `thread_id` must exactly match `threadId`. All supplied
identity fields must agree; null, malformed or conflicting newer fields cannot
fall back to the older representation. Resolution verifies the app-server
process birth and executable. The canonical package-cache path supplies the host
profile when Codex filters `CODEX_HOME` from the MCP environment; an explicitly
supplied profile must agree. It requires the root thread/session to agree and
refuses missing, ambiguous, stale or child bindings. Identity-resolution errors
say `cannot resolve native Codex identity` and retain their underlying cause. A
missing live binding does not establish a daemon connection failure; the daemon
may have answered successfully with only an exited provider generation. This
diagnostic does not create a binding or reconnect a conversation. It creates no
app-server agent and never derives a thread from history or a display name. The
native receiver keeps ownership of input receipts. Once resolved, a native
question cannot fall back to a blocking legacy call if a binding changes during
the request. Backend I/O failures while reading the binding inventory preserve
the `agentd unreachable: <cause>` diagnostic and its cause chain. Local host or
binding validation failures retain the native identity diagnostic.

Native launch configuration flags are passed to both the dedicated server and
its terminal, including supported settings on explicit UUID reopen. Remote resume
restores the conversation's saved permissions: direct overrides of
`approval_policy`, `approvals_reviewer`, `default_permissions`, `sandbox_mode`,
`sandbox_workspace_write`, `permissions` or `network`, including quoted root
spellings, are refused before provider startup. Omit these overrides to reuse the
saved permissions, or start a new conversation with new settings. The launcher
never silently removes a requested restriction. The terminal reads local settings
even in remote mode; dropping those flags can ignore launch-only preferences and
let provider startup persist defaults. The launcher does not rewrite the profile.
Codex separately seeds its screen-reader detection marker directly from the saved
user layer, before CLI overrides. Forwarding options does not prevent that
provider first-use write.

The receiver probes the read-only queue/history APIs before taking ownership.
Hooks keep their normal delivery while that probe is pending or unsupported;
only an accepted daemon binding suppresses their competing reads. An incompatible
provider version on an existing binding keeps the queue and reports a pause.

The receiver also has a hidden `--app-server-record` integration path for an
existing authenticated loopback app-server. Its private immutable record pins the
native terminal and server process births, executable, checkout, profile, thread,
numeric loopback port and capability-file digest. Both processes must select that
exact endpoint in their arguments. The ledger retains the record path and digest;
reconnect verifies both processes and the capability again, then checks the
initialized profile and existing thread. It does not create or resume a
conversation. The WebSocket observer bounds frames and notifications, reads
durable history for receipts, refuses provider requests and cannot answer
approvals. It opts out of streamed message, reasoning, command, file-change, diff
and plan notifications on its own connection, keeping unused output from
exhausting its bounded reader. Closing it closes only its socket. The
`--app-server-record` argument alone launches nothing; the owned `codex-native`
launcher and version-2 birth admission are described below. Without a valid birth
receipt, an unavailable pre-first-message history API refuses delivery.

For a first installation, preview **Tools → Codex → Review setup** (or
`agentdocker setup codex --preview`), then apply the reviewed plan. The plan
installs MCP, hooks and the shared coordination skill; it does not start a
receiver for an unverified conversation. Accept the new hook definitions in
Codex as required by the provider. The first ordinary turn emits the verified
startup hook and bootstraps the receiver automatically. A separate
maintainer-only receiver install is not required. Verify actual receipt for that
session; a new or reopened terminal with no first turn has no receiver until its
first turn, as described above. An unsupported queue API reports its limitation
and does not claim idle delivery.

The private `$AGENTDOCKER_HOME/codex-queue/<agent-id>` record stores the
controller token before binding and the exact input before offering it. An
enqueue reply is only an offer. The receiver records the matching provider
thread/turn/user-item before acknowledging the original AgentDocker message.
Restart recovers that receipt or the exact native queue entry; absence of either
pauses delivery and never authorizes blind resubmission. Competing legacy
consumers are fenced by the daemon. A registered launch descriptor lets the
daemon restart an ended receiver with bounded backoff, without restarting the
provider or waiting for a hook. It pins the receiver's installed release and
contains paths and arguments, not authentication. Delivered message bodies are
discarded from the private ledger; only 128 receipt records are retained.
`inbox --peek` provides administrative inspection of a bound queue.

When a person explicitly reopens the same Codex thread, the verified hook finds
its prior binding. A detached helper waits for the old receiver to exit and asks
the daemon to resume the exact thread/profile/checkout into its original agent
record. The newly registered ID becomes an alias; queued input, token, receipts
and provider limits remain on the original record. The daemon starts the updated
receiver descriptor, whose locked ledger accepts the new process only after
checking the daemon's matching generation and retained token. A live prior
process, different conversation or missing ledger refuses the handoff.

The daemon holds a synchronous question's answer until its route is settled. An
answer handed to that tool stays queued as uncertain until the receiver finds its
exact provider tool receipt. An unoffered answer from a CLI-posted question or
disconnected ask uses ordinary input when the daemon confirms answer routing.
Unknown historical offers require reconciliation.

The receiver does not infer idleness from elapsed time. A direct user turn held
open for a long time keeps peer input in the native queue: the historical turn
query does not establish live TUI idleness (the sidecar can report `notLoaded` and
reconstruct an active turn as `interrupted`, without a completion time), so no
idle deadline is inferred while the exact native queue entry remains present. One
outstanding offer, exact receipts, provider-generation checks and the
missing-entry reconciliation deadline apply. Receipt-history and provider-queue
scans each have a one-minute total bound. A queue offer does not prove provider
consumption or task completion. A live bootstrap marker is preserved; a dead
receiver generation's marker may be replaced after a successful launch, and the
private ledger validates the provider generation independently.

The receiver keeps one outstanding offer and follows Codex's queue polling
cadence, so a burst of messages is delivered durably and in order, with no
low-latency promise.

## Active input in an existing terminal

The native queue starts new turns only at idle, and a bound session suppresses
ordinary hook context. A PreToolUse/PostToolUse hook whose process matches the
bound provider generation asks that same receiver for the next FIFO input over a
private local socket, so an app pause does not wait behind older messages
throughout an active turn. The receiver remains the only daemon queue consumer;
the hook never acknowledges messages itself.

The private receiver ledger reserves the exact hook context before removing its
own native queue entry and returning output. A false or lost removal response, a
lost hook response, or missing exact history keeps the original input for
reconciliation without automatic resubmission. The receiver accepts the matching
complete provider context receipt. Codex records Pre/PostToolUse context as a
developer message tagged `hooks.additional_context`, with a provider item ID and
turn ID, but omits it from `thread/items/list`. A bounded reader therefore
verifies the provider's reported transcript path, profile/session/checkout,
opened file identity and complete records after the pre-offer byte boundary. It
scans a fixed end of that suffix with bounded memory, including large compaction
records. It retains only receipt fields; JSON nesting, object keys and captured
strings have explicit bounds. Malformed or structurally oversized records fail
closed. The whole suffix is checked for duplicate receipts; an incomplete last
record is not evidence. The scan runs off the async executor, but has no fixed
latency promise. It never accepts plain text or untagged messages. The older
`hookPrompt` representation is supported when the API exposes it. Native
`userMessage` receipts remain valid if the original queue entry won the race.
Older ledger versions migrate without changing the token, queue ID, original
input or receipts, and an older receiver refuses a newer ledger version.

The hook waits at most four seconds including coordination and output. Human and
peer text exceeding the 6,000-byte hook context budget stays on the native route;
generated stale notices may omit repeated metadata while retaining the original
ID and complete paths, with an explicit summary label. Permission answers keep
their existing route. Hooks do not type into the terminal, resume another thread,
or choose permission decisions. This is delivery at tool boundaries; a provider
that performs no tool call still controls when the next input is consumed.

The hook shares the receiver's ordinary-input classification: a peer message
whose kind is `answer` is ordinary input, and a human answer may enter hook
context only when the daemon confirms native-queue routing. An uncertain prior
offer is never eligible. Human answers without routing proof remain on the exact
MCP receipt path; the hook does not replay synchronous tool answers.

Hooks of a Codex child (subagent) never act for the root conversation. Child
hooks use their parent's `session_id`, so PID ancestry and that field alone do
not establish which conversation receives hook output. The hook reads `agent_id`
and `agent_type` and skips child hooks before registration, activity reporting,
receiver startup or legacy/native queue access. Either non-null field, including
an empty string, means the hook cannot act for the root. Root hooks omit the
child fields; child hooks supply `agent_id` and an `agent_type` (`default` for an
ordinary child). A receipt that reaches a child rather than the root conversation
does not count, and the root retains the offer. Native hook protocol 2 requires
explicit root scope: older clients that discarded the child identity are refused
before queue access. Both the hook executable and the receiver must be upgraded;
replacing only the receiver cannot restore active-turn hook delivery from an old
client. Idle queue delivery is unaffected. The request carries the original
monotonic deadline, leaving output time within the caller's four-second budget;
delayed or expired requests cannot reserve a head. A timeout after reservation
remains uncertain and is never automatically replayed.

The hook endpoint follows the host socket's owning-user trust boundary. Kernel
peer credentials establish the connecting process, and the claimed hook PID is
authenticated against them before queue access; PID birth and ancestry associate
it with the provider. They do not prove that the provider invoked a genuine hook:
an unsandboxed same-user descendant can call the endpoint itself. The nonce
correlates the response; it is not an authorization credential. Such a caller can
reserve an offer and stall delivery if no provider receipt follows. It cannot
make the receiver acknowledge that offer merely by receiving its context, but a
malicious same-user process can also access the host socket and state directly.
Do not expose this endpoint as a boundary between mutually untrusted local
processes. Restricted agents need OS isolation and the scoped container endpoint;
an invocation secret readable by the same user would not supply that isolation.

The provider's [hook contract](https://learn.chatgpt.com/docs/hooks) supports
additional context without replacing the tool result.

### Retained hook recovery

A lost hook result without an exact native receipt stays unresolved; increasing
transcript capacity cannot turn missing evidence into delivery. An uncertain hook
offer blocks later messages until it is resolved.

`agentdocker codex-queue-resolve --agent <id>` asks the owning receiver for a
read-only preview of its complete retained envelope and a confirmation digest.
After actually reading that message, an operator can run:

```sh
agentdocker codex-queue-resolve --agent <id> --message <message-id> \
  --confirm-read <digest-from-preview> --note 'Read the complete original message'
```

This explicitly records **manual readback**, never a native provider receipt. A
confirmation prints the resolution id alone on stdout (the way every command that
creates something prints its id) and its report on stderr; the preview prints its
JSON report on stdout. The private endpoint checks OS peer identity, daemon
endpoint and provider generation; the digest binds the exact input, hook nonce
and generation. The controller refuses an input still scheduled under its
original client or queue ID, even if its text was edited. A missing project
journal refuses before any disposition is written. The existing provider,
receiver ownership and queue order remain intact.

The manual readback protocol persists the manual intent, journals its resolution
ID, acknowledges that one ID through the existing token-bound inbox, then records
completion. Loss of a journal reply can produce duplicate notes with the same
resolution ID; a lost ACK or reply, or a restart, resumes the same disposition
without another input. At most 256 manual records are retained separately from
the 128 rotating native receipts; reaching that bound refuses further resolution
without discarding the audit. Resolved hook nonces cannot advance a later head.
The receiver serves recovery while paused. A same-user local process can make
this explicit administrative request; this permission does not prove a human or
provider child made it. Windows uses the same generation-bound recovery protocol
over a private named pipe. Kernel peer identity binds the caller and receiver
before retained input is read or acknowledged. Provider receipt and task
completion remain separate from manual readback.

The same read-only preview also exposes a separate `start_confirmation` when an
original native queue entry is retained without a hook offer or provider receipt.
Codex can intentionally hold that entry after interruption even while its public
thread status is idle. An operator who wants to continue that reviewed message
can use the launcher-owned shared server's explicit recovery path:

```sh
agentdocker codex-queue-resolve --agent <id> --message <message-id> \
  --start-queued <start-confirmation-from-preview> --note 'Continue this reviewed queued message'
```

This starts the existing submission ID; it never adds a replacement input. The
receiver checks the live generation, original content and queue head, absence of
an existing receipt, and the daemon's ownership/provider-limit checks. It checks
daemon admission and the current project's pause before persisting an intent and
again after the journal write. Inbox visibility alone does not permit starting
paused work: lifecycle notices must remain readable during a pause. A provider
hold, ownership loss or pause observed after persistence retains the intent
without sending to the provider. These are snapshots before transmission, not an
atomic lock across daemon and provider; a later pause cannot retract an already
transmitted start. The provider itself refuses a newly active or pending turn. A
private ledger persists one prepared start intent before the journal write. If
journaling fails or a subsequent hold refuses the start, an explicit retry reuses
that intent and repeats every live preflight check and the journal step. The
preview keeps the same start confirmation available while transmission has not
begun. Repeated journal notes identify the original intent and the current
confirming operator. Immediately before the provider call, a second durable write
marks transmission started. From that point repeating the same confirmation only
reports the retained intent, including after a crash between the marker and the
actual write. This conservative uncertainty never permits another provider start.
The command prints only the intent ID to stdout; the full result goes to stderr.
Once its ordinary receipt retires the pending entry, an old start request is
refused because there is no retained input; the completed receipt is the delivery
record. Lost replies, refused starts or interrupted journal writes leave the input
retained without an automatic retry or hook handoff. The ordinary exact provider
receipt is still required before acknowledgement. A reply naming a turn is not a
delivery receipt. The path never adds, deletes, reorders or repeats the queued
submission to recover a lost reply: the original submission ID is the only thing
started. A lost provider reply, a closed client, or a crash after the
transmission marker leaves the intent retained as possibly transmitted, and the
only ways forward are the ordinary exact provider receipt or an explicit
manual-readback resolution. An explicit retry is permitted only before
transmission has begun, and then reuses the persisted intent and repeats every
live preflight check. The same generation-bound intent and reconciliation path
applies on every platform.

The receiver ledger (current version 7) migrates versions 2 through 6 without
changing their input or token. Versions before 5 cannot contain the immutable
remote-server descriptor; versions before 6 cannot contain a queued-start intent.
Version-6 intents have no transmission marker, so migration treats every such
intent as possibly transmitted and forbids another start. A version-6 record
containing a fabricated marker, or a version-7 intent missing its marker, is
refused without modifying the retained file. The receiver lives in a permanent
private release directory. Further receiver operations must use a CLI compatible
with the ledger; never restore an old ledger over already acknowledged input.

Installing an app alone does not change an immutable receiver launch descriptor;
`codex-queue-upgrade` below replaces the receiver.

### Receiver upgrade

`agentdocker codex-queue-upgrade --agent <id>` replaces only the existing
receiver with the CLI release running that command. It requires a matching daemon
that supports `upgrade_controller` (schema 22 or later), checks the live provider
and retained ledger without migrating it, and probes the provider's read-only
native APIs. Only the receiver executable may change; its arguments, environment,
checkout, provider PID/thread/profile, queue, token and receipts remain. The
daemon compares the expected controller and launch, pins the new release, and
commits a replacement intent before its normal supervision stops the old process.
A coordinator crash resumes that intent. Refused writes and a transfer fence leave
the running receiver alone. The successor takes the same lifetime ledger lock and
reconciles pending input; a missing receipt never authorizes resubmission.
Repeating an accepted request cannot stop the successor. The upgrade identity
defaults from the environment.

If the live receiver executable cannot be verified, the daemon refuses without
changing the binding or stopping it. Error details retain the controller
identity, requested path, both canonical lookup results and the expected-file
check from that decision. Lookup errors include their kind and OS code;
arguments, environment and binding tokens are excluded. Two failed lookups never
count as a match.

The schema version prevents an older daemon from opening a retained replacement
intent it cannot understand.

## New managed conversations

New Codex sessions can receive human and peer messages while idle. In New session,
choose Codex and keep the default **Idle messages: On**, or run:

```sh
agentdocker run --runtime codex --codex-input --tty -- codex
```

This starts an owned Codex app-server conversation under the native supervisor.
It needs a matching schema-16 daemon and CLI, and a Codex version supporting
`hooks/list` and paginated thread history. It does not attach to an existing Codex
TUI. The desktop defaults the option on for a new Codex session; it applies only
to that launch. The CLI requires the explicit `--codex-input` flag. Codex's
app-server interface is experimental.

Send through the selected session's message composer, `send_message`, or the
terminal input. All three use the daemon's ordinary `Send` queue. The bridge polls
while idle and starts one ordinary input turn. Once that input has an exact
receipt, busy arrivals use `turn/steer` with the owned active turn's ID. Terminal,
CLI, UI and peer input all follow the same daemon queue; a pending provider
approval or elicitation request retains ordinary input until its answer is resolved.
A synchronous AgentDocker MCP `ask_human` call is separate: ordinary busy input
may steer into its active turn while the human question remains open. Its answer
still requires the exact tool receipt and must not become another user input.
This route applies to the owned bridge, not an independently running native TUI.
Peer content carries its original sender and message ID in an
`agentdocker_message` envelope. Model text is shown in the session terminal;
AgentDocker MCP `send_message` supplies a correlated peer reply.

Terminal lines are limited to 16,000 UTF-8 bytes. Invalid or oversized lines
produce a local error and are skipped; the controller keeps running and accepts
the next complete line. The bounded reader retains its place when provider
events interrupt a partial read. A line of exactly 16,000 bytes is accepted.

Normal managed macOS terminals have a lower limit: the kernel's canonical input
buffer can fill before the bridge sees a long paste, stranding even the next
line. Managed Unix bridges therefore read characters with a bounded append/erase
editor instead. Backspace erases a Unicode grapheme, Ctrl-U clears the draft and
Ctrl-W erases its last word. Terminal navigation escape sequences are ignored;
this is a line prompt, not the native Codex TUI editor. Signals and output
processing retain their terminal settings, restored with input mode on exit.
After an oversized line, all bytes through its newline are discarded, including
edits, so its tail cannot become a separate prompt; the paste is rejected
visibly and the next line is accepted intact.

The provider profile, authentication, hooks, trust and approval policy are
inherited. No profile is rewritten. The session's AgentDocker MCP entry is bound
explicitly to the matching CLI, managed identity and daemon socket through leaf
configuration overrides. Each enabled local AgentDocker MCP entry is required
for this launch, so a startup failure cannot silently leave queued peer replies
without their bound tool; a startup timeout makes `thread/start` refuse and
pauses delivery. Other MCP settings, approval modes and explicitly disabled
entries remain unchanged. Managed identity lookup includes the nearest Codex
app-server process even though inventory hides that helper; the exact managed
role, controller ancestry, kernel birth and physical working-directory checks
still decide whether the MCP server belongs to the session. This is required
because Codex filters the MCP environment; plain inheritance sends tools to the
wrong daemon. `-c`/`--config`, `--enable`, `--disable` and `--strict-config` are
preserved; `-m`/`--model` has an exact config equivalent. Other arguments,
including initial prompts, are refused rather than dropped. Send the first prompt
through AgentDocker. Provider restart follows the launch's explicit restart
policy (`--restart on-failure:2`, for example).

## Delivery and recovery

One private, locked record under `$AGENTDOCKER_HOME/codex-input/<agent-id>` binds
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

These queues are reserved for `provider_inbox`. Legacy inbox reads,
acknowledgements and receiver subscriptions are refused for that mode. Hooks skip
their competing delivery path after checking the actual provider's ownership. The
MCP server hides and refuses its three inbox-consumer tools. The daemon joins
registration from the owned app-server child to its existing controller; it does
not merge ordinary nested Codex sessions by ancestry alone. These are cooperative
local delivery semantics, not an authentication boundary against other programs
running as the same OS user.

## Questions and approvals

In Inbox, command requests show the command, folder and reason with **Allow once**
and **Deny** controls, and **Allow for session** where Codex offers it: its
`acceptForSession`, which lets the same command through without asking until that
Codex session ends. Multiple-choice questions offer buttons and a text field
for a different answer. These choices use the existing human answer queue, retain
other question drafts and become unavailable when the question closes or expires.
A Codex approval or question waits up to 24 hours for the person, as Codex's own
terminal waits until someone answers; only then does it expire, and an expired
approval is a denial.
The structured presentation is checked against the complete fallback question
text, so the native app and CLI describe the same request.

Local command approvals also include the requested connection host/protocol and
concrete additional filesystem/network permissions in that same checked review
text. Read, write and excluded paths are shown completely. The parser follows the
Codex [app-server contract](https://learn.chatgpt.com/docs/app-server):
omitted/null or `local` environments are accepted; other environments and
unsupported permission selectors are refused. When Codex supplies
`availableDecisions`, it must offer `accept` and either `decline` or `cancel`. When
the optional list is absent or null, the controller uses Codex's
[default decision behavior](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/tui/src/approval_events.rs#L56):
`accept` and `cancel`, with the cancellation explanation retained in the human
question. Existing saved reviews retain their original response semantics. The
controller never selects a proposed policy amendment or session-wide approval.
Only exact human `Allow` approves the reviewed operation; other answers deny it.
This uses the existing shared command presentation, with no daemon schema change.
If Codex offers cancellation instead of decline, the card explains that Deny
cancels the request. Delivery-record version 8 retains that exact negative
response; older records cannot claim the cancellation meaning. Recovery preserves
the saved response without automatically sending it again.

Managed-network requests with an exact host/protocol use the existing choice
controls whenever `networkApprovalContext` is present. Optional command and folder
metadata remain context; absent fields are not invented. Display controls,
including bidi controls and line injection, are refused in these fields before
publishing a question. The question shows the destination and any supplied access
details, and explains that approval can cover multiple pending connections to that
destination, as the
[provider contract specifies](https://learn.chatgpt.com/docs/app-server#command-execution-approvals).
Allow selects `accept` for the pending request, never a session or policy grant.
Delivery-record version 9 retains this separate review kind and refuses it in an
older-version record without rewriting bytes. Restored command records cannot
carry a network choice presentation; legacy command records without a
presentation are supported.

Terminal-input approvals (`kind: writeStdin`) have a separate one-time review.
The card shows the existing terminal session and command item, launch directory,
complete escaped input, reason and concrete retained access. Newlines, terminal
controls and bidirectional marks in the input are visibly escaped; the approval
does not change the original input. The directory is explicitly identified as the
launch directory because the terminal's current directory and state may have
changed. **Allow** approves only that input; **Deny** uses the provider's offered
negative decision, with cancellation explained on the card. Session and policy
grants are never selected.

The parser requires one complete `write_stdin --session-id <id> <input>` argument
vector, valid item/callback identities, local execution, and fully reviewable
permissions. It refuses malformed or oversized input, NUL bytes, unsupported
permissions and mixed network/terminal-input requests. Delivery-record version 11
retains the distinct review kind and rejects it in older records without
rewriting them. Human answer correlation, pending peer/human ordering, and
retained uncertain responses use the existing review path; a saved response is
never automatically replayed. Queued peer/human input waits through review and
resumes in the original thread, and the answer is not replayed as ordinary input.
Managed delivery-ledger writes use the shared private atomic snapshot publisher.
Windows replaces the flushed record while preserving open readers; Unix also
flushes the containing directory. A failed publication never advances the
in-memory attempt.

Provider `requestUserInput` bundles reject `isSecret: true` before opening any
ordinary question route. A supplied flag must be a boolean; malformed values,
including strings and null, cannot fall back to persisted ordinary answers.
Masking a question is not proof that the provider or model does not retain its
answer: the provider and model receive the value and may retain it, including in
the provider's private rollout history.

The secret route keeps answers out of AgentDocker's messages, database, events,
delivery-ledger payloads, logs and ordinary drafts. It uses dedicated masked
entry, exact live request/turn/recipient ownership, bounded in-memory storage,
explicit cancellation/expiry and no replay after a disconnect or crash. Before
entry it explains that Codex and the selected model receive the value and may
retain it. Mixed bundles do not send their secret fields through the ordinary
persisted answer route. The bounded local daemon broker, pipe-only administration
commands, managed-provider producer, durable metadata fence and masked desktop
entry are behind `[experimental] secret_input = true` in `agentd.toml` (or
`AGENTDOCKER_EXPERIMENTAL_SECRET_INPUT=1` on the bridge). Default and native
sessions, plus unsupported interactive console modes, refuse secret bundles.
While either kind of review is pending, a conflicting secret/ordinary request
gets an error without replacing the first review or changing its held input
queue. The broker does not save submitted values, but cannot prevent Codex or its
model from repeating them into ordinary output history.

File-change approval is bounded. Inbox lists the complete file operations and
offers **Review changes**, **Allow once**, **Allow for session** and **Deny**.
Allow becomes available after opening the complete diff; Deny remains available
without that extra step. The fallback question retains the same paths, rename
destination, complete diffs, directory and reason. No diff is silently truncated.

The controller correlates the request's item ID with an earlier file-change item
in the same active thread and turn, following the
[documented approval sequence](https://developers.openai.com/codex/app-server#file-change-approvals).
It retains at most 64 items of 32,000 bytes, at most 16 files per review, and a
16,000-byte complete question. Missing, completed, reused, ambiguous, unsupported
or oversized changes are refused. Repeated details after review publication pause
the controller, shut down its owned Codex process and cancel its pending human
routes. The ledger keeps the uncertain review; it does not invent a confirmed
cancellation or discard a possibly transmitted approval. Restart continues to
require review of that retained uncertainty. Turn completion discards old item
snapshots. Non-null `grantRoot` is refused because it can describe session-wide
write authority. Allow sends `accept`; **Allow for session** sends
`acceptForSession`, which Codex takes for any file change: later changes to the
same files apply without asking until that Codex session ends. Both wait for the
complete diff to be opened. Empty diffs are refused. File presentations and their
receipts require delivery-record version 6; an older record cannot claim to
contain them.

Concrete permission requests appear in the same Inbox review. The card shows the
directory, reason, each read/write/excluded path and requested network access.
**Allow for this turn** sends exactly the reviewed permission profile with
`scope: "turn"`, and **Allow for session** the same profile with
`scope: "session"`; **Deny** sends an empty profile for the turn. Only an exact
human Allow or Allow for session grants access, and no choice overrides Codex's
configured automatic review policy.

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
trailing dots/spaces. Broader host-specific path forms are unsupported.
Glob/special selectors, scan-depth settings and remote environments require a
richer review and are unsupported. The omitted/null environment ID and Codex's
reserved `local` ID select this local flow; other IDs are refused. Permission
receipts require delivery-record version 7; older records cannot claim this
review meaning.

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
cursor. Replayed closures remain private until the daemon's checked
replay-complete marker arrives; incomplete replay is retried from the previous
cursor. The worker retains at most 128 pending question events and attempts at
most three reconnects with 100 ms backoff. That allowance resets only after a
connection remains caught up for 30 seconds. A stream the daemon ends because
the subscriber fell behind its live broadcast (a burst of more than 1,024
events, file changes included) counts as such a loss: the durable log still
holds what was missed, so the worker resumes from its cursor within the same
allowance. Unknown event kinds, history found missing at the resume and buffer
overflow pause immediately; no fresh subscription replaces a lost cursor. The
five-second connection/replay/frame bounds apply to each attempt, and shutdown
cancels the worker and its socket. The daemon reload gate is experimental.

Read-only `provider_inbox` calls with an empty acknowledgement list retry up to
three times after transient I/O failures or the five-second request timeout, with
100 ms between attempts. Retries cannot start a replacement daemon. Protocol
errors and explicit daemon refusals stop immediately, as does an uncertain
acknowledgement or malformed response. Each retry reads the retained queue again;
it neither submits a provider turn nor acknowledges a message.

Periodic readiness refresh is bounded diagnostic metadata: a refused or timed-out
refresh leaves the receiver running and its UI state expires after 90 seconds.
This does not relax receipt or acknowledgement durability.

Queue acknowledgements, question publication, activity/receipt writes and other
failed RPCs pause delivery; a lost write response cannot prove whether the daemon
accepted that operation. A restarted controller cancels its known pending human
routes and requires recovery; it never automatically resends an approval. The
private delivery record (version 15) preserves version-3 through version-14
records and accepts version-1/2 records only without recorded question history.
Version 2 could already have discarded older question IDs; those records are
refused without rewriting the file. The record retains eight detailed closed
requests and up to 10,000 older question IDs, and has an 8 MiB total bound.
Reaching a bound preserves the previous record and reports an error.

## MCP answers and URL elicitation

The separate MCP `ask_human` path records an exact completed tool result before
acknowledging its human answer. Each submitted input retains the MCP server
bindings actually installed for that turn. Recovery matches the result's message
ID, sender and complete text to the queued answer and retains its question route.
Configuration read at recovery time cannot prove an older turn's MCP origin; unmatched human
answers stay queued and pause input instead of becoming new turns. Up to 128
detailed MCP receipts rotate only after acknowledgement and a later turn; their
older question routes share the 10,000-ID bound above.

MCP URL elicitation (`mcpServer/elicitation/request`, `mode: "url"`) has a
bounded human route. The request must name the bound thread. A supplied turn
must match an active input; absent/null turn correlation uses the local active
turn when present. While idle, an uncorrelated request instead belongs to the
owned conversation and holds ordinary queued input until provider resolution.
An outstanding uncertain input prevents this idle route. The server, message
and full original destination are reviewed before choosing **Accept**,
**Decline** or **Cancel**. Copy the link and open it in your own browser only if
you consent; AgentDocker neither fetches the page nor sees its input. Terminal
clients use `agentdocker answer <question-id> Accept|Decline|Cancel`. The desktop
has no free-text input for this question, and the daemon refuses other pending
answers before storing them. Copying the link does not answer the request.

Only HTTPS or canonical loopback HTTP addresses with a bounded printable ASCII
URL are supported. Credentials, hidden display controls and ambiguous authorities
are rejected before question publication. The review records the opaque
elicitation ID unchanged, but does not confuse it with Codex's request ID.
Cancellation and five-minute expiry return `action: "cancel"`; an exact human
Accept returns `action: "accept"` with no form content (Codex forwards an empty
`content` object to the MCP server). Provider resolution acknowledges the
decision, not completion of the external website action. There is no page
polling, completion inference or automatic tool retry. Existing uncertain-write
and no-replay recovery rules apply; URL reviews require ledger version 12 or
later and cannot be fabricated in older records. Under the `never` approval
policy Codex declines the URL request before human review. When a provider
request omits its turn ID, the review binds to the locally active turn; a late
request with no turn ID, after an interruption or restart, cannot be tied to its
original turn. Human review is required for every decision.

Unknown callbacks, session-wide file grants, unsupported permission selectors,
extended MCP forms/device-verification elicitation, secret inputs without the
development switch, and oversized requests return an explicit provider error.
Automatic provider review and configured approval policy are not overridden. The
bridge is not a general replacement for the provider terminal.

Ordinary-input receipts and bounded pause reasons are persisted for the desktop.
The selected session shows queue count and the latest receipt; **Review delivery**
opens the reason and recent saved logs without resending input, restarting a
provider or changing the draft. Paused exited sessions remain in **Needs input**.
The latest UI receipt covers ordinary inputs; native question and MCP answer
receipts keep their existing ledger paths.

Post-review peer input may be steered into the existing provider turn, so
distinct message/item receipts do not imply distinct turns.

The provider transport and MCP policy reference are documented by OpenAI in
[App server](https://learn.chatgpt.com/docs/app-server) and
[MCP configuration](https://learn.chatgpt.com/docs/extend/mcp).

## Active-turn steering

The owned bridge retains one additional steering attempt independently of the
message that started the turn. Its complete input is persisted before the
provider call; only an exact item receipt permits queue acknowledgement. Lost
replies retain the attempt for history reconciliation. A definite active-turn
precondition refusal leaves the message eligible for a later ordinary turn: the
server's structured `activeTurnNotSteerable` (a review or compaction turn), or
the two documented preconditions it states only in words. Other errors do not
permit resubmission. A `turn/start` the server answers with an error of its own
is its word that no turn started: the prepared input is dropped and the message
stays queued with the provider's structured reason on the record; a timeout or a
closed pipe is retained for receipt recovery. Both loops refuse an app-server
older than 0.154.0 at `initialize`, naming both versions, before any thread is
touched. Steering needs delivery-record version 10; older records keep their
inputs when upgraded.

A no-active-turn precondition refusal suppresses further steering for the
retained turn. Its completion clears the suppression and the still-queued input
can enter through the ordinary turn-start path. A different-active-turn refusal
instead pauses delivery immediately with a clear reason, keeps the original
receipt and leaves the unsubmitted message queued. It cannot treat that other
turn's completion as the retained turn or acknowledge its refused input. A later
controller restart must recover the retained conversation before becoming ready.
Ambiguous failures retain the durable attempt for receipt reconciliation; they do
not authorize another submission.

The [documented active-turn steering API](https://learn.chatgpt.com/docs/app-server#steer-an-active-turn)
requires an active turn on the target thread and a matching `expectedTurnId`. The
bridge's separate connection to an independently running terminal cannot steer
that terminal's active turn; this is a local compatibility limit, not an
additional ownership requirement in the API contract. The native TUI queue alone
waits for idle; the separate bounded [hook route](#active-input-in-an-existing-terminal)
supplies active-turn input for the existing-session receiver.

The managed bridge records an accepted conversation as the existing managed
agent's `session_id` after Codex accepts its first turn, and confirms that same
identity before recovering a retained attempt. It reuses atomic same-process
registration: agent ID, process generation, owner and project must agree, and
a conflicting session or changed registration reply pauses input. Empty unused
threads are not registered; Codex may replace them on a later launch without
splitting the managed agent. This label also lets token accounting attribute
retained provider usage to the agent/project instead of leaving it unattributed.
Binding after acceptance avoids an additional pre-submission failure window:
an unavailable daemon cannot make this accounting step strand an input that
Codex never received. Existing receipt recovery still prevents replay; the
collector reconciles samples scanned before the label becomes available.

## Owned native launcher

The native shared-server path accepts a private version-2 birth receipt for a
launcher that observed an empty dedicated server and then its sole TUI creating a
new thread. Initial history admission requires both children still belong to that
exact live launcher, matching empty/idle root-thread metadata, an empty native
queue and a receiver ledger with no attempted, completed, failed or manually
disposed input. This establishes an initial anchor without treating any history
error as empty history. Writing the first attempt permanently spends that
allowance; receipt recovery, explicit resolution and upgrades keep normal history
requirements. After a queued input is accepted, its history can briefly remain
unavailable: the exact `-32601` "list_turns is not supported yet" refusal on the
first page gets up to two additional seconds of read-only retries. An actual
history page and exact receipt are still required; this never recreates the birth
allowance, treats an error as empty history, or sends the input again. Other
errors, later-page errors and an expired wait retain normal recovery behavior.
Version-1 records cannot opt into the birth allowance. Codex reconstructs both
timestamps from its current clock while a thread remains unmaterialized. They must
be equal and bounded by the witnessed creation and current read time; they are not
thread identity. Thread/session IDs, canonical checkout, live generations and all
empty state checks remain required.

`AGENTDOCKER_TRACE_NATIVE_STARTUP=1` optionally records fixed startup refusal
stages (launcher generation/child ancestry, empty-thread metadata or queue). It
does not log arguments, capability contents or environment values and does not
change admission or retry a submitted message.

The experimental hidden `codex-native --program <native-codex-executable>`
launcher owns one dedicated authenticated loopback server and its native TUI.
A separate invocation of the same loaded CLI owns those children. The front end
keeps an authenticated private loopback connection open; its closure, including
front-end SIGKILL, requests bounded child cleanup and capability revocation. The
owner verifies its exact parent generation/image before starting a provider.
A resumed receiver may be started by the daemon outside that owner's process
tree. Its paused recovery endpoint checks the exact provider generation once
per second, interrupting the 30-second reconnect delay or an outstanding
recovery client when that generation disappears. Cancellation preserves the
durable input disposition and never acknowledges or retries an uncertain write.
Stalled local handshakes are read concurrently within fixed connection/time
bounds so they do not serialize the owner's startup. The TUI inherits the
terminal directly.

The launcher observes an empty server before launching the terminal, requires the
sole new thread birth, then registers the exact TUI and publishes its private
birth receipt before starting the receiver, so the receiver binds before the first
prompt. The TUI inherits the terminal; the launcher never types a warmup or
answers approvals. Configuration flags after `--` use the existing conservative
app-server mapping. `--profile` and `--cwd` select existing paths without editing
the provider profile. Initial prompts and positional resume commands are refused.
The explicit `--resume <UUID>` option asks the native TUI to reopen that exact
persisted root conversation. The launcher observes the same sole loaded thread,
checks its checkout/root identity, requires normal persisted history, and
publishes a version-1 record with no birth allowance. A unique dead predecessor
hands off the canonical identity, original queued IDs and retained receipts
through the daemon; a live or ambiguous predecessor refuses. The daemon alone
starts the replacement receiver. After the native client exits, a message queued
while offline arrives once under the same canonical agent, thread and profile,
with a new provider and server generation and without a bootstrap prompt.

If receiver binding and its cleanup both fail, the original binding error is
retained with the cleanup failure as additional context. On Unix, the front end
creates a dedicated group for the internal owner and retains its unreaped child
identity until that group is retired. It also owns the capability path before
startup, so an internal-owner crash can revoke it. A controlling terminal is
handed to the owner group directly, with foreground and mode restoration on
cleanup and suspension; stdin is never proxied. On Windows, the authenticated
internal owner enters an unnamed kill-on-close job before starting provider
children. Its non-inheritable handle remains until process exit; daemon startup
happens before admission, and provider descendants cannot autostart a shared
daemon inside the job. A retained private kernel delete-on-close file handle
revokes the capability even when console closure prevents either Rust cleanup
path from running.

The launcher retires its own children on exit, revokes its capability and retains
bounded private server diagnostics. Capability cleanup is owned before fallible
file writes and server creation, so a failed initial spawn also revokes the file.
The launcher reaps its receiver while the terminal continues, allowing
daemon-owned replacement. On Windows it terminates its owned server through the
native process API. Ordinary `codex` and the managed bridge are unchanged by the
launcher.

## Nonsecret MCP form review

Managed `mcpServer/elicitation/request` form requests use typed desktop fields
with explicit Submit, Decline and Cancel controls. Only the bound thread can
ask. Absent provider turn correlation uses the local active turn when present;
while idle it uses the owned-conversation route described above.
Forms support flat strings, numbers/integers, booleans and titled/untitled
single/multiple selections, required fields, defaults and bounds. Email, URI,
date and date-time formats are validated without fetching or changing values.
Unknown constraints, nested schemas, secret/extended formats and device
verification fail explicitly. Explicit `null` schema fields are invalid, including
constraints and defaults; only omission selects the documented fallback.
Idle form and URL reviews require delivery-ledger version 14 or newer. Earlier
records cannot introduce these receipts. The delivery ledger (version 15) and the
daemon state schema also preserve secret-response metadata. A binary rollback to
a schema that predates it is refused before activation; older receivers also
refuse ledger 15 without rewriting it. Keep a compatible receiver for these
conversations, and never restore an older delivery ledger over already
acknowledged input. Pending or uncertain reviews on restart pause delivery;
replies are never replayed. Requests are limited to 16 fields, 32 options,
16,000-byte schemas/responses and 4,096-byte text fields. Schema text-length
bounds count Unicode characters; exceeding the separate byte cap reports its
own error. Numeric magnitudes may not exceed 2^53 − 1. The generated Codex
schema does not carry regex patterns, so such constraints are unsupported rather
than ignored.

Defaults are editable suggestions. Optional fields can be omitted; submitting
shares only included values with the named server. Unsubmitted edits remain in
window memory and disappear when it closes. Existing ordinary drafts are kept
separate. Submitted answers are retained in the conversation; passwords, tokens,
API keys and payment credentials belong in URL elicitation, never these forms.
Terminal users can answer with a JSON object: `agentdocker answer <id>
'{"count":2}'`, or with `Decline` or `Cancel`. Cancel/expiry sends no form content.
An accepted answer requires the original question's exact daemon receipt, and an
uncertain provider write is retained without replay. Peer input stays queued
during review and resumes only after the exact MCP answer, either in the active
turn or a new one. Stale-schema submissions and oversized edits are refused.

## Secret input

A managed-only secret producer and metadata-only restart fence are behind
`[experimental] secret_input = true` in `agentd.toml` (or
`AGENTDOCKER_EXPERIMENTAL_SECRET_INPUT=1` on the bridge). Default sessions refuse
secret bundles. An interactive route additionally requires AgentDocker to control
terminal echo. On Windows, the managed bridge owns a duplicate console input
handle, disables line buffering and echo, and enables virtual-terminal input for
the bounded editor. It preserves Ctrl-C and output behavior and restores the saved
input mode when the guard drops. Non-terminal or unsupported interactive handles
refuse. With virtual-terminal input mode, Ctrl-] arrives as a Win32 keyboard
record, so Windows attachment recognizes the complete key-down record as the local
detach key, including the protocol defaults for omitted parameters; other keyboard
bytes remain unchanged. A split record is bounded to 64 bytes, and an incomplete
Escape prefix is forwarded after 25 ms without further input, or immediately on
EOF/error.

A supported managed terminal discards new typing without echo while a secret route
is open. After it closes, Enter discards the suspended line and returns to the
preexisting draft; buffered text cannot become an ordinary queued message. A
possibly sent answer without a provider resolution stays paused for inspection and
is never resubmitted by restart. If the exact `serverRequest/resolved`
notification is lost after the provider received the answer, the metadata-only
uncertainty fence and original input receipt are retained across a controller
restart and the waiting ordinary message stays queued; read-only history recovery
does not send the secret again or start another model turn. Receipts are
correlated by message ID: same-turn steering may finish before the original turn
and share its post-answer model request.

## Trial drivers

`python3 scripts/native_codex_queue_smoke.py --help` lists the required binary
paths and scenario choices. The driver runs an actual Codex terminal against a
private profile and loopback Responses fixture and saves a sanitized result
beside private traces. Scenarios cover idle wake, draft preservation, mixed
human/peer busy ordering, asynchronous MCP questions, synchronous MCP answers
without duplicate input (`legacy-question`, `legacy-reply`, `migration`),
`posted-question`, `disconnected-question`, a typed HTTP 429 hold with explicit
resumption (`rate-limit`), lost queue replies and retained ambiguous submissions
(`recovery`), and explicit TUI `resume`. `--reload --scenario question`
additionally hands the private daemon over while idle, with an unsubmitted draft,
during mixed-origin busy input and with a pending question, and checks unchanged
provider/controller identities, exact answer receipt and retirement of all
daemons and controllers. Cleanup covers provider startup failure after the
private daemon has started, before the terminal reader or input binding exists.

- `--scenario long-busy` holds a direct user turn for 65 seconds, requires both
  human/peer inputs to remain queued without a receipt or pause, then verifies
  their ordered consumption and receiver crash recovery.
- `--scenario active-hook` adds peer, human project and human global input during
  one busy TUI turn and requires exact receipts, FIFO order and no later replay
  after the baseline idle, draft and crash checks. `--active-peer-kind answer`
  puts a peer answer ahead of the human messages and requires the same exact
  receipts and same-turn delivery.
- `--scenario active-hook-lost` discards one offered hook output and requires the
  original queued IDs to remain paused without receipt or automatic resubmission.
- `--scenario active-hook-resolve` covers dropped hook output, full readback,
  stale-confirmation refusal, a lost reply and receiver restart.
- `--scenario subagent-hook` checks that child tool hooks leave the root queue and
  ledger unchanged and that one exact root receipt names the root turn.
- `--scenario controller-upgrade` with `--initial-receiver-cli` pointing to an
  immutable older CLI keeps a native queue offer pending through replacement and
  requires the same provider and token, old receipts, new exact FIFO receipts and
  clean retirement.
- `--scenario approval-wait` uses a private `on-request` policy and a narrowly
  matched prompt rule, then holds a harmless print command for 65 seconds while
  peer and human input remain queued; the approval-triggering message may retain
  its receipt until the current turn completes. It requires ordered provider
  receipts after one-time approval and complete private-process cleanup.

`python3 scripts/codex_steering_smoke.py --binary-dir target/release
--output <new-directory>` exercises active-turn steering against an actual Codex
and private local model; add `--scenario lost-reply` for recovery,
`--scenario refused` for an injected no-active-turn failure, or
`--scenario changed-turn` for a refusal followed by that other turn's completion.

On native Windows, `scripts/windows_package_smoke.py` runs against an extracted
package: `--native-manifest <manifest> --output <new-directory> --codex
<native-codex.exe>`, with `--codex-scenario automatic` for the product-owned
launcher. Add `--queue-recovery` (requires the automatic scenario) for explicit
queued start and daemon holds; `--mcp-forms` for managed Codex form decisions
(Submit, Decline, Cancel, invalid-answer refusal, held peer input and exact
ordinary-input receipts); `--mcp-urls` for website decisions without opening a
browser (the original destination and callback, no form content, and exact
receipts); `--mcp-idle` for form and website decisions after the model turn
ends; and `--managed-secret` for experimental masked input and synthetic ConPTY
boundaries. The `--mcp-*` and `--managed-secret` options require `--codex`.

These drivers use an actual Codex client with a loopback model fixture and
synthetic terminals; they do not exercise a real account or provider service.
