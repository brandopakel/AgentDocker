# The remote connector: agents that work inside a browser

Status: September 17, 2026. Delivered in source as `agentdocker connector`
(`crates/cli/src/connector/`), with unit tests, a real-binary trial on loopback,
and an acceptance against a real Claude account: through a cloudflared quick
tunnel to the production daemon, claude.ai registered its client, the person
consented with the pairing code, and the browser Claude delivered a message to
keel's terminal sessions ([record](verification/2026-09-12-integrated-desktop.json), `remote_connector_2026_09_17`,
`vendor_acceptance`). ChatGPT is not yet tried. A quick tunnel's hostname is
ephemeral; a named tunnel or reverse proxy is the durable setup.

## Why it exists

Claude's and ChatGPT's browser extensions run an agent in the browser's side
panel. Those sessions run on the vendor's side; nothing on this machine speaks
for them ([the inventory](GUIDE.md#bring-in-the-agents-already-on-the-machine)
finds the extension and says so). When such an agent learns something a terminal
agent should know, the only way in is the one the vendors offer hosted agents:
a remote MCP server over public HTTPS with OAuth. The connector is that server,
kept as small as the requirement allows and as far from the daemon as possible.

## What it is, and is not

- A separate, opt-in process: `agentdocker connector serve`. The daemon never
  listens on the network; the connector talks to it over the local socket like
  any other client.
- Loopback only. It binds `127.0.0.1` and refuses anything else. The public
  hostname and TLS come from a tunnel the person runs in front of it
  (`cloudflared tunnel --url http://127.0.0.1:<port>` needs no account; ngrok,
  Tailscale Funnel or a reverse proxy work the same way). The tunnel's HTTPS
  address is `--public-url`.
- Its own OAuth 2.1 authorization server, with dynamic client registration that
  admits only the vendors' callback URLs.
- One browser-agent identity per consent, in one project, alive until revoked.
- A messaging-only tool surface. A browser agent has no checkout here.

## The flow

1. `agentdocker connector serve --public-url https://<tunnel host>` in the
   project's directory (or `--project <path>`). It prints the MCP URL to paste,
   `<public-url>/mcp`, and a **pairing code** that changes on every run.
2. The person adds `<public-url>/mcp` as a custom connector — Claude: *Settings ›
   Connectors › Add custom connector*; ChatGPT: *Settings › Connectors ›
   Advanced › Developer mode*. The vendor discovers the metadata, registers a
   client, and sends the person to the consent page.
3. The consent page names the vendor and the project, asks for the pairing code
   from the terminal and a name for the agent (a `claude-browser-xxxx` /
   `chatgpt-browser-xxxx` name is suggested), and sends the person back to the
   vendor with a one-time code. Consent alone creates nothing.
4. The vendor redeems the code with its PKCE verifier. That is when the daemon
   registers the browser agent — an external, pidless agent in the project,
   runtime `claude-browser` or `chatgpt-browser`, labels `connector=true` and
   `vendor=<label>` — and the grant and its first tokens exist. A code that is
   never redeemed, or redeemed with the wrong verifier, leaves no agent behind.
5. From then on the hosted agent calls `POST <public-url>/mcp` with its bearer
   token. Every accepted request is a heartbeat for the agent. `initialize`
   tells the model it works inside a browser and has no checkout.

## Endpoints

| Path | Role |
| --- | --- |
| `GET /` | A page saying what this is and how to add it; nothing to run or fetch. |
| `GET /.well-known/oauth-protected-resource[/mcp]` | RFC 9728: `resource` is `<public-url>/mcp`, `authorization_servers` is `[<public-url>]`, the one scope is `agentdocker`. |
| `GET /.well-known/oauth-authorization-server[/mcp]` | RFC 8414: the three endpoints below, `code` only, `authorization_code` and `refresh_token`, PKCE `S256` only, public clients only. |
| `POST /register` | RFC 7591. `redirect_uris` must all be vendors' callbacks (`https://claude.ai/api/mcp/auth_callback`, `https://chatgpt.com/connector_platform_oauth_redirect`, `https://chatgpt.com/connector/oauth/<id>`) or ones passed with `--allow-callback`; `token_endpoint_auth_method` must be `none`. At most 200 clients are kept. |
| `GET /authorize` | Validates the request. A bad client or callback is a page (nothing is sent to an untrusted callback); any other problem goes back to the callback as an OAuth error. Otherwise the consent page. |
| `POST /authorize` | The consent form: pairing code (case, spaces and the dash are forgiven), agent name, and the request's fields. Five wrong codes close consent until the process restarts. A name that is a live agent's is refused on the page. |
| `POST /token` | `authorization_code` (code, `code_verifier`, client and callback must match; single use; five-minute lifetime) registers the agent and issues tokens; `refresh_token` rotates. Errors are RFC 6749 codes: a rotated or revoked refresh token is `invalid_grant`. |
| `POST /mcp` | Streamable HTTP, JSON-RPC in `application/json`, plain JSON out (`202` for a notification). No bearer, an expired or revoked token, or an agent the daemon answers is no longer live: `401` with `WWW-Authenticate: Bearer resource_metadata="…"`, and the grant ends. A daemon that is not answering: `503` with `Retry-After`, and the grant stands. `GET`/`DELETE` are `405`: the server opens no stream. |

Access tokens last one hour; the vendor refreshes on the `401`. Refresh tokens
rotate on every use. Clients and grants persist in
`$AGENTDOCKER_HOME/connector/state.json` (mode `0600`) as hashes — no token is
ever written down — so a connector restart costs the vendor one refresh; codes
and access tokens live in memory only.

## Tools

`whoami`, `list_agents`, `inspect_agent`, `send_message`, `read_inbox`,
`acknowledge_messages`, `ask_human`, `open_questions`, `read_journal`,
`journal_note`, `report_activity`. Everything else — leases, observations,
worktrees, commits, hand-offs, tasks, channels, `wait_for_messages` — is refused
by name with the reason: the agent has no checkout here, and a long wait would
hold an HTTP request. Messages a browser agent sends are attributed input to
their recipients, never instructions; its own instructions say the same about
what it reads on web pages.

## Commands

| Command | What it does |
| --- | --- |
| `connector serve --public-url <https://…> [--bind 127.0.0.1:0] [--project <path>] [--allow-callback <url>]…` | Serve; prints the MCP URL, the pairing code and the vendors' setup steps. Plain `http://` is accepted only for a loopback host, for a trial without a tunnel. |
| `connector grants` | Every consent: agent, runtime, vendor, project, when connected and last used, whether active, and whether the daemon still holds the agent live. |
| `connector revoke <agent>` | Ends the grant (tokens stop) and marks the agent finished. A serving connector notices on that agent's next request. |

## Security limits, stated

- The consent is the only thing that makes an agent, and it needs the pairing
  code from the terminal: the person at the page is the person at the machine.
- Only the vendors' own hosted surfaces can complete the flow, because only
  their callbacks can be registered. Knowing the URL is not enough.
- Every request is bounded: 8 KiB request line, 32 KiB of headers, 1 MiB body,
  15 s to arrive, 64 connections, one request per connection, chunked bodies
  refused. TLS, keep-alive and any client-IP allowlisting are the tunnel's.
- The tool surface cannot touch files, leases, worktrees or commits, whatever
  the hosted model is told by a page it read.
- Not done yet: allowlisting the vendors' egress ranges (Anthropic publishes
  `160.79.104.0/21`) through the tunnel's client-IP header; Client ID Metadata
  Documents (the vendors fall back to DCR); a `connector tunnel` helper that
  runs cloudflared and fills in the URL; connector status and grants on the
  desktop Tools card. These are the second increment.

## What a browser agent cannot get

It is not woken. A message to a browser agent waits in its inbox until the
hosted model calls `read_inbox` — which it does only when the person's prompt
leads it to. Nothing in the vendors' connector model lets a server start a
turn. Terminal agents, by contrast, can be woken through their own adapters
([the message audit](MESSAGE-DELIVERY-AUDIT.md)).
