# Claude channel input

AgentDocker has a Claude Code input adapter over MCP stdio. It offers
addressed messages from the durable inbox through Claude's channel interface,
including messages sent while no MCP request is running. The ordinary MCP
integration remains available for other providers.

This implementation uses Claude's research-preview channel contract. A fresh
Claude session must enable the MCP entry as a channel and satisfy its provider
consent and organization policy. Merely configuring MCP does not enable input.
See the [official channel contract](https://code.claude.com/docs/en/channels-reference).

After initialization and any session verification, the adapter waits one second
before its first queue offer. A September 18 real-provider trace found an offer
written just before Claude registered its channel handler; an uninstrumented
run lost that offer. This short startup settling interval mitigates that race;
it is not a readiness guarantee or a receipt. Control and receipt requests
remain responsive during the wait. Later messages follow the normal polling
cadence. Missing receipts still retain and visibly pause the queue, without
automatic replay based only on an absent transcript entry.

## Launch from AgentDocker

For a new Claude session, open **New session** and choose Claude Code.
**Idle messages: On** is selected by default. You can turn it off for a terminal
session without the channel. Opening the form or choosing a supported provider
restores the On default. Complete Claude's channel consent in the terminal;
organization policy and tool permissions still apply. The CLI equivalent is:

```sh
agentdocker run --runtime claude-code --tty --claude-channel -- claude
```

This launch passes an inline MCP entry and the channel flag to the new process,
using AgentDocker's matching absolute CLI path. It sets the input-mode variable
on the managed parent so hooks cannot race channel delivery. It does not write
provider configuration files or take over existing sessions. Claude may still
update its own usage counters. Other configured MCP entries remain available;
an explicit competing MCP/channel configuration or print-mode command is rejected
before launch. Provider consent and organization policy still apply.

## One entry for every session

Fresh setup generates `--claude-channel` in the Claude MCP entry, and the
inventory/setup checks recognize that exact form. The flag makes the adapter
available; it does not add a channel flag to an already running parent or accept
provider consent. The adapter also recognizes the opted-in parent launch flag;
`agentdocker setup --shell` previews a shell block for future terminal launches. Existing plain MCP entries remain valid and are preserved. Before
relaunching an existing session for idle input, ensure its actual entry includes
`--claude-channel`; the provider launch flag alone cannot enable a plain adapter.

With `--claude-channel` in a user-level MCP configuration, under
a session launched without the input-mode variable and the channel opt-in it
serves the ordinary MCP server (no channel capability, no offers, no owner
lock; the hooks adapter and the tools deliver the inbox as usual) and says so
on stderr, so the same entry fits a session started plainly and one started
for channel input. Only the launch decides.

## Resuming a session

The installed September 18 preview (`6bd97894`, source `705f924`) provides
**Reconnect here** in a Claude session's **Details**. Exit that session normally
with `/exit`, select it under **Earlier sessions**, and press the button. The
app resumes the same conversation under its existing agent record, retaining
its queue, aliases, questions and cards. Its terminal opens in the app for
Claude's own channel consent. The matching inline MCP entry and launch flags
are supplied by the app; no handwritten environment command is needed.
The CLI alternative is `agentdocker reconnect <agent>`. A still-live process,
conflicting subscriber or uncertain persistence refuses the relaunch.

Three sequential actual-Claude trials and one additional final-package trial
preserved the ended identity, delivered its retained message, and answered an
idle project pause in the same chat with verified receipts and no explicit ACK.
The final package passed thirteen installation/rollback checks and is installed
with all six external provider processes unchanged. These bounded private
trials do not certify the user's still-plain sessions: each needs reconnect and
provider consent before its own idle-pause acceptance. Exact sources, receipts,
earlier failed trials and activation are in the
[existing delivery record](verification/2026-09-12-input-delivery-status.json).

For an external relaunch outside the app, the registration path below applies.
A session that comes back as a new process takes up the record it ended
with: the hooks adapter names the session, and the daemon joins the new
process to the ended records of that session in the same checkout, once
their processes are gone. The record that ended last stays, with its id, its
direct conversation and its journal cursor; whatever was still queued for it,
for any earlier ended life of the session and for the new process's own
registration is one queue in durable sequence order with each message once, a
question an earlier life asked is now its own, and every other id becomes an
alias. This applies only when the register/session-resumption eligibility rule
in [ARCHITECTURE.md](ARCHITECTURE.md#wire-protocol) is satisfied. A record whose
process still runs, or one that holds leases or has pending stale notices, is
left as it is. Eligible open channel memberships, opener and reviewer references
move in the same transaction; duplicate memberships collapse to one. A rewrite
that would create a self-review refuses the entire fold. File observations join by path, keeping the latest capture; conflicting
captures at the same time refuse the fold. The joined working set is bounded to
1,000 paths and 4 MiB of stored input. Its rewrite commits with the queue, aliases
and event, so a failed write leaves them all unchanged. Unreadable observations
still disable coordination. An initialized fresh input receiver
also stays separate: it may already have offered its queue head, so folding old
backlog in front would change delivery order. Its old queue remains retained;
this ordering does not establish successful existing-session handover.

So that the hooks get there first, the channel waits for them when the
session asked to resume: the MCP server reads its parent Claude command line
(`--resume <id>`, `-r <id>`, a bare `--resume` picker or `--continue`) and,
when it finds one, answers the MCP handshake as usual but does not report its
readiness — the step that binds input delivery — until the daemon's record for
this very process (the same pid and process birth, both known) carries the
`session_id` the hooks registered, or ten seconds have passed. Each look at
the record is bounded by the transport timeout and cut at the deadline, and is
polled beside the transport, so a slow daemon neither holds up control or
receipts nor stretches the wait. The hook's word is followed even
when it names a different session from the one the command line asked for;
a command-line id is a claim and is never registered as identity. Arguments after `--` are prompt text and do not select the wait. Past the
wait, input is bound anyway and the adapter says so on stderr — the daemon's
guard then keeps the earlier record separate, as before. Control calls, `ping`
and receipts are served throughout; no message is offered before readiness.
A session started fresh binds at the handshake as it always did.

This is how a session started plainly is relaunched with channel input
without becoming a second agent: with the user-level entry carrying
`--claude-channel` (see above), start the same session again as

```sh
AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 claude --resume <session-id> \
  --dangerously-load-development-channels server:agentdocker
```

from the same checkout, after the old process has exited. A managed launch
(`agentdocker run ...`) is a new supervised agent, not a resumption: a
supervised process is its supervisor's to bring back, and the daemon does
not fold a managed record into an unmanaged one or the reverse. A running
session cannot be given a channel; the relaunch is the whole of it.

## Manual local trial

Use the rebuilt CLI; older installed binaries do not have this option. In a
disposable project, write a private MCP configuration using that CLI's absolute
path:

```json
{
  "mcpServers": {
    "agentdocker": {
      "command": "/absolute/path/to/agentdocker",
      "args": ["mcp", "--runtime", "claude-code", "--claude-channel"]
    }
  }
}
```

Start a fresh Claude session with the input-mode variable on the **parent
process**, and enable only this local development entry:

```sh
AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 claude \
  --strict-mcp-config --mcp-config ./channel-mcp.json \
  --dangerously-load-development-channels server:agentdocker
```

Complete Claude's displayed consent for this trusted local server. The
development flag bypasses its channel allowlist for this entry; it does not
bypass organization policy or general tool permissions. The `--strict-mcp-config`
option limits which MCP entries load; it does not isolate the provider profile.
Existing sessions must be relaunched normally to load another configuration.
Plain reconfiguration alone does not prove an old AgentDocker record's queue
was transferred. The **Reconnect here** flow described above resumes an eligible
ended record with its conversation and queue; it refuses live or unsafe cases.
Installed consent and idle receipt passed for the recorded managed session.
Do not restart a working session merely to make the indicator green.

Actual-provider acceptance uses an owned `CLAUDE_CONFIG_DIR` and private daemon
home/socket, monitors existing provider configuration for changes, and reuses
existing authentication only in the child environment. It must not edit the
user's existing provider profile or restart their working sessions.

From a separate client targeting the same daemon, send to the registered agent:

```sh
agentdocker send --from user --to <agent-name-or-id> "Your message"
```

Peer `send_message` calls and these user sends enter the same durable inbox and
channel path. Direct typing into Claude's terminal still belongs to Claude's
own input handling. The September 11 trial exercised both paths during a blocked
tool call; broader ordering, approval, cancellation and starvation cases remain.

## Delivery and recovery

The adapter waits for MCP initialization, then offers one queued envelope with
its complete JSON payload and stable `message_id`, `from_agent`, `kind`,
`sent_at`, `destination` and optional `reply_to` metadata. The model can acknowledge
received IDs using `acknowledge_messages`. Lifecycle hooks also recover a forgotten
ACK when the current session's provider transcript records the exact complete
channel body and metadata followed by a real assistant response in its parent
chain. This accepts both idle channel input and a busy `queued_command` attachment;
an attachment without that continuation is insufficient. The receipt commits
before removing the queue head. It confirms input receipt, not task completion.
Replies use `send_message` to the envelope's `reply_destination` with
`reply_to=message_id`, so project/channel responses appear in their original chat.
A terminal-only response is not an app reply.
The legacy lifecycle-hook path carries the same complete envelope and reply
destination; it no longer strips IDs and routing down to a display-name/text
summary. Hooks alone still require a provider prompt/tool boundary and cannot
wake a plain idle session.

Automatic recovery first reads a 2 MiB suffix, then at most one additional
2 MiB history window if the head's evidence is older. A private per-agent cursor
keeps only the file identity, process/session generation, head ID and offset;
successive hook boundaries advance with a 1 MiB overlap. Changes of head,
generation or source, and truncation, reset the scan. No historical proof is
trusted without rereading it. Each proof is limited to 4,096 records after its
candidate ID, within a 250 ms recovery budget inside the existing one-second
hook budget. It requires the
current process generation and session, and rejects provider errors, synthetic
responses, unrelated turns, sidechains, changed bodies and malformed metadata.
No transcript content is retained. Missing hooks, unknown provider formats,
ambiguous UUIDs and proof chains too large for a window keep the explicit-ACK
fallback and queued input. Recovery clears at most one verified head per hook
boundary; a deep backlog is not consumed in a burst.

A final text-only response may not be visible when `Stop` runs. The hook schedules
one bounded receipt helper after returning control: a per-agent lock, three-second
lifetime and three delayed proof attempts. Each attempt rechecks the actual PID
birth time, registered generation, session and channel ownership. It uses the
same exact-body proof and receipt-before-ACK ordering; it neither submits a prompt
nor fabricates a hook/contact/activity event. Unknown or absent proof stays queued.
This closes the deferred-flush path; installed text-only acceptance is tracked
separately from the earlier tool-call trials.

A stdout write never removes an inbox message. Until a verified receipt,
delivery is unconfirmed. Claude may silently ignore a channel that was not
enabled; after 30 seconds without a receipt the adapter reports a durable
delivery pause naming the outstanding message. That state appears in session
details and send-readiness warnings. It stays paused through periodic refreshes
until that message leaves the queue; fresh transport contact alone does not
clear it. A failed or stalled diagnostic write is bounded and retried, without
blocking the receipt/control path or offering the message again.
The message remains recoverable through a non-draining CLI inbox read.
The channel MCP hides and refuses `read_inbox` and `wait_for_messages` so the
model receives input through the channel queue. Reconnects offer the same
unacknowledged ID again, so consumers must deduplicate IDs.

In channel mode, `ask_human` posts the question and immediately returns
`posted: true` with its `question_id`. It does not return the answer a second
time through its tool response. Finish the current turn while waiting; the
human answer arrives through the normal channel queue with `reply_to` naming
that question. Acknowledge its message ID after receiving its complete content.
Other providers and ordinary MCP mode retain their blocking question tool.

The parent input-mode variable suppresses hook inbox injection even while the
channel reconnects. Hooks also detect a held channel ownership lock for their
agent, preventing an active adapter from racing hook delivery when only the
MCP child's environment carried the variable. Activity and lease operations
continue through hooks. One private lock permits one channel adapter per agent
and daemon home; a duplicate entry exits with an explicit error.

The channel transport permits eight concurrent ordinary RPC calls. Extra
requests receive a visible retryable capacity error. A separate control path
keeps initialization, ping and explicit receipts available while other tools
wait. Input frames are limited to 1 MiB; daemon polls/control requests have a
two-second deadline and output has a five-second write deadline. Bounded
dedicated stdio workers keep blocked pipes out of Tokio's runtime shutdown.
Addressed inboxes retain the daemon's 1,000-message/4 MiB admission limits.

## Acceptance

`scripts/claude_channel_smoke.py` exercises actual daemon and MCP processes:
initialization gating, single-owner detection, retained offers, request pressure,
receipts during waiting calls, daemon/MCP restart, idle transport offers, and
broken or unread stdout. It speaks MCP itself; passing it does not prove that a
Claude model received or acted on a message.

The [September 11 source-pinned trial](verification/2026-09-11-claude-channel-input.json)
used actual Claude Code 2.1.268 with the AgentDocker adapter. Idle peer input
started a turn without another prompt; peer and canonical-user messages queued
during a 45-second tool call, then received ordered explicit receipts and
correlated replies. A terminal prompt submitted during the wait also completed.
A second idle delivery preserved an unsubmitted terminal draft. Four model
receipts/replies and six release-transport scenarios passed.

The first isolated profile had nonessential traffic disabled and reported
channels unavailable. A fresh trial with normal traffic enabled channel consent
and registration. A failed whole-file configuration guard is retained: backup
comparison found only three global Claude usage counters changed during the
concurrent-session trial, despite the private profile. Authentication, MCP
entries, provider settings and the other monitored files were unchanged.
Attribution of those counter changes is unproven; no user files were restored.

The [managed-launch trial](verification/2026-09-11-managed-claude-input.json)
at `78fc835` used `run --claude-channel`, the daemon's managed PTY and normal
`attach`. Its first model turn came from a queued peer message, without a typed
model prompt. A second canonical-user message preserved an unsubmitted draft.
Both received explicit receipts and correlated replies under the original managed
identity; no duplicate Claude registration appeared. The private launch spec
contained no authentication token. The monitored user configuration hashes were
unchanged and the owned processes exited. The UI's separate 26-step rendering
trial covers the checkbox; it does not replace this actual-provider evidence.

The [owned Codex input adapter](CODEX-INPUT.md) and durable desktop receipt
status are implemented. Actual-provider reconnect/ambiguous receipt,
additional versions/policies and sustained-use acceptance remain in the
[message delivery audit](MESSAGE-DELIVERY-AUDIT.md).


### September 12: Claude questions use the normal channel queue

PR #105 merged as `7110670` and PR #106 as `90c9e24` after final CI and actual
source inspections. The [Claude question checkpoint](verification/2026-09-12-claude-question-queue.json)
then reproduced a human answer appearing both in the blocking MCP result and
a channel input without its reply ID. Channel questions now return the posted
question ID immediately; their answers arrive once through the normal queue
with `reply_to`. Explicit model acknowledgements still release accepted input.
The channel MCP also hides/refuses competing inbox-read tools.

At `5819975`, actual Claude 2.1.269 processed peer, human, answer and peer inputs
with four ordered receipts/replies and one provider record. Provider history
shows one answer in the native busy-input attachment and a posted-ID-only tool
result. The local gate passed 815 Rust tests, 65 Python checks, 137 native steps
and seven transport scenarios. The raw-response driver failure and provider
history parser correction remain recorded. User configuration hashes were
unchanged, and all owned fixture processes exited. PR #107 merged as `645e1d5`
after final CI and actual source review of `fb73bb6`; broader sustained/recovery
acceptance remains. The delivery-status follow-up is recorded below.


### Durable delivery status

With schema 13, explicit channel acknowledgements persist a receipt before
removing input from the queue. Iced shows the actual queue count and last receipt;
a failed channel retains a bounded pause reason. **Review delivery** shows that
reason and recent saved logs without changing the queue or current draft.
Paused sessions remain in **Needs input** after exit. An unavailable count from
an older daemon is not shown as zero; a disconnected window labels cached status.

The [status evidence](verification/2026-09-12-input-delivery-status.json) includes
seven transport scenarios and an actual Claude 2.1.269 trial with three ordered
receipts/replies under one identity. That bounded trial used one initial typed
authorization turn. An earlier unprimed trial requested further authorization;
receipt is not permission to act. The retained report explains a controller
replay mistake and the separate read-only audit of saved events/tool calls.
Final CI/source review of this follow-up and broader recovery acceptance remain.

Channel ownership now uses both the agent ID and provider process generation.
If SessionStart folds an uninitialized registration before its channel starts
input, a second MCP entry still cannot acquire another channel under the new
canonical ID. Hooks check the process lock too. `claude_channel_smoke.py --resume`
checks this boundary and the initialized-receiver refusal using real daemon/MCP
processes with fixture provider processes. It does not prove model idle wake or
actual Claude startup ordering. The following dated gates record the subsequent
transport verification; September 18 actual reconnect/idle evidence is separate.

September 17 reconnect review: a provider-generation owner lock supplements the
agent-ID lock when SessionStart folds an MCP-first registration. A channel that
has already initialized is not folded behind its offered head. Source `b5ea76c`
passes the full 1,094-Rust/84-Python gate (seven skipped) and the actual daemon/MCP
transport regression; the older binary admits a second channel and fails. See
[existing channel evidence](verification/2026-09-11-claude-channel-input.json).
At this September 17 checkpoint actual Claude relaunch/model idle wake was
not tested. The September 18 **Reconnect here** evidence above later covers
those cases for its named provider session and candidate.

September 17 startup validation: runtime `a45f831` passed the full gate with
1,149 Rust tests (seven skipped), 94 Python checks (one skipped), formatting,
strict Clippy, doctests, packaging and release build. The parser regression
includes resume-like prompt text after `--`. Local backend fixtures cover both
startup orders, wrong/missing generations, a silent daemon, timeout, and control
and explicit receipts before readiness. These do not establish actual provider
startup/consent or an idle model reply; those remain open in the
[existing channel record](verification/2026-09-11-claude-channel-input.json).

September 17 resumption follow-up: PR #179 keeps the latest capture per path
and eligible open channel memberships in the same transaction as the queue and
aliases. Runtime `3b21d64` passed the full gate (1,149 Rust tests, seven skipped;
94 Python checks, one skipped). The real MCP transport fixture passed with
observations in both lives, preserved room membership, ordered older backlog
and a new room message delivered once through the canonical alias; no fixture
processes survived. Daemon tests also cover storage reopen, newly created
self-review refusal and transaction rollback. The earlier observation-only
fixture failed against the preceding runtime and passed with that fix.
This is source/transport evidence, not installed Claude model acceptance.
The startup ordering guard above remains. Results are in the
[existing channel record](verification/2026-09-11-claude-channel-input.json).

The board integration also migrates a card's typed assignee and creator when
an eligible identity folds. Card text, column, timestamps and archive state stay
unchanged; resumption never grants or renews a task lease. A still-held lease
continues to refuse the fold, and a lapsed hold requires explicit recovery.
Runtime `197bca4` passed the full 1,165-Rust/94-Python gate (seven/one skipped).
The real MCP fixture preserved a completed card through resumption; a separate
private daemon trial reproduced the old task-document refusal and passed on the
fixed binary with its queue and card intact. No fixture processes survived.
Source and driver pins are retained in the existing channel record.
