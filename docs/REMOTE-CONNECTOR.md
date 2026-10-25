# The remote connector: clients of a Claude or ChatGPT account

The connector is `agentdocker connector` (`crates/cli/src/connector/`). A
connector on `--tunnel tailscale` serves from the machine's own `*.ts.net` name,
which stays the same; a quick cloudflared tunnel's hostname is ephemeral and
both vendors' saved connectors must be re-added when it changes. One connector
serves every project on the machine (the consent page chooses), a vendor may
identify itself by its Client ID Metadata Document instead of registering, and
the desktop's Tools screen shows the connector's state.

## Why it exists

Claude's and ChatGPT's browser extensions run an agent in the browser's side
panel. Those sessions run on the vendor's side; nothing on this machine speaks
for them ([the inventory](GUIDE.md#the-machine)
finds the extension and says so). When such an agent learns something a terminal
agent should know, the only way in is the one the vendors offer hosted agents:
a remote MCP server over public HTTPS with OAuth. The connector is that server,
kept as small as the requirement allows and as far from the daemon as possible.
It is one per machine, not one per project: whoever installs AgentDocker gets
one door for every project the daemon knows, and each consent chooses its
project.

A connector is added to the person's account, not to a browser. Every client
of that account that offers its connectors — the browser side panel, the web
and desktop apps, and a terminal Codex or Claude Code reaching the account's
connectors — presents the same grant from the same vendor addresses, so
nothing the connector receives proves which one is calling. It does not claim
to know: the agent a consent makes is named for the connector and labelled
`surface=unverified`, and what each client says about its own software is kept
beside the grant for the person to read.

## What it is, and is not

- A separate, opt-in process: `agentdocker connector serve`. The daemon never
  listens on the network; the connector talks to it over the local socket like
  any other client.
- Loopback only. It binds `127.0.0.1` and refuses anything else. The public
  hostname and TLS come from a tunnel. `--tunnel tailscale` is the one to
  reach for: Tailscale Funnel on this machine's own `*.ts.net` name, which is
  the same after every restart and needs no domain — the connector reads the
  name from `tailscale status`, sets `tailscale funnel --bg --https=<443|8443|10000>
  http://127.0.0.1:<port>` on the way in and clears it on the way out, on an
  error or a panic too (waiting at most three seconds for `tailscale`, then
  killing it). A port that already serves something else is refused, never
  taken over. The Funnel is recorded in `connector/funnel.json` (its pid,
  port and loopback address) before it is set, and the record goes only once
  the Funnel is confirmed undone, so it is never open unrecorded. A set that
  fails is undone only if the port then points at this connector's own
  address; one it cannot confirm stays recorded. A Funnel a killed connector
  left is cleared by the next start before anything else, but only while
  `tailscale serve status` still points it at the recorded address: one
  pointed elsewhere since is somebody else's and is left alone, and one that
  cannot be read or cleared stops the start, keeping its record, with the
  commands to check and clear it. One connector serves from a home at a
  time (`connector/serve.lock`, held while it runs), so no second one writes
  over its state, status or record. Every `tailscale` command the connector
  runs has 15 s, after which it is killed. Funnel
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
- One agent identity per consent, in the project the consent chose,
  registered until revoked, used by whichever client of the account calls.
  It has no process here whose end could retire it,
  so it counts as connected only while it is heard from: within an hour (one
  access token's lifetime) of its last tool call. Initializing and listing
  tools is a client loading the connector — Claude Code loads the claude.ai
  connectors at every start — and does not count. Past that, `ps` and the
  desktop show it **not connected** and do not count it as a live session; its
  grant, record and queue stand, and its next tool call makes it connected again.
- A messaging-only tool surface. The connector's agent has no checkout here.
- Not a way in for terminal agents. A tool call whose `_meta` names a Codex
  conversation (a `threadId`, or `x-codex-turn-metadata` naming a thread or
  session — what Codex sends with its tool calls) is refused with the way to
  AgentDocker's local MCP server, which acts as the session itself, and does
  not count as the agent being used. A bare `sessionId` is not taken for
  Codex: other clients may send one.

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
   fetched from — never the document's own name) and asks for the pairing code,
   and nothing else: nothing about this machine is shown before the code. A
   right code is spent (the connector shows a new one) and opens a second page,
   under a one-time ticket bound to that authorization request, that asks
   which project the agent joins (the projects the daemon has seen agents in,
   the proposed one first, or the full path of any folder on this machine) and
   a name for the agent (a `claude-connector-xxxx` / `chatgpt-connector-xxxx`
   name is suggested: the vendor's account and how it came in, never a
   surface), and sends the person back to the vendor with a one-time code.
   Consent alone creates nothing.
4. The vendor redeems the code with its PKCE verifier. That is when the daemon
   registers the agent — an external, pidless agent in the chosen project,
   runtime `claude-browser` or `chatgpt-browser` (the names existing records,
   logos and the inventory know; they name the vendor account, not a
   browser), labels `connector=true`, `vendor=<label>` and
   `surface=unverified` — and the grant and its first tokens exist. A code
   that is never redeemed, or redeemed with the wrong verifier,
   leaves no agent behind.
5. From then on the account's clients call `POST <public-url>/mcp` with the
   grant's bearer token. Every accepted tool call is a heartbeat for the
   agent. `initialize` names the project the connector reaches, says the
   account offers it in sessions about other work too and that it acts only
   in that project, and that it has no checkout.

## Endpoints

| Path | Role |
| --- | --- |
| `GET /` | A page saying what this is and how to add it; nothing to run or fetch. |
| `GET /.well-known/oauth-protected-resource[/mcp]` | RFC 9728: `resource` is `<public-url>/mcp`, `authorization_servers` is `[<public-url>]`, the one scope is `agentdocker`. |
| `GET /.well-known/oauth-authorization-server[/mcp]` | RFC 8414: the three endpoints below, `code` only, `authorization_code` and `refresh_token`, PKCE `S256` only, public clients only, `client_id_metadata_document_supported` and `authorization_response_iss_parameter_supported` (RFC 9207: every authorization response, success or error, carries `iss=<public-url>`, which is what lets ChatGPT use its one stable client and callback). |
| `POST /register` | RFC 7591. `redirect_uris` must all be vendors' callbacks (`https://claude.ai/api/mcp/auth_callback`, `https://chatgpt.com/connector_platform_oauth_redirect`, `https://chatgpt.com/connector/oauth/<id>`) or ones passed with `--allow-callback`; `token_endpoint_auth_method` must be `none`. At most 200 clients are kept; at the cap, the oldest client with no grant and no pending code, registered ten minutes or more ago, gives its place to the new one, so a flood of unauthenticated registrations holds the door at most ten minutes. |
| `GET /authorize` | Validates the request. A `client_id` that is an `https://` URL with a path is a Client ID Metadata Document: it is fetched (`connector/cimd.rs`: HTTPS only, verified against the roots the system trusts, no redirects, 10 s, 64 KiB, once an hour per URL) only when its host is a vendor's (`claude.ai`, `chatgpt.com`) or the host of an `--allow-callback`, and admitted only when the document names its own URL as `client_id` and every `redirect_uris` entry is that vendor's callback — explicit token authentication methods must support the public-client `none` method. Unsupported or malformed method declarations are refused before client state changes. A bad client, host, document or callback is a page (nothing is sent to an untrusted callback); any other problem goes back to the callback as an OAuth error. Otherwise the first consent page: who asks, and the pairing code. It lists no project and asks the daemon nothing. |
| `POST /authorize` | Two forms, both carrying the request's fields. The first carries the pairing code: ten characters, fifty random bits, compared in constant time with case, spaces and the dash forgiven. A right code is spent and answers the second page with a `consent_ticket` (ten minutes, one consent, bound to the request's client, callback, challenge and state; at most 64 open). A wrong one costs its source a strike: five within fifteen minutes and that source is refused for the rest of them. The source is the address in the header the tunnel is known to write (the one the connector's own tunnel writes, or `--client-ip-header`, with or without `--allow-from`), read as the allowlist reads it; with no such header known, or none on the request, there is no source and nobody is refused. Twenty wrong codes against one code, from anywhere, replace it, so guessing never gets further and nobody is locked out by somebody else's. Judging a code, counting it and spending or replacing it are one step under one lock, so a right code typed on several pages at once opens one, and the status file shows the codes in the order they were made. A ticket is checked again against the time when it is spent. The second carries the ticket, the project (`project`, one of the listed roots, or `project_path`, any absolute directory on this machine; a folder that is not here is refused on the page, with the choices kept) and the agent name. A name that is a live agent's is refused on the page. |
| `POST /token` | `authorization_code` (code, `code_verifier`, client and callback must match; single use; five-minute lifetime) registers the agent and issues tokens; `refresh_token` rotates. Errors are RFC 6749 codes: a rotated or revoked refresh token is `invalid_grant`. |
| `POST /mcp` | Streamable HTTP, JSON-RPC in `application/json`, plain JSON out (`202` for a notification). No bearer, an expired or revoked token, or an agent the daemon answers is no longer live: `401` with `WWW-Authenticate: Bearer resource_metadata="…"`, and the grant ends. A daemon that is not answering: `503` with `Retry-After`, and the grant stands. `GET`/`DELETE` are `405`: the server opens no stream. An `initialize` or `tools/call` adds its client's fingerprint to the grant (below); a `tools/call` naming a Codex conversation is answered with a JSON-RPC error and no heartbeat. |

Access tokens last one hour; the vendor refreshes on the `401`. Refresh tokens
rotate on every use. Clients and grants persist in
`$AGENTDOCKER_HOME/connector/state.json` (mode `0600`) as hashes — no token is
ever written down — so a connector restart costs the vendor one refresh; codes
and access tokens live in memory only.

Each grant also keeps `clients_seen`: the last eight distinct fingerprints its
clients presented, each with when it was first and last seen. A fingerprint is
what one `initialize` or `tools/call` said about its software — the
`User-Agent` header, the `clientInfo` name and version and the protocol
version (from `initialize`, or a later request's `MCP-Protocol-Version`
header), and the sorted names of its `_meta` keys — never a `_meta` value,
which may carry a person's locale, location or ids, and nothing from the
arguments. Each field is bounded and stripped of control characters. A new
fingerprint is saved when it first appears; one seen again is updated in
memory and saved with the next change, as `last_used_at` is. Every field is
the client's own claim: a fingerprint is evidence of what called, not proof.

The connector advertises only public-client token exchange (`none`) with PKCE.
For CIMD, `token_endpoint_auth_methods_supported` must include `none` when
present; the older singular `token_endpoint_auth_method` is a preference when
both fields exist. A document offering both `none` and `private_key_jwt` can
therefore use public PKCE even if it prefers the latter. A singular declaration
alone must be `none`; legacy documents omitting both fields retain the existing
public-client default. An incompatible refresh leaves prior client/grant state
unchanged. This follows the [OpenAI method-intersection contract](https://developers.openai.com/plugins/build/auth).
It does not implement JWT client assertions.

## Tools

`whoami`, `list_agents`, `inspect_agent`, `send_message`, `read_inbox`,
`acknowledge_messages`, `ask_human`, `open_questions`, `read_journal`,
`journal_note`, `report_activity`, and board reads plus comments: `list_tasks`,
`get_task`, `search_tasks`, `comment_task`, `list_plans`, `get_plan`. Everything
else — leases, observations, worktrees, commits, hand-offs, pulling or moving
cards, channels, `wait_for_messages` — is refused
by name with the reason: the agent has no checkout here, and a long wait would
hold an HTTP request. Messages the connector's agent sends are attributed
input to their recipients, never instructions; its own instructions say the
same about what it reads on web pages, and tell a Claude Code or Codex
session to use its own AgentDocker tools instead.

## Commands

| Command | What it does |
| --- | --- |
| `connector serve [--public-url <https://…>] [--tunnel tailscale [--tunnel-port 443\|8443\|10000] [--tailscale <path>] \| --tunnel cloudflared [--tunnel-name <name>] [--cloudflared <path>]] [--bind 127.0.0.1:0] [--project <path>] [--allow-callback <url>]… [--allow-from <cidr>\|anthropic\|openai\|@<file>]… [--client-ip-header <name>]` | Serve every project on this machine; `--project` is the one proposed first at consent. Prints the MCP URL, the admitted addresses and the vendors' setup steps, and the pairing code only to a terminal (a service's stderr is its log, which never gets it), and writes `$AGENTDOCKER_HOME/connector/serve.json` (created 0600; `agentdocker_host::connector::Serving`, which the desktop reads too) with the same for `status`, rewritten whenever the code is replaced. Plain `http://` is accepted only for a loopback host, for a trial without a tunnel. |
| `connector status` | Whether a connector is serving here: its address, listening socket, pairing code, proposed project, tunnel, admitted prefixes, and how many connector agents the daemon holds live. |
| `connector install <serve arguments> [--dry-run]` | Run the connector as a login service — a launchd agent (`dev.agentdocker.connector`) or a systemd user unit (on Windows, a login task, described below) — with those arguments, `AGENTDOCKER_HOME` and a PATH that includes where cloudflared was found; its log is `$AGENTDOCKER_HOME/connector/serve.log`. A quick tunnel gets a new hostname at every start and says so. |
| `connector enable <serve arguments> [--dry-run]` | Install/start a new login service or start its byte-identical definition. A synced temporary file is published atomically without replacement; exact comparison refuses symlinks or different existing settings. Failed staging leaves no partial definition to block retry. The desktop uses this conservative entry point; `install` remains the explicit replacement command. |
| `connector uninstall [--dry-run]` | Stop and remove the service. On Linux, stop/disable runs while the definition exists; a stop or disable failure preserves the installed definition for diagnosis and retry. Those failures are tolerated when the definition was already missing. Systemd reload runs after removal. |
| `connector grants` | Every consent: agent, runtime, vendor, project, when connected and last used, whether active, whether the daemon still holds the agent live, and how many client fingerprints it has seen; then each grant's fingerprints, most recent first, as the clients described their own software. |
| `connector revoke <agent>` | Ends the grant (tokens stop) and marks the agent finished. A serving connector notices on that agent's next request. |

## Admitting only the vendors

Behind a tunnel every TCP peer is the tunnel, so the address worth checking is
the one it writes into a header: `cf-connecting-ip` for cloudflared,
`x-forwarded-for` for Tailscale Funnel and most others; the connector picks the
right one for a tunnel it runs (`--client-ip-header` otherwise). Only the last entry of the header's last line is read. Tailscale Serve and Funnel replace `x-forwarded-for` with the address they saw; a tunnel of your own must do the same, or append a trustworthy final address after whatever the client sent, so an address the client puts before it, an allowed one included, is never taken for the client's. `--allow-from` takes a CIDR, `anthropic` (its published
connector range, `160.79.104.0/21`), `openai` (its automatically refreshed feed),
or `@<file>` (a local feed JSON or one CIDR per line, re-read when changed).
It gates `/register`, `/token` and `/mcp`; the consent page is
the person's browser and is never gated; the metadata stays open. A refused
address is logged. With no `--allow-from`, every address is admitted and the
OAuth flow is the only gate.

`--allow-from openai` fetches only
[`https://openai.com/chatgpt-connectors.json`](https://openai.com/chatgpt-connectors.json)
before starting the listener or tunnel, then once an hour after each attempt.
The fetch uses HTTPS without redirects, a 10-second global timeout, a 256 KiB
body limit and at most 4,096 prefixes. Every entry must parse, match its declared
IP family and have a nonzero prefix length; empty or partially malformed feeds
are refused. A startup failure stops startup. A later failure logs the error
and retains the whole last valid in-memory list until a later refresh succeeds;
there is no allow-all fallback or disk cache. Restart requires a successful
fresh fetch. Changes apply without restarting active OAuth sessions, and the
status prefix count is updated after a successful refresh. Sleep/wake does not
trigger catch-up requests. Explicit CIDRs, `anthropic` and `@<file>` do not
trigger network refreshes or change their existing configuration.

## Security limits, stated

- The consent is the only thing that makes an agent, and it needs the pairing
  code from the terminal: the person at the page is the person at the machine.
- Only the vendors' own hosted surfaces can complete the flow, because only
  their callbacks can be registered or named by a metadata document, and a
  metadata document is fetched only from their hosts. Knowing the URL is not
  enough. A `client_id` naming any other host never causes a fetch, so the
  authorization server cannot be pointed at an address of an attacker's
  choosing. The optional `openai` admission preset makes a separate periodic
  request only to the fixed vendor feed above.
- Every request is bounded: 8 KiB request line, 32 KiB of headers, 1 MiB body,
  15 s to arrive and 15 s for its reply to be taken, 64 connections (one
  more is closed at once, never waited on), one request per connection,
  chunked bodies refused. TLS, keep-alive and any client-IP allowlisting are the tunnel's.
- The tool surface cannot touch files, leases, worktrees or commits, whatever
  the hosted model is told by a page it read.
- `project` as a recipient is the project the consent chose, wherever the
  connector's process runs; a service starts in no project at all, so a
  broadcast is never resolved from its working directory. `project:<path>` reaches any other project, as for every agent.
- Every tool carries MCP annotations (`readOnlyHint`, `destructiveHint`,
  `idempotentHint`, `openWorldHint`), so a host that asks before destructive or
  open-world calls — ChatGPT's *Allow low-risk actions* default — lets the
  reads through and asks for the writes; nothing here is open-world but
  `validate`, which runs a command.
- The desktop's Tools screen reads `serve.json` (with its process alive) and
  says on each in-browser runtime's card whether the connector is serving, its
  MCP URL, the pairing code and where to add it. On macOS and Linux, the browser
  runtime Details card also offers **Enable with Tailscale** or **Enable with
  Cloudflare**. These explicit actions install/start a login service through
  the sibling CLI with both vendor address allowlists, preserving a differently
  configured existing service. Tailscale needs Funnel enabled; Cloudflare quick
  tunnels change address after restart, which the screen explains before setup.
  Setup marks only the selected tunnel as starting and disables both setup
  actions until it finishes. It has its own bounded worker; failure remains on the card, while ordinary
  message delivery and refresh continue. Service installation does not prove
  browser consent or a provider receipt.

## What the connector's agent cannot get

It is not woken. A message to it waits in its inbox until the
hosted model calls `read_inbox` — which it does only when the person's prompt
leads it to. Nothing in the vendors' connector model lets a server start a
turn. Terminal agents, by contrast, can be woken through their own adapters.

One more thing to know: a connector added to a Claude account appears in every
Claude surface of that account, Claude Code sessions on any machine and about
any other work included, as the one agent the consent made in the chosen
project. So it reaches that project and the person, never another project:
an explicit `project` argument is refused, a card, plan or agent named by id
must be that project's, and `open_questions` lists only its agents'
questions. Its instructions name the project and say to use it only when the
person asks about it; a terminal session with its own AgentDocker tools uses
those. To keep it out of a session, turn it off in that session's `/mcp`, or
remove it under the account's Settings › Connectors.

The Windows login service requires an owned daemon login task for the
same state home: install that prerequisite explicitly with `agentdocker daemon
install`, then review `connector install ... --dry-run` or use conservative
`connector enable`. The connector starts the daemon through its separate task,
retains the selected endpoint and never starts a daemon as its own child.
Uninstall requests an exact connector generation's graceful exit, preserving
the daemon and saved browser grants. A changed or foreign task is refused.

Windows login-service setup resolves configured project, egress-feed and tunnel
executable paths at installation time, so a relative shell path does not become
a different path under Task Scheduler's working directory. `tailscale.exe` and
`cloudflared.exe` are discovered through the executable PATH lookup on Windows;
an explicit executable path remains supported. Discovered tunnel paths are
captured before argument serialization, so the installed task and its reparsed
service-run command keep the same order with vendor allowlists.

Startup holds the service mutation lock from ownership validation through
publication of its exact process generation. It waits up to 30 seconds for an
in-progress install/remove operation, then rechecks the receipt; removal holds
the same lock while stopping the published generation. The runtime releases
this lock before serving, so graceful shutdown does not wait on the remover.
Service confirmations go to stderr, as on
Unix; dry-run plans remain on stdout.
