//! The remote connector: how an agent that works inside a browser — the
//! Claude side panel, the ChatGPT extension — reaches a project's other
//! agents. Those sessions run on the vendor's side and speak to tools only
//! over public HTTPS with OAuth, so this is a separate, opt-in process:
//! it binds loopback, the person's tunnel gives it a public name, and it
//! serves the vendors an MCP endpoint whose tools are the messaging ones.
//! Every consent creates one browser-agent identity in the daemon; the
//! daemon itself never listens on the network.

pub mod http;
pub mod oauth;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use agentdocker_core::agent::{GENERATED_NAME, NAME_LABEL};
use agentdocker_core::{AgentSpec, ErrorCode, Request, Response};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::client::{Backend, Client};
use crate::mcp::{Identity, McpServer};
use http::{Request as HttpRequest, Response as HttpResponse, escape_html, field};
use oauth::{AgentIdentity, AuthorizeRefusal, OAuthError, Store, Vendor};

/// Consents refused for a wrong pairing code before this process stops
/// taking any: a code is eight characters, and nobody types it wrong
/// this often.
const MAX_CONSENT_FAILURES: u32 = 5;
/// Registered clients kept at most; DCR is unauthenticated by design, so
/// a flood of registrations must not grow the state file without bound.
const MAX_CLIENTS: usize = 200;
/// Connections served at once.
const MAX_CONNECTIONS: usize = 64;

#[derive(Args, Debug)]
pub struct ConnectorArgs {
    #[command(subcommand)]
    pub command: ConnectorCommand,
}

#[derive(Subcommand, Debug)]
pub enum ConnectorCommand {
    /// Serve the connector on loopback for the tunnel in front of it.
    Serve(ServeArgs),
    /// The browser agents that have connected, and whether they still can.
    Grants,
    /// End a browser agent's access: its tokens stop working and the agent is marked finished.
    Revoke {
        /// The agent's name or id, or the grant id from `grants`.
        agent: String,
    },
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// The HTTPS address the tunnel publishes this connector at, without a
    /// trailing slash: the vendors add `/mcp` to it.
    #[arg(long)]
    pub public_url: String,
    /// The loopback address to listen on; port 0 picks a free one.
    #[arg(long, default_value = "127.0.0.1:0")]
    pub bind: String,
    /// The project browser agents join (default: the project of the current directory).
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// A callback URL to admit besides the vendors' own, for a hosted client of yours.
    #[arg(long = "allow-callback")]
    pub allow_callbacks: Vec<String>,
}

pub async fn run(client: Client, args: ConnectorArgs) -> Result<()> {
    match args.command {
        ConnectorCommand::Serve(args) => serve(client, args).await,
        ConnectorCommand::Grants => grants(&client).await,
        ConnectorCommand::Revoke { agent } => revoke(&client, &agent).await,
    }
}

/// Where clients and grants live between runs.
fn state_path() -> PathBuf {
    agentdocker_host::dirs::home()
        .join("connector")
        .join("state.json")
}

fn load_store(path: &Path) -> Result<Store> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not a connector state file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Write the whole store, privately, and only ever whole.
fn save_store(path: &Path, store: &Store) -> Result<()> {
    let parent = path.parent().context("state path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".state.{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(store)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// The public address, checked: HTTPS, or plain HTTP only on loopback for
/// a trial without a tunnel.
fn public_url(raw: &str) -> Result<String> {
    let url = raw.trim().trim_end_matches('/').to_owned();
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
            bail!("--public-url must be https://; plain http is accepted only on loopback");
        }
    } else if !url.starts_with("https://") {
        bail!("--public-url must start with https://");
    }
    if url.contains(['?', '#']) || url.len() > 512 {
        bail!("--public-url must be a plain origin, with at most a path");
    }
    Ok(url)
}

/// Everything a request handler needs, shared by the connections.
pub struct Connector<B> {
    backend: B,
    public_url: String,
    project: agentdocker_core::ProjectRef,
    extra_callbacks: Vec<String>,
    pairing_code: String,
    store: Mutex<Store>,
    state_path: Option<PathBuf>,
    servers: Mutex<HashMap<String, Arc<McpServer<B>>>>,
    consent_failures: AtomicU32,
}

impl<B: Backend + Clone + Send + Sync + 'static> Connector<B> {
    pub fn new(
        backend: B,
        public_url: String,
        project: agentdocker_core::ProjectRef,
        extra_callbacks: Vec<String>,
        pairing_code: String,
        store: Store,
        state_path: Option<PathBuf>,
    ) -> Self {
        Self {
            backend,
            public_url,
            project,
            extra_callbacks,
            pairing_code,
            store: Mutex::new(store),
            state_path,
            servers: Mutex::new(HashMap::new()),
            consent_failures: AtomicU32::new(0),
        }
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.public_url)
    }

    fn persist(&self) {
        let Some(path) = &self.state_path else {
            return;
        };
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = save_store(path, &store) {
            eprintln!(
                "agentdocker connector: could not save {}: {error:#}",
                path.display()
            );
        }
    }

    /// One request in, one response out. Nothing here holds the store
    /// across an await.
    pub async fn handle(&self, request: HttpRequest) -> HttpResponse {
        match (request.method.as_str(), request.path()) {
            ("GET", "/") => self.front_page(),
            ("GET", "/.well-known/oauth-protected-resource")
            | ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                HttpResponse::json(200, &self.resource_metadata())
            }
            ("GET", "/.well-known/oauth-authorization-server")
            | ("GET", "/.well-known/oauth-authorization-server/mcp") => {
                HttpResponse::json(200, &self.server_metadata())
            }
            ("POST", "/register") => self.register(&request),
            ("GET", "/authorize") => self.consent_form(&request),
            ("POST", "/authorize") => self.consent(&request).await,
            ("POST", "/token") => self.token(&request).await,
            ("POST", "/mcp") => self.mcp(&request).await,
            ("GET" | "DELETE", "/mcp") => HttpResponse::text(
                405,
                "this connector answers JSON-RPC over POST only; it opens no stream",
            )
            .header("Allow", "POST"),
            _ => HttpResponse::text(404, "not here"),
        }
    }

    fn resource_metadata(&self) -> Value {
        json!({
            "resource": self.mcp_url(),
            "authorization_servers": [self.public_url],
            "scopes_supported": [oauth::SCOPE],
            "bearer_methods_supported": ["header"],
            "resource_name": format!("AgentDocker connector for {}", self.project.name()),
        })
    }

    fn server_metadata(&self) -> Value {
        json!({
            "issuer": self.public_url,
            "authorization_endpoint": format!("{}/authorize", self.public_url),
            "token_endpoint": format!("{}/token", self.public_url),
            "registration_endpoint": format!("{}/register", self.public_url),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": [oauth::SCOPE],
        })
    }

    fn front_page(&self) -> HttpResponse {
        HttpResponse::html(
            200,
            page(
                "AgentDocker connector",
                &format!(
                    "<p>This is the AgentDocker connector for the project <b>{}</b>. It lets an agent working inside a browser join that project's messaging.</p>\
                     <p>Add <code>{}</code> as a custom connector: in Claude under <i>Settings › Connectors › Add custom connector</i>, in ChatGPT under <i>Settings › Connectors › Advanced › Developer mode</i>. Consent asks for the pairing code shown in the terminal that runs <code>agentdocker connector serve</code>.</p>\
                     <p>A browser agent has no checkout here: it can find the project's agents, message them, read its inbox and the journal, and nothing else.</p>",
                    escape_html(&self.project.name()),
                    escape_html(&self.mcp_url())
                ),
            ),
        )
    }

    fn register(&self, request: &HttpRequest) -> HttpResponse {
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            return HttpResponse::json(
                400,
                &OAuthError {
                    code: "invalid_client_metadata",
                    description: "the registration body is not JSON".into(),
                }
                .json(),
            );
        };
        let outcome = {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            if store.clients.len() >= MAX_CLIENTS {
                return HttpResponse::json(
                    429,
                    &json!({"error": "too_many_clients", "error_description": "this connector holds as many registered clients as it will; revoke or restart"}),
                );
            }
            store.register_client(&body, &self.extra_callbacks, Utc::now())
        };
        match outcome {
            Ok(reply) => {
                self.persist();
                HttpResponse::json(201, &reply)
            }
            Err(error) => HttpResponse::json(400, &error.json()),
        }
    }

    /// Validate the request and show the consent form, or say why not.
    fn consent_form(&self, request: &HttpRequest) -> HttpResponse {
        let params = http::parse_query(request.query());
        let pending = {
            let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.begin_authorization(&params)
        };
        match pending {
            Ok(pending) => HttpResponse::html(200, self.consent_html(&pending, None)),
            Err(refusal) => self.refuse_authorization(refusal),
        }
    }

    fn refuse_authorization(&self, refusal: AuthorizeRefusal) -> HttpResponse {
        match refusal {
            AuthorizeRefusal::Page(error) => HttpResponse::html(
                400,
                page(
                    "Cannot continue",
                    &format!(
                        "<p>{}</p><p>Only a client registered by Claude or ChatGPT can connect here.</p>",
                        escape_html(&error.description)
                    ),
                ),
            ),
            AuthorizeRefusal::Redirect(pending, error) => {
                HttpResponse::redirect(&pending.error_redirect(&error))
            }
        }
    }

    fn consent_html(&self, pending: &oauth::Pending, problem: Option<&str>) -> String {
        let hidden: String = pending
            .fields()
            .iter()
            .map(|(name, value)| {
                format!(
                    "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
                    escape_html(value)
                )
            })
            .collect();
        let who = match &pending.client_name {
            Some(name) => format!("{} ({})", pending.vendor.label(), escape_html(name)),
            None => pending.vendor.label().to_owned(),
        };
        let suggested = generated_name(pending.vendor);
        let problem = problem
            .map(|p| format!("<p class=\"problem\">{}</p>", escape_html(p)))
            .unwrap_or_default();
        page(
            "Connect a browser agent",
            &format!(
                "<p><b>{who}</b> asks to join the project <b>{project}</b> as a browser agent. It will be able to find the project's agents, message them, read its own inbox and the journal. It gets no files, leases or worktrees.</p>\
                 {problem}\
                 <form method=\"post\" action=\"/authorize\">{hidden}\
                 <label>Pairing code from the terminal running <code>agentdocker connector serve</code><br><input name=\"pairing_code\" autocomplete=\"off\" autofocus required placeholder=\"ABCD-EFGH\"></label>\
                 <label>Name for this agent<br><input name=\"agent_name\" value=\"{suggested}\" maxlength=\"64\" pattern=\"[A-Za-z0-9._-]+\"></label>\
                 <button type=\"submit\">Connect</button></form>",
                project = escape_html(&self.project.name()),
            ),
        )
    }

    /// The consent form came back: the pairing code proves the person at
    /// this page is the person at the machine; then the daemon gets one
    /// new browser-agent identity and the client gets its code.
    async fn consent(&self, request: &HttpRequest) -> HttpResponse {
        let Some(form) = request.form() else {
            return HttpResponse::text(
                415,
                "the consent form posts as application/x-www-form-urlencoded",
            );
        };
        let pending = {
            let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.begin_authorization(&form)
        };
        let pending = match pending {
            Ok(pending) => pending,
            Err(refusal) => return self.refuse_authorization(refusal),
        };
        if self.consent_failures.load(Ordering::Relaxed) >= MAX_CONSENT_FAILURES {
            return HttpResponse::html(
                429,
                page(
                    "Consent closed",
                    "<p>Too many wrong pairing codes: this connector takes no more consents until it is restarted.</p>",
                ),
            );
        }
        let typed = field(&form, "pairing_code").unwrap_or("");
        if !oauth::pairing_code_matches(&self.pairing_code, typed) {
            let failures = self.consent_failures.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!(
                "agentdocker connector: consent refused, wrong pairing code ({failures}/{MAX_CONSENT_FAILURES})"
            );
            return HttpResponse::html(
                403,
                self.consent_html(
                    &pending,
                    Some("That pairing code is not the one in the terminal."),
                ),
            );
        }
        let typed_name = field(&form, "agent_name")
            .filter(|n| {
                n.len() <= 64
                    && n.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            })
            .map(str::to_owned);
        let generated = typed_name.is_none();
        let name = typed_name.unwrap_or_else(|| generated_name(pending.vendor));
        // A name that is already an agent's here is refused now, on the
        // page, rather than at the token exchange where nobody is looking.
        if !generated
            && let Ok(Response::Agent { agent }) = self
                .backend
                .call(Request::Inspect {
                    agent: name.clone(),
                })
                .await
            && agent.status.is_live()
        {
            return HttpResponse::html(
                409,
                self.consent_html(
                    &pending,
                    Some("That name is already a live agent's in this project; pick another."),
                ),
            );
        }
        let redirect = {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.complete_authorization(&pending, name.clone(), generated, Utc::now())
        };
        eprintln!(
            "agentdocker connector: {} consented to join {} as {}; waiting for the token exchange",
            pending.vendor.label(),
            self.project.name(),
            name
        );
        HttpResponse::redirect(&redirect)
    }

    /// One external, pidless agent per redeemed consent, in the project
    /// this connector serves. It stays live until revoked.
    async fn register_agent(&self, consent: &oauth::Consent) -> Result<AgentIdentity> {
        let vendor = consent.vendor;
        let generated = consent.generated;
        let mut attempt = consent.agent_name.clone();
        for _ in 0..3 {
            let mut labels = std::collections::BTreeMap::from([
                ("connector".to_owned(), "true".to_owned()),
                ("vendor".to_owned(), vendor.label().to_owned()),
            ]);
            if generated {
                labels.insert(NAME_LABEL.to_owned(), GENERATED_NAME.to_owned());
            }
            let spec = AgentSpec {
                name: attempt.clone(),
                runtime: vendor.runtime().to_owned(),
                workdir: Some(self.project.root.clone()),
                labels,
                ..AgentSpec::default()
            };
            match self
                .backend
                .call(Request::Register {
                    spec,
                    pid: None,
                    session: None,
                })
                .await?
            {
                Response::Agent { agent } => {
                    return Ok(AgentIdentity {
                        id: agent.id.to_string(),
                        name: agent.spec.name,
                        runtime: agent.spec.runtime,
                        project: self.project.root.clone(),
                    });
                }
                Response::Error {
                    code: ErrorCode::NameTaken,
                    ..
                } if generated => {
                    attempt = generated_name(vendor);
                }
                Response::Error { code, message, .. } => {
                    bail!("{message} ({code:?})");
                }
                other => bail!("unexpected reply to register: {other:?}"),
            }
        }
        bail!("could not find a free name for the browser agent")
    }

    /// The token endpoint. Redeeming a code is where the browser agent
    /// comes into being: the daemon registers it, then the grant and its
    /// first tokens exist. A refresh only rotates.
    async fn token(&self, request: &HttpRequest) -> HttpResponse {
        let Some(form) = request.form() else {
            return HttpResponse::json(
                400,
                &json!({"error": "invalid_request", "error_description": "the token request must be application/x-www-form-urlencoded"}),
            );
        };
        let outcome = match field(&form, "grant_type") {
            Some("authorization_code") => {
                let consent = {
                    let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
                    store.redeem_code(&form, Utc::now())
                };
                match consent {
                    Err(error) => Err(error),
                    Ok(consent) => match self.register_agent(&consent).await {
                        Ok(identity) => {
                            eprintln!(
                                "agentdocker connector: {} connected as {} ({}) in {}",
                                consent.vendor.label(),
                                identity.name,
                                identity.id,
                                self.project.name()
                            );
                            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
                            Ok(store.issue(&consent, identity, Utc::now()))
                        }
                        Err(error) => {
                            eprintln!(
                                "agentdocker connector: the daemon refused the browser agent: {error:#}"
                            );
                            Err(OAuthError {
                                code: "invalid_grant",
                                description: format!(
                                    "the daemon did not accept the agent; consent again ({error:#})"
                                ),
                            })
                        }
                    },
                }
            }
            Some("refresh_token") => {
                let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
                store.refresh(&form, Utc::now())
            }
            Some(other) => Err(OAuthError {
                code: "unsupported_grant_type",
                description: format!("{other} is not issued here"),
            }),
            None => Err(OAuthError {
                code: "invalid_request",
                description: "grant_type is missing".into(),
            }),
        };
        match outcome {
            Ok(issued) => {
                self.persist();
                HttpResponse::json(200, &issued.json())
            }
            Err(error) => HttpResponse::json(400, &error.json()),
        }
    }

    fn unauthorized(&self, why: &str) -> HttpResponse {
        HttpResponse::json(
            401,
            &json!({"error": "invalid_token", "error_description": why}),
        )
        .header(
            "WWW-Authenticate",
            format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\", scope=\"{}\", error=\"invalid_token\"",
                self.public_url,
                oauth::SCOPE
            ),
        )
    }

    /// Streamable HTTP: JSON-RPC in, JSON out. Every accepted request is
    /// a heartbeat for the agent; an agent the daemon no longer holds
    /// live ends its grant.
    async fn mcp(&self, request: &HttpRequest) -> HttpResponse {
        let Some(bearer) = request.bearer() else {
            return self.unauthorized("a bearer token is required");
        };
        let grant = {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.authenticate(bearer, Utc::now())
        };
        let Some((grant_id, grant)) = grant else {
            return self.unauthorized("the token is unknown, expired or revoked");
        };
        match self
            .backend
            .call(Request::Inspect {
                agent: grant.agent_id.clone(),
            })
            .await
        {
            Ok(Response::Agent { agent }) if agent.status.is_live() => {}
            Ok(_) | Err(_) => {
                {
                    let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
                    store.revoke(&grant_id, Utc::now());
                }
                self.persist();
                self.servers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&grant.agent_id);
                return self.unauthorized("this browser agent was ended; connect again");
            }
        }
        if request.content_type() != Some("application/json") {
            return HttpResponse::json(
                415,
                &json!({"error": "unsupported_media_type", "error_description": "JSON-RPC is posted as application/json"}),
            );
        }
        let incoming: Value = match serde_json::from_slice(&request.body) {
            Ok(value) => value,
            Err(error) => {
                return HttpResponse::json(
                    400,
                    &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {error}")}}),
                );
            }
        };
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            self.backend.call(Request::Heartbeat {
                agent: grant.agent_id.clone(),
            }),
        )
        .await;
        let server = {
            let mut servers = self.servers.lock().unwrap_or_else(|e| e.into_inner());
            servers
                .entry(grant.agent_id.clone())
                .or_insert_with(|| {
                    Arc::new(
                        McpServer::new(
                            self.backend.clone(),
                            Identity {
                                id: grant.agent_id.clone(),
                                name: grant.agent_name.clone(),
                                registered_here: false,
                                host_pid: None,
                                host_started_at: None,
                            },
                        )
                        .remote(),
                    )
                })
                .clone()
        };
        match server.handle_incoming(incoming).await {
            Some(reply) => HttpResponse::json(200, &reply),
            None => HttpResponse::new(202),
        }
    }
}

/// `claude-browser-k3f9`: the runtime and four random characters.
fn generated_name(vendor: Vendor) -> String {
    let suffix: String = oauth::random_token()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(4)
        .collect::<String>()
        .to_ascii_lowercase();
    format!("{}-{suffix}", vendor.runtime())
}

/// A page with no assets to fetch and nothing to run.
fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title>\
         <style>body{{font:16px/1.5 system-ui,sans-serif;max-width:36rem;margin:3rem auto;padding:0 1rem;color:#1a1a1a}}label{{display:block;margin:1rem 0}}input{{font:inherit;padding:.4rem .6rem;width:100%;box-sizing:border-box}}button{{font:inherit;padding:.5rem 1.2rem}}code{{background:#f2f2f2;padding:.1rem .3rem}}.problem{{color:#a30000}}</style>\
         </head><body><h1>{title}</h1>{body}</body></html>",
        title = escape_html(title),
    )
}

async fn serve(client: Client, args: ServeArgs) -> Result<()> {
    let public = public_url(&args.public_url)?;
    let dir = match &args.project {
        Some(path) => path.clone(),
        None => std::env::current_dir()?,
    };
    let project = agentdocker_host::project::discover(&dir);
    if let Response::Error { message, .. } = client.call(&Request::Ping).await? {
        bail!("the daemon is not answering: {message}");
    }
    let listener = TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("cannot listen on {}", args.bind))?;
    let local = listener.local_addr()?;
    if !local.ip().is_loopback() {
        bail!(
            "the connector listens on loopback only; the tunnel in front of it is what is public"
        );
    }
    let path = state_path();
    let store = load_store(&path)?;
    let pairing = oauth::pairing_code();
    let connector = Arc::new(Connector::new(
        client,
        public.clone(),
        project.clone(),
        args.allow_callbacks.clone(),
        pairing.clone(),
        store,
        Some(path),
    ));
    eprintln!(
        "AgentDocker connector for project {} ({})\n  listening on http://{local}, published as {public}\n  MCP URL to add as a custom connector: {public}/mcp\n  pairing code: {pairing}   (typed on the consent page; new each time this runs)\n  Claude:  Settings › Connectors › Add custom connector › paste the URL › Connect\n  ChatGPT: Settings › Connectors › Advanced › Developer mode › Create › paste the URL, OAuth\n  Tunnel example: cloudflared tunnel --url http://{local}",
        project.name(),
        project.root.display(),
    );
    let limit = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = limit.clone().acquire_owned().await else {
            break;
        };
        let connector = connector.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let response = match http::read_request(&mut reader).await {
                Ok(request) => connector.handle(request).await,
                Err(http::HttpError::Closed | http::HttpError::Timeout) => return,
                Err(http::HttpError::Bad(status, why)) => HttpResponse::text(status, why),
                Err(http::HttpError::Io(_)) => return,
            };
            let _ = http::write_response(&mut write, &response).await;
            let _ = write.shutdown().await;
        });
    }
    Ok(())
}

async fn grants(client: &Client) -> Result<()> {
    let store = load_store(&state_path())?;
    if store.grants.is_empty() {
        eprintln!("no browser agent has connected through this connector");
        return Ok(());
    }
    let mut rows = Vec::new();
    for (id, grant) in &store.grants {
        let status = if grant.revoked_at.is_some() {
            "revoked".to_owned()
        } else {
            match client
                .call(&Request::Inspect {
                    agent: grant.agent_id.clone(),
                })
                .await
            {
                Ok(Response::Agent { agent }) if agent.status.is_live() => "active".to_owned(),
                Ok(Response::Agent { agent }) => format!("agent {}", agent.status),
                _ => "agent unknown to the daemon".to_owned(),
            }
        };
        rows.push(vec![
            grant.agent_name.clone(),
            grant.runtime.clone(),
            grant.vendor.label().to_owned(),
            grant.project.display().to_string(),
            crate::format::ago(grant.created_at),
            grant
                .last_used_at
                .map(crate::format::ago)
                .unwrap_or_else(|| "never".to_owned()),
            status,
            id[..8].to_owned(),
        ]);
    }
    crate::format::table(
        &[
            "AGENT",
            "RUNTIME",
            "VENDOR",
            "PROJECT",
            "CONNECTED",
            "LAST USED",
            "STATUS",
            "GRANT",
        ],
        &rows,
    );
    Ok(())
}

/// End a browser agent: tokens first, then the daemon's record. A serving
/// connector notices on that agent's next request.
async fn revoke(client: &Client, reference: &str) -> Result<()> {
    let path = state_path();
    let mut store = load_store(&path)?;
    let Some((grant_id, grant)) = store.grant_for_agent(reference) else {
        bail!("no active browser agent named `{reference}`; see `agentdocker connector grants`");
    };
    let agent_id = grant.agent_id.clone();
    let name = grant.agent_name.clone();
    store.revoke(&grant_id, Utc::now());
    save_store(&path, &store)?;
    match client
        .call(&Request::Deregister { agent: agent_id })
        .await?
    {
        Response::Ok | Response::Agent { .. } => {}
        Response::Error {
            code: ErrorCode::NotFound,
            ..
        } => {}
        Response::Error { message, .. } => eprintln!("tokens revoked; the daemon said: {message}"),
        _ => {}
    }
    eprintln!("{name}: access revoked and the agent marked finished");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::mock::Mock;
    use crate::mcp::REMOTE_TOOLS;
    use agentdocker_core::{AgentRecord, AgentStatus};

    /// The mock behind an `Arc`, so the connector can clone its backend
    /// for each browser agent's MCP server.
    #[derive(Clone)]
    struct Shared(Arc<Mock>);

    impl Backend for Shared {
        fn call(&self, request: Request) -> impl std::future::Future<Output = Result<Response>> {
            let inner = self.0.clone();
            async move { inner.call(request).await }
        }
    }

    fn live_agent(name: &str) -> Response {
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: name.into(),
                runtime: "claude-browser".into(),
                ..AgentSpec::default()
            },
            false,
            Utc::now(),
        );
        agent.status = AgentStatus::Running;
        Response::Agent { agent }
    }

    fn connector(responses: Vec<Response>) -> (Connector<Shared>, Arc<Mock>) {
        let mock = Arc::new(Mock::with(responses));
        let connector = Connector::new(
            Shared(mock.clone()),
            "https://x.trycloudflare.com".into(),
            agentdocker_core::ProjectRef {
                root: "/p/keel".into(),
                worktree: None,
                fingerprint: None,
                source: agentdocker_core::ProjectSource::Directory,
            },
            vec![],
            "ABCD-EFGH".into(),
            Store::default(),
            None,
        );
        (connector, mock)
    }

    fn get(path: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".into(),
            target: path.into(),
            headers: vec![],
            body: vec![],
        }
    }

    fn post(path: &str, content_type: &str, body: &str, bearer: Option<&str>) -> HttpRequest {
        let mut headers = vec![("content-type".to_owned(), content_type.to_owned())];
        if let Some(token) = bearer {
            headers.push(("authorization".to_owned(), format!("Bearer {token}")));
        }
        HttpRequest {
            method: "POST".into(),
            target: path.into(),
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    fn json_body(response: &HttpResponse) -> Value {
        serde_json::from_slice(&response.body).unwrap()
    }

    #[tokio::test]
    async fn metadata_names_this_server_and_its_one_scope() {
        let (connector, _) = connector(vec![]);
        let resource = json_body(
            &connector
                .handle(get("/.well-known/oauth-protected-resource"))
                .await,
        );
        assert_eq!(resource["resource"], "https://x.trycloudflare.com/mcp");
        assert_eq!(
            resource["authorization_servers"][0],
            "https://x.trycloudflare.com"
        );
        let server = json_body(
            &connector
                .handle(get("/.well-known/oauth-authorization-server"))
                .await,
        );
        assert_eq!(server["issuer"], "https://x.trycloudflare.com");
        assert_eq!(
            server["registration_endpoint"],
            "https://x.trycloudflare.com/register"
        );
        assert_eq!(server["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(
            server["token_endpoint_auth_methods_supported"],
            json!(["none"])
        );
        let unauthenticated = connector
            .handle(post("/mcp", "application/json", "{}", None))
            .await;
        assert_eq!(unauthenticated.status, 401);
        let challenge = unauthenticated
            .headers
            .iter()
            .find(|(n, _)| n == "WWW-Authenticate")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert!(challenge.contains(
            "resource_metadata=\"https://x.trycloudflare.com/.well-known/oauth-protected-resource\""
        ));
        assert_eq!(connector.handle(get("/mcp")).await.status, 405);
        assert_eq!(connector.handle(get("/nothing")).await.status, 404);
        let front = connector.handle(get("/")).await;
        assert!(String::from_utf8_lossy(&front.body).contains("https://x.trycloudflare.com/mcp"));
    }

    /// Registration, consent with the pairing code, the code exchange
    /// and a first MCP call, in the order the vendor performs them; the
    /// daemon sees one registration and the messaging tools only.
    #[tokio::test]
    async fn a_vendor_connects_a_browser_agent_end_to_end() {
        let (connector, mock) = connector(vec![
            Response::error(ErrorCode::NotFound, "no such agent"), // Inspect: the typed name is free
            live_agent("claude-browser-test"), // Register, at the token exchange
            live_agent("claude-browser-test"), // Inspect before initialize
            Response::Ok,                      // Heartbeat
            live_agent("claude-browser-test"), // Inspect before tools/list
            Response::Ok,                      // Heartbeat
            live_agent("claude-browser-test"), // Inspect before send_message
            Response::Ok,                      // Heartbeat
            Response::Ok,                      // Send
            live_agent("claude-browser-test"), // Inspect before the notification
            Response::Ok,                      // Heartbeat
            live_agent("claude-browser-test"), // Inspect before the refused claim
            Response::Ok,                      // Heartbeat
        ]);
        let registered = connector
            .handle(post(
                "/register",
                "application/json",
                r#"{"redirect_uris":["https://claude.ai/api/mcp/auth_callback"],"client_name":"Claude"}"#,
                None,
            ))
            .await;
        assert_eq!(registered.status, 201);
        let client_id = json_body(&registered)["client_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let verifier = "v".repeat(60);
        let challenge = oauth::s256(&verifier);

        let form = connector
            .handle(get(&format!(
                "/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fclaude.ai%2Fapi%2Fmcp%2Fauth_callback&code_challenge={challenge}&code_challenge_method=S256&state=s1"
            )))
            .await;
        assert_eq!(form.status, 200);
        let html = String::from_utf8_lossy(&form.body);
        assert!(html.contains("Claude (Claude)") && html.contains("<b>keel</b>"));
        assert!(html.contains("name=\"pairing_code\""));
        assert!(html.contains(&format!("value=\"{challenge}\"")));

        let consent = |code: &str, name: &str| {
            post(
                "/authorize",
                "application/x-www-form-urlencoded",
                &format!(
                    "response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fclaude.ai%2Fapi%2Fmcp%2Fauth_callback&code_challenge={challenge}&code_challenge_method=S256&state=s1&pairing_code={code}&agent_name={name}"
                ),
                None,
            )
        };
        let wrong = connector
            .handle(consent("ABCD-XXXX", "claude-browser-test"))
            .await;
        assert_eq!(wrong.status, 403);
        assert!(
            mock.requests().is_empty(),
            "a wrong code asks the daemon nothing"
        );
        let right = connector
            .handle(consent("abcd+efgh", "claude-browser-test"))
            .await;
        assert!(
            matches!(&mock.requests()[..], [Request::Inspect { agent }] if agent == "claude-browser-test"),
            "consent checks the name and registers nothing yet"
        );
        assert_eq!(
            right.status,
            302,
            "{}",
            String::from_utf8_lossy(&right.body)
        );
        let location = right
            .headers
            .iter()
            .find(|(n, _)| n == "Location")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(location.starts_with("https://claude.ai/api/mcp/auth_callback?code="));
        assert!(location.ends_with("&state=s1"));
        let code = location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned();

        let token = connector
            .handle(post(
                "/token",
                "application/x-www-form-urlencoded",
                &format!(
                    "grant_type=authorization_code&code={code}&client_id={client_id}&redirect_uri=https%3A%2F%2Fclaude.ai%2Fapi%2Fmcp%2Fauth_callback&code_verifier={verifier}"
                ),
                None,
            ))
            .await;
        assert_eq!(
            token.status,
            200,
            "{}",
            String::from_utf8_lossy(&token.body)
        );
        let issued = json_body(&token);
        assert_eq!(issued["token_type"], "Bearer");
        let access = issued["access_token"].as_str().unwrap().to_owned();
        assert!(
            matches!(
                &mock.requests()[1],
                Request::Register { spec, pid: None, .. }
                    if spec.name == "claude-browser-test"
                        && spec.runtime == "claude-browser"
                        && spec.workdir.as_deref() == Some(Path::new("/p/keel"))
                        && spec.labels.get("connector").map(String::as_str) == Some("true")
            ),
            "the agent is registered when the code is redeemed"
        );

        let init = connector
            .handle(post(
                "/mcp",
                "application/json",
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                Some(&access),
            ))
            .await;
        assert_eq!(init.status, 200);
        let reply = json_body(&init);
        assert!(
            reply["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("working inside a browser")
        );
        let listed = json_body(
            &connector
                .handle(post(
                    "/mcp",
                    "application/json",
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                    Some(&access),
                ))
                .await,
        );
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let mut names = names;
        names.sort_unstable();
        let mut expected = REMOTE_TOOLS.to_vec();
        expected.sort_unstable();
        assert_eq!(names, expected);
        let sent = connector
            .handle(post(
                "/mcp",
                "application/json",
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"claude-2c8d1062","text":"the docs page says the flag was renamed"}}}"#,
                Some(&access),
            ))
            .await;
        assert_eq!(sent.status, 200);
        assert!(json_body(&sent)["error"].is_null());
        let requests = mock.requests();
        assert!(
            requests
                .iter()
                .any(|r| matches!(r, Request::Heartbeat { .. }))
        );
        assert!(matches!(
            requests.last().unwrap(),
            Request::Send { to, .. } if to == "claude-2c8d1062"
        ));
        let notification = connector
            .handle(post(
                "/mcp",
                "application/json",
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                Some(&access),
            ))
            .await;
        assert_eq!(notification.status, 202);
        let refused = json_body(
            &connector
                .handle(post(
                    "/mcp",
                    "application/json",
                    r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"claim","arguments":{"resource":"path:/p/keel"}}}"#,
                    Some(&access),
                ))
                .await,
        );
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no checkout here")
        );
    }

    /// An agent the daemon has ended (revoked, or deregistered by hand)
    /// loses its tokens on its next request, and a client's callback that
    /// is not a vendor's never gets a code.
    #[tokio::test]
    async fn an_ended_agent_is_refused_and_foreign_callbacks_never_register() {
        let (connector, _) = connector(vec![live_agent("b"), {
            let mut ended = AgentRecord::new(
                AgentSpec {
                    name: "b".into(),
                    ..AgentSpec::default()
                },
                false,
                Utc::now(),
            );
            ended.status = AgentStatus::Exited { code: Some(0) };
            Response::Agent { agent: ended }
        }]);
        let foreign = connector
            .handle(post(
                "/register",
                "application/json",
                r#"{"redirect_uris":["https://attacker.example/cb"]}"#,
                None,
            ))
            .await;
        assert_eq!(foreign.status, 400);
        assert_eq!(json_body(&foreign)["error"], "invalid_redirect_uri");

        let registered = json_body(
            &connector
                .handle(post(
                    "/register",
                    "application/json",
                    r#"{"redirect_uris":["https://chatgpt.com/connector_platform_oauth_redirect"]}"#,
                    None,
                ))
                .await,
        );
        let client_id = registered["client_id"].as_str().unwrap().to_owned();
        let verifier = "v".repeat(60);
        let consent = connector
            .handle(post(
                "/authorize",
                "application/x-www-form-urlencoded",
                &format!(
                    "response_type=code&client_id={client_id}&code_challenge={}&code_challenge_method=S256&pairing_code=ABCD-EFGH",
                    oauth::s256(&verifier)
                ),
                None,
            ))
            .await;
        assert_eq!(consent.status, 302);
        let location = consent
            .headers
            .iter()
            .find(|(n, _)| n == "Location")
            .unwrap()
            .1
            .clone();
        assert!(
            location.starts_with("https://chatgpt.com/connector_platform_oauth_redirect?code=")
        );
        let code = location.split("code=").nth(1).unwrap().to_owned();
        let issued = json_body(
            &connector
                .handle(post(
                    "/token",
                    "application/x-www-form-urlencoded",
                    &format!("grant_type=authorization_code&code={code}&code_verifier={verifier}"),
                    None,
                ))
                .await,
        );
        let access = issued["access_token"].as_str().unwrap().to_owned();
        let gone = connector
            .handle(post(
                "/mcp",
                "application/json",
                r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
                Some(&access),
            ))
            .await;
        assert_eq!(gone.status, 401);
        assert!(
            json_body(&gone)["error_description"]
                .as_str()
                .unwrap()
                .contains("ended")
        );
        let refresh = connector
            .handle(post(
                "/token",
                "application/x-www-form-urlencoded",
                &format!(
                    "grant_type=refresh_token&refresh_token={}",
                    issued["refresh_token"].as_str().unwrap()
                ),
                None,
            ))
            .await;
        assert_eq!(refresh.status, 400);
        assert_eq!(json_body(&refresh)["error"], "invalid_grant");
    }

    #[test]
    fn the_public_url_is_https_or_loopback() {
        assert_eq!(
            public_url("https://x.trycloudflare.com/").unwrap(),
            "https://x.trycloudflare.com"
        );
        assert_eq!(
            public_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert!(public_url("http://example.com").is_err());
        assert!(public_url("ftp://x").is_err());
        assert!(public_url("https://x/?a=1").is_err());
    }

    #[test]
    fn the_state_file_round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connector/state.json");
        assert!(
            load_store(&path).unwrap().grants.is_empty(),
            "absent is empty"
        );
        let mut store = Store::default();
        store
            .register_client(
                &json!({"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"]}),
                &[],
                Utc::now(),
            )
            .unwrap();
        save_store(&path, &store).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(load_store(&path).unwrap().clients, store.clients);
        std::fs::write(&path, "{broken").unwrap();
        assert!(load_store(&path).is_err());
    }
}
