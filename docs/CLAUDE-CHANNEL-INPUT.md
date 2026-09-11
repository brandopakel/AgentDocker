# Claude channel input

AgentDocker has an opt-in Claude Code input adapter over MCP stdio. It offers
addressed messages from the durable inbox through Claude's channel interface,
including messages sent while no MCP request is running. The ordinary MCP
integration remains available for other providers.

This implementation uses Claude's research-preview channel contract. A fresh
Claude session must enable the MCP entry as a channel and satisfy its provider
consent and organization policy. Merely configuring MCP does not enable input.
See the [official channel contract](https://code.claude.com/docs/en/channels-reference).

## Fresh local trial

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
option makes this an isolated integration trial. Existing sessions must be
relaunched normally to load another configuration.

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
own input handling; ordering relative to channel events needs an actual mixed
input trial before claiming complete submitted-input parity.

## Delivery and recovery

The adapter waits for MCP initialization, then offers one queued envelope with
its complete JSON payload and stable `message_id`, `from_agent`, `kind`,
`sent_at` and `destination` metadata. The model must acknowledge received IDs
using `acknowledge_messages`. That receipt frees the queue head; it confirms
receipt, not task completion. A reply remains a separate `send_message` call.

A stdout write never removes an inbox message. Until an explicit receipt,
delivery is unconfirmed. Claude may silently ignore a channel that was not
enabled; after 30 seconds without a receipt the adapter reports a diagnostic.
The message remains recoverable through a non-draining inbox read. Reconnects
offer the same unacknowledged ID again, so consumers must deduplicate IDs.

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

Fresh actual Claude idle, busy and mixed-input trials remain required. Codex's
supported input adapter, managed launch integration, durable provider-specific
delivery status in the desktop, additional provider versions and sustained-use
acceptance also remain in the [message delivery audit](MESSAGE-DELIVERY-AUDIT.md).
