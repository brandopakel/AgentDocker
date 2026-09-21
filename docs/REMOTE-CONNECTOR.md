# The remote connector: agents that work inside a browser

Status: September 18, 2026. Delivered in source as `agentdocker connector`
(`crates/cli/src/connector/`), with unit tests, a real-binary trial on loopback,
and acceptances against real Claude and ChatGPT accounts: through a cloudflared
tunnel to the production daemon, each vendor registered its client by DCR, the
person consented with the pairing code, and each browser agent delivered a
message to keel's terminal sessions — first from a terminal-run connector, then
from the connector installed as a login service with a vendor-egress allowlist
([record](verification/INDEX.md), `remote_connector_2026_09_17`, `vendor_acceptance`
and `service_acceptance`). The production connector now runs on `--tunnel
tailscale`, this machine's own `*.ts.net` name, with both vendors connected
(`stable_host_acceptance`); a quick cloudflared tunnel's hostname is ephemeral
and both vendors' saved connectors must be re-added when it changes. Since
then one connector serves every project on the machine (the consent page
chooses), a vendor may identify itself by its Client ID Metadata Document
instead of registering, and the desktop's Tools screen shows the connector's
state — in source with unit tests; the vendors' CIMD path is not yet exercised
against a real account (`any_project_and_cimd`).

## Why it exists

Claude's and ChatGPT's browser extensions run an agent in the browser's side
panel. Those sessions run on the vendor's side; nothing on this machine speaks
for them ([the inventory](GUIDE.md#the-machine)
finds the extension and says so). When such an agent learns something a terminal
agent should know, the only way in is the one the vendors offer hosted agents:
a remote MCP server over public HTTPS with OAuth. The connector is that server,
kept as small as the requirement allows and as far from the daemon as possible.
It is one per machine, not one per project: whoever installs AgentDocker gets
one door for every project the daemon knows, and each browser agent chooses
its project when it consents.

## What it is, and is not

- A separate, opt-in process: `agentdocker connector serve`. The daemon never
  listens on the network; the connector talks to it over the local socket like
  any other client.
- Loopback only. It binds `127.0.0.1` and refuses anything else. The public
  hostname and TLS come from a tunnel. `--tunnel tailscale` is the one to
  reach for: Tailscale Funnel on this machine's own `*.ts.net` name, which is
  the same after every restart and needs no domain — the connector reads the
  name from `tailscale status`, sets `tailscale funnel --bg --https=<443|8443|10000>
  http://127.0.0.1:<port>` on the way in and clears it on the way out; Funnel
  must be enabled for the tailnet (the error says how). `--tunnel cloudflared`
  starts cloudflared as a child: a quick tunnel, whose random
  `*.trycloudflare.com` hostname it reads from cloudflared's output and which
  changes at every start, or with `--tunnel-name` a named tunnel the person
  created and routed (`cloudflared tunnel login`, `tunnel create <name>`,
  `tunnel route dns <name> <host>`), whose hostname is `--public-url`. Any
  other tunnel or reverse proxy is `--public-url` with the person running it.
  A tunnel child dies with the connector and the connector stops when it
  exits.
- Its own OAuth 2.1 authorization server. A vendor identifies itself either by
  its Client ID Metadata Document — an HTTPS URL on its own host, fetched and
  admitted here, which both Claude and ChatGPT prefer — or by dynamic client
  registration; either way only the vendors' callback URLs are admitted.
- One browser-agent identity per consent, in the project the consent chose,
  alive until revoked.
- A messaging-only tool surface. A browser agent has no checkout here.

## The flow

1. `agentdocker connector serve --tunnel tailscale` (or `--tunnel cloudflared`,
   or `--public-url https://<tunnel host>` with a tunnel of yours), once per
   machine; `--project <path>` only proposes a project first on the consent
   page. It prints the MCP URL to paste, `<public-url>/mcp`, and a **pairing
   code** that changes on every run; `connector status` and the desktop's
   Tools screen show both.
2. The person adds `<public-url>/mcp` as a custom connector — Claude: *Settings ›
   Connectors › Add custom connector*; ChatGPT: *Settings › Connectors ›
   Advanced › Developer mode*. The vendor discovers the metadata, identifies
   its client (its metadata document, or a registration), and sends the person
   to the consent page.
3. The consent page names the vendor (for a metadata document, the host it was
   fetched from — never the document's own name), asks for the pairing code,
   which project the agent joins (the projects the daemon has seen agents in,
   the proposed one first, or the full path of any folder on this machine) and
   a name for the agent (a `claude-browser-xxxx` / `chatgpt-browser-xxxx` name
   is suggested), and sends the person back to the vendor with a one-time code.
   Consent alone creates nothing.
4. The vendor redeems the code with its PKCE verifier. That is when the daemon
   registers the browser agent — an external, pidless agent in the chosen
   project, runtime `claude-browser` or `chatgpt-browser`, labels
   `connector=true` and `vendor=<label>` — and the grant and its first tokens
   exist. A code that is never redeemed, or redeemed with the wrong verifier,
   leaves no agent behind.
5. From then on the hosted agent calls `POST <public-url>/mcp` with its bearer
   token. Every accepted request is a heartbeat for the agent. `initialize`
   tells the model it works inside a browser and has no checkout.

## Endpoints

| Path | Role |
| --- | --- |
| `GET /` | A page saying what this is and how to add it; nothing to run or fetch. |
| `GET /.well-known/oauth-protected-resource[/mcp]` | RFC 9728: `resource` is `<public-url>/mcp`, `authorization_servers` is `[<public-url>]`, the one scope is `agentdocker`. |
| `GET /.well-known/oauth-authorization-server[/mcp]` | RFC 8414: the three endpoints below, `code` only, `authorization_code` and `refresh_token`, PKCE `S256` only, public clients only, `client_id_metadata_document_supported` and `authorization_response_iss_parameter_supported` (RFC 9207: every authorization response, success or error, carries `iss=<public-url>`, which is what lets ChatGPT use its one stable client and callback). |
| `POST /register` | RFC 7591. `redirect_uris` must all be vendors' callbacks (`https://claude.ai/api/mcp/auth_callback`, `https://chatgpt.com/connector_platform_oauth_redirect`, `https://chatgpt.com/connector/oauth/<id>`) or ones passed with `--allow-callback`; `token_endpoint_auth_method` must be `none`. At most 200 clients are kept. |
| `GET /authorize` | Validates the request. A `client_id` that is an `https://` URL with a path is a Client ID Metadata Document: it is fetched (`connector/cimd.rs`: HTTPS only, no redirects, 10 s, 64 KiB, once an hour per URL) only when its host is a vendor's (`claude.ai`, `chatgpt.com`) or the host of an `--allow-callback`, and admitted only when the document names its own URL as `client_id` and every `redirect_uris` entry is that vendor's callback — what it says otherwise is not trusted, and it is a public client whatever authentication it prefers. A bad client, host, document or callback is a page (nothing is sent to an untrusted callback); any other problem goes back to the callback as an OAuth error. Otherwise the consent page, which lists the projects the daemon has seen agents in. |
| `POST /authorize` | The consent form: pairing code (case, spaces and the dash are forgiven), the project (`project`, one of the listed roots, or `project_path`, any absolute directory on this machine; a folder that is not here is refused on the page, with the choices kept), agent name, and the request's fields. Five wrong codes close consent until the process restarts. A name that is a live agent's is refused on the page. |
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
| `connector serve [--public-url <https://…>] [--tunnel tailscale [--tunnel-port 443\|8443\|10000] [--tailscale <path>] \| --tunnel cloudflared [--tunnel-name <name>] [--cloudflared <path>]] [--bind 127.0.0.1:0] [--project <path>] [--allow-callback <url>]… [--allow-from <cidr>\|anthropic\|@<file>]… [--client-ip-header <name>]` | Serve every project on this machine; `--project` is the one proposed first at consent. Prints the MCP URL, the pairing code, the admitted addresses and the vendors' setup steps, and writes `$AGENTDOCKER_HOME/connector/serve.json` (mode 0600; `agentdocker_host::connector::Serving`, which the desktop reads too) with the same for `status`. Plain `http://` is accepted only for a loopback host, for a trial without a tunnel. |
| `connector status` | Whether a connector is serving here: its address, listening socket, pairing code, proposed project, tunnel, admitted prefixes, and how many browser agents the daemon holds live. |
| `connector install <serve arguments> [--dry-run]` | Run the connector as a login service — a launchd agent (`dev.agentdocker.connector`) or a systemd user unit — with those arguments, `AGENTDOCKER_HOME` and a PATH that includes where cloudflared was found; its log is `$AGENTDOCKER_HOME/connector/serve.log`. A quick tunnel gets a new hostname at every start and says so. |
| `connector uninstall [--dry-run]` | Remove the service. |
| `connector grants` | Every consent: agent, runtime, vendor, project, when connected and last used, whether active, and whether the daemon still holds the agent live. |
| `connector revoke <agent>` | Ends the grant (tokens stop) and marks the agent finished. A serving connector notices on that agent's next request. |

## Admitting only the vendors

Behind a tunnel every TCP peer is the tunnel, so the address worth checking is
the one it writes into a header: `cf-connecting-ip` for cloudflared,
`x-forwarded-for` for Tailscale Funnel and most others; the connector picks the
right one for a tunnel it runs (`--client-ip-header` otherwise). `--allow-from` takes a CIDR, `anthropic` (its published
connector range, `160.79.104.0/21`) or `@<file>` — OpenAI's feed
(`https://openai.com/chatgpt-connectors.json`, a few hundred prefixes that
change; keep it fresh with `curl -o`) or one CIDR per line — and re-reads a file
when it changes. It gates `/register`, `/token` and `/mcp`; the consent page is
the person's browser and is never gated; the metadata stays open. A refused
address is logged. With no `--allow-from`, every address is admitted and the
OAuth flow is the only gate.

## Security limits, stated

- The consent is the only thing that makes an agent, and it needs the pairing
  code from the terminal: the person at the page is the person at the machine.
- Only the vendors' own hosted surfaces can complete the flow, because only
  their callbacks can be registered or named by a metadata document, and a
  metadata document is fetched only from their hosts. Knowing the URL is not
  enough. The fetch is the connector's one outbound request; a `client_id`
  naming any other host never causes one, so the authorization server cannot
  be pointed at an address of an attacker's choosing.
- Every request is bounded: 8 KiB request line, 32 KiB of headers, 1 MiB body,
  15 s to arrive, 64 connections, one request per connection, chunked bodies
  refused. TLS, keep-alive and any client-IP allowlisting are the tunnel's.
- The tool surface cannot touch files, leases, worktrees or commits, whatever
  the hosted model is told by a page it read.
- `project` as a recipient is the project the consent chose, wherever the
  connector's process runs; a service starts in no project at all, and a
  broadcast resolved from its working directory once went to a project nobody
  was in. `project:<path>` reaches any other project, as for every agent.
- Every tool carries MCP annotations (`readOnlyHint`, `destructiveHint`,
  `idempotentHint`, `openWorldHint`), so a host that asks before destructive or
  open-world calls — ChatGPT's *Allow low-risk actions* default — lets the
  reads through and asks for the writes; nothing here is open-world but
  `validate`, which runs a command.
- The desktop's Tools screen reads `serve.json` (with its process alive) and
  says on each in-browser runtime's card whether the connector is serving, its
  MCP URL, the pairing code and where to add it; it does not start or install
  the service yet. The installed CLI now includes `connector`; desktop service
start/install is still an implementation gap, not an old-binary limitation.
- Not yet exercised against a real account: the vendors' Client ID Metadata
  Document path (both vendors used DCR when they connected; the next connection
  a vendor makes after this metadata is served is the trial).

## What a browser agent cannot get

It is not woken. A message to a browser agent waits in its inbox until the
hosted model calls `read_inbox` — which it does only when the person's prompt
leads it to. Nothing in the vendors' connector model lets a server start a
turn. Terminal agents, by contrast, can be woken through their own adapters
(the message audit (in git history)).

One more thing to know: a connector added to a Claude account appears in every
Claude surface of that account, Claude Code sessions included, as that
account's browser-agent identity. A terminal session with its own AgentDocker
tools should use those; the connector's instructions say so.
