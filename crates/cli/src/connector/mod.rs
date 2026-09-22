//! The remote connector: how an agent that works inside a browser — the
//! Claude side panel, the ChatGPT extension — reaches the agents on this
//! machine. Those sessions run on the vendor's side and speak to tools
//! only over public HTTPS with OAuth, so this is a separate, opt-in
//! process: it binds loopback, a tunnel gives it a public name, and it
//! serves the vendors an MCP endpoint whose tools are the messaging ones.
//! One connector serves every project the daemon knows: each consent
//! chooses the project its browser-agent identity joins. The daemon
//! itself never listens on the network.

pub mod cimd;
pub mod http;
pub mod net;
pub mod oauth;
pub mod service;
pub mod tunnel;

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
    /// Serve the connector on loopback, behind a tunnel you run or one it starts for you.
    Serve(ServeArgs),
    /// Whether a connector is serving here: its address, pairing code and tunnel.
    Status,
    /// Run the connector as a login service (launchd or systemd) with these serve arguments.
    Install(InstallArgs),
    /// Remove the connector service.
    Uninstall {
        /// Say what would be removed without removing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// The browser agents that have connected, and whether they still can.
    Grants,
    /// End a browser agent's access: its tokens stop working and the agent is marked finished.
    Revoke {
        /// The agent's name or id, or the grant id from `grants`.
        agent: String,
    },
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    #[command(flatten)]
    pub serve: ServeArgs,
    /// Say what would be written and run without doing it.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug, Default, Clone)]
pub struct ServeArgs {
    /// The HTTPS address the tunnel publishes this connector at, without a
    /// trailing slash: the vendors add `/mcp` to it. Not needed with a
    /// quick tunnel, which chooses its own; required with a named one.
    #[arg(long)]
    pub public_url: Option<String>,
    /// The loopback address to listen on; port 0 picks a free one.
    #[arg(long, default_value = "127.0.0.1:0")]
    pub bind: String,
    /// The project the consent page proposes first; the person may pick any project on this machine there.
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// A callback URL to admit besides the vendors' own, for a hosted client of yours.
    #[arg(long = "allow-callback")]
    pub allow_callbacks: Vec<String>,
    /// Start the tunnel too: `tailscale` (Funnel on this machine's own stable `*.ts.net` name) or `cloudflared` (a quick tunnel with a random hostname, or with --tunnel-name a named one you routed).
    #[arg(long, value_parser = ["tailscale", "cloudflared"])]
    pub tunnel: Option<String>,
    /// The named cloudflared tunnel to run (`cloudflared tunnel create <name>` and a DNS route first); needs --public-url.
    #[arg(long, requires = "tunnel")]
    pub tunnel_name: Option<String>,
    /// The public HTTPS port for Tailscale Funnel: 443, 8443 or 10000.
    #[arg(long, default_value_t = 443, requires = "tunnel")]
    pub tunnel_port: u16,
    /// Where cloudflared is, when not on PATH or in the usual places.
    #[arg(long, requires = "tunnel")]
    pub cloudflared: Option<PathBuf>,
    /// Where the tailscale CLI is, when not on PATH, in the usual places or in the macOS app.
    #[arg(long, requires = "tunnel")]
    pub tailscale: Option<PathBuf>,
    /// Only admit these addresses at /register, /token and /mcp: a CIDR, `anthropic`, `openai` (its HTTPS feed, refreshed hourly), or `@<file>` (local feed JSON or CIDRs, re-read when changed). Needs the tunnel's client-address header.
    #[arg(long = "allow-from")]
    pub allow_from: Vec<String>,
    /// The header the tunnel writes the client address into (cloudflared: cf-connecting-ip, the default with --tunnel cloudflared).
    #[arg(long)]
    pub client_ip_header: Option<String>,
}

impl ServeArgs {
    /// The arguments back as given, for a service definition.
    pub fn to_argv(&self) -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(url) = &self.public_url {
            argv.extend(["--public-url".to_owned(), url.clone()]);
        }
        if self.bind != "127.0.0.1:0" {
            argv.extend(["--bind".to_owned(), self.bind.clone()]);
        }
        if let Some(project) = &self.project {
            argv.extend([
                "--project".to_owned(),
                project.to_string_lossy().into_owned(),
            ]);
        }
        for callback in &self.allow_callbacks {
            argv.extend(["--allow-callback".to_owned(), callback.clone()]);
        }
        if let Some(tunnel) = &self.tunnel {
            argv.extend(["--tunnel".to_owned(), tunnel.clone()]);
        }
        if let Some(name) = &self.tunnel_name {
            argv.extend(["--tunnel-name".to_owned(), name.clone()]);
        }
        if self.tunnel_port != 443 {
            argv.extend(["--tunnel-port".to_owned(), self.tunnel_port.to_string()]);
        }
        if let Some(path) = &self.cloudflared {
            argv.extend([
                "--cloudflared".to_owned(),
                path.to_string_lossy().into_owned(),
            ]);
        }
        if let Some(path) = &self.tailscale {
            argv.extend([
                "--tailscale".to_owned(),
                path.to_string_lossy().into_owned(),
            ]);
        }
        for allow in &self.allow_from {
            argv.extend(["--allow-from".to_owned(), allow.clone()]);
        }
        if let Some(header) = &self.client_ip_header {
            argv.extend(["--client-ip-header".to_owned(), header.clone()]);
        }
        argv
    }

    /// The header to read the client address from: the one given, else
    /// cloudflared's when it runs the tunnel.
    fn client_ip_header(&self) -> String {
        match &self.client_ip_header {
            Some(header) => header.clone(),
            None if self.tunnel.as_deref() == Some("cloudflared") => "cf-connecting-ip".into(),
            None if self.tunnel.as_deref() == Some("tailscale") => "x-forwarded-for".into(),
            None => String::new(),
        }
    }
}

pub async fn run(client: Client, args: ConnectorArgs) -> Result<()> {
    match args.command {
        ConnectorCommand::Serve(args) => serve(client, args).await,
        ConnectorCommand::Status => status(&client).await,
        ConnectorCommand::Install(args) => service::install(&args.serve, args.dry_run),
        ConnectorCommand::Uninstall { dry_run } => service::uninstall(dry_run),
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
    /// Proposed first on the consent page; the person may choose another.
    default_project: Option<agentdocker_core::ProjectRef>,
    extra_callbacks: Vec<String>,
    pairing_code: String,
    store: Mutex<Store>,
    state_path: Option<PathBuf>,
    servers: Mutex<HashMap<String, Arc<McpServer<B>>>>,
    consent_failures: AtomicU32,
    allowlist: Option<net::Allowlist>,
    fetcher: Arc<cimd::Fetcher>,
}

/// A project the consent page offers: its root and the name shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectChoice {
    pub root: PathBuf,
    pub name: String,
}

impl<B: Backend + Clone + Send + Sync + 'static> Connector<B> {
    pub fn new(
        backend: B,
        public_url: String,
        default_project: Option<agentdocker_core::ProjectRef>,
        extra_callbacks: Vec<String>,
        pairing_code: String,
        mut store: Store,
        state_path: Option<PathBuf>,
    ) -> Self {
        store.issuer = public_url.clone();
        Self {
            backend,
            public_url,
            default_project,
            extra_callbacks,
            pairing_code,
            store: Mutex::new(store),
            state_path,
            servers: Mutex::new(HashMap::new()),
            consent_failures: AtomicU32::new(0),
            allowlist: None,
            fetcher: Arc::new(cimd::fetch),
        }
    }

    /// Admit only these addresses at the vendor-facing endpoints.
    pub fn with_allowlist(mut self, allowlist: Option<net::Allowlist>) -> Self {
        self.allowlist = allowlist;
        self
    }

    /// What fetches a client's metadata document; tests supply a table.
    #[cfg(test)]
    pub fn with_fetcher(mut self, fetcher: Arc<cimd::Fetcher>) -> Self {
        self.fetcher = fetcher;
        self
    }

    /// The projects the daemon has seen agents in, by root, with the
    /// default first when there is one. A daemon that does not answer
    /// leaves the default alone: the page still works, with less choice.
    async fn known_projects(&self) -> Vec<ProjectChoice> {
        let mut choices: Vec<ProjectChoice> = self
            .default_project
            .iter()
            .map(|p| ProjectChoice {
                root: p.root.clone(),
                name: p.name(),
            })
            .collect();
        if let Ok(Response::Agents { agents, .. }) = self
            .backend
            .call(Request::List {
                all: true,
                project: None,
                labels: Default::default(),
            })
            .await
        {
            let mut roots: Vec<agentdocker_core::ProjectRef> = agents
                .into_iter()
                .filter_map(|agent| agent.project)
                .collect();
            roots.sort_by(|a, b| a.root.cmp(&b.root));
            roots.dedup_by(|a, b| a.root == b.root);
            for project in roots {
                if !choices.iter().any(|c| c.root == project.root) {
                    choices.push(ProjectChoice {
                        name: project.name(),
                        root: project.root,
                    });
                }
            }
        }
        choices
    }

    /// The project a consent form names, if it names one that is here:
    /// a typed folder first, then the chosen entry, then the default.
    /// A path that is not a directory on this machine is a problem the
    /// page shows, not a project.
    fn chosen_project(
        &self,
        form: &[(String, String)],
    ) -> Result<Option<agentdocker_core::ProjectRef>, &'static str> {
        let typed = field(form, "project_path")
            .map(str::trim)
            .filter(|p| !p.is_empty());
        let picked = field(form, "project")
            .map(str::trim)
            .filter(|p| !p.is_empty());
        let Some(named) = typed.or(picked) else {
            return Ok(self.default_project.clone());
        };
        if let Some(default) = &self.default_project
            && default.root == Path::new(named)
        {
            return Ok(Some(default.clone()));
        }
        let path = Path::new(named);
        if !path.is_absolute() {
            return Err("The project is a full path to a folder on this machine.");
        }
        if !path.is_dir() {
            return Err("That folder is not on this machine.");
        }
        Ok(Some(agentdocker_host::project::discover(path)))
    }

    /// A URL-formatted `client_id` is its own registration: fetch its
    /// metadata document (once an hour) and admit it, or say why not
    /// before anything is sent to a callback it names.
    async fn ensure_metadata_client(&self, client_id: &str) -> Result<(), OAuthError> {
        if !oauth::is_metadata_client_id(client_id) {
            return Ok(());
        }
        {
            let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            if store.metadata_client_fresh(client_id, Utc::now()) {
                return Ok(());
            }
            if store.clients.len() >= MAX_CLIENTS && !store.clients.contains_key(client_id) {
                return Err(OAuthError::new(
                    "invalid_client",
                    "this connector holds as many clients as it will; revoke or restart",
                ));
            }
        }
        let host = oauth::host_of(client_id).unwrap_or_default();
        if Vendor::of_metadata_host(host, &self.extra_callbacks).is_none() {
            return Err(OAuthError::new(
                "invalid_client",
                format!(
                    "{host} is not a host this connector fetches client metadata from; only the vendors' hosted surfaces can connect"
                ),
            ));
        }
        let fetcher = self.fetcher.clone();
        let url = client_id.to_owned();
        let document = tokio::task::spawn_blocking(move || fetcher(&url))
            .await
            .map_err(|_| {
                OAuthError::new("invalid_client", "the metadata fetch did not finish")
            })??;
        {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.admit_metadata_client(client_id, &document, &self.extra_callbacks, Utc::now())?;
        }
        self.persist();
        eprintln!("agentdocker connector: admitted the client metadata document at {client_id}");
        Ok(())
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
        // The vendor speaks to these three; the person's browser to the
        // rest. An allowlist is exact: no header, no entry.
        if matches!(request.path(), "/register" | "/token" | "/mcp")
            && let Some(allowlist) = &self.allowlist
            && !allowlist.allows(&request)
        {
            eprintln!(
                "agentdocker connector: refused {} from {} (not in --allow-from)",
                request.path(),
                allowlist
                    .client_ip(&request)
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "an unreported address".to_owned())
            );
            return HttpResponse::json(
                403,
                &json!({"error": "forbidden", "error_description": "this address is not one the connector admits"}),
            );
        }
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
            ("GET", "/authorize") => self.consent_form(&request).await,
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
            "resource_name": "AgentDocker connector",
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
            // A vendor's own metadata document stands in for registration
            // (draft-ietf-oauth-client-id-metadata-document); ChatGPT uses
            // its one stable client and callback when the issuer is named
            // in every authorization response (RFC 9207).
            "client_id_metadata_document_supported": true,
            "authorization_response_iss_parameter_supported": true,
        })
    }

    fn front_page(&self) -> HttpResponse {
        HttpResponse::html(
            200,
            page(
                "AgentDocker connector",
                &format!(
                    "<p>This is an AgentDocker connector. It lets an agent working inside a browser join the messaging of a project on this machine.</p>\
                     <p>Add <code>{}</code> as a custom connector: in Claude under <i>Settings › Connectors › Add custom connector</i>, in ChatGPT under <i>Settings › Connectors › Advanced › Developer mode</i>. Consent asks for the pairing code shown by <code>agentdocker connector status</code> or the desktop's Tools screen, and which project the agent joins.</p>\
                     <p>A browser agent has no checkout here: it can find agents, message them, read its inbox and the journal, and nothing else.</p>",
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
    async fn consent_form(&self, request: &HttpRequest) -> HttpResponse {
        let params = http::parse_query(request.query());
        if let Some(client_id) = field(&params, "client_id")
            && let Err(error) = self.ensure_metadata_client(client_id).await
        {
            return self.refuse_authorization(AuthorizeRefusal::Page(error));
        }
        let pending = {
            let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.begin_authorization(&params)
        };
        match pending {
            Ok(pending) => {
                let projects = self.known_projects().await;
                HttpResponse::html(200, self.consent_html(&pending, &projects, None))
            }
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

    /// The consent page: who asks, the pairing code, a name, and which
    /// project the agent joins — the ones the daemon knows to pick from,
    /// or any folder on this machine typed in.
    fn consent_html(
        &self,
        pending: &oauth::Pending,
        projects: &[ProjectChoice],
        problem: Option<&str>,
    ) -> String {
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
        let who = escape_html(&pending.shown_as);
        let suggested = generated_name(pending.vendor);
        let problem = problem
            .map(|p| format!("<p class=\"problem\">{}</p>", escape_html(p)))
            .unwrap_or_default();
        let options: String = projects
            .iter()
            .enumerate()
            .map(|(i, p)| {
                format!(
                    "<option value=\"{}\"{}>{} — {}</option>",
                    escape_html(&p.root.to_string_lossy()),
                    if i == 0 { " selected" } else { "" },
                    escape_html(&p.name),
                    escape_html(&p.root.to_string_lossy())
                )
            })
            .collect();
        let chooser = if projects.is_empty() {
            "<label>Project this agent joins: the full path of a folder on this machine<br><input name=\"project_path\" required placeholder=\"/Users/you/project\"></label>".to_owned()
        } else {
            format!(
                "<label>Project this agent joins<br><select name=\"project\">{options}</select></label>\
                 <label>…or the full path of another folder on this machine<br><input name=\"project_path\" placeholder=\"/Users/you/project\"></label>"
            )
        };
        page(
            "Connect a browser agent",
            &format!(
                "<p><b>{who}</b> asks to join a project on this machine as a browser agent. It will be able to find that project's agents, message them, read its own inbox and the journal. It gets no files, leases or worktrees.</p>\
                 {problem}\
                 <form method=\"post\" action=\"/authorize\">{hidden}\
                 <label>Pairing code, from <code>agentdocker connector status</code> or the desktop's Tools screen<br><input name=\"pairing_code\" autocomplete=\"off\" autofocus required placeholder=\"ABCD-EFGH\"></label>\
                 {chooser}\
                 <label>Name for this agent<br><input name=\"agent_name\" value=\"{suggested}\" maxlength=\"64\" pattern=\"[A-Za-z0-9._-]+\"></label>\
                 <button type=\"submit\">Connect</button></form>"
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
        if let Some(client_id) = field(&form, "client_id")
            && let Err(error) = self.ensure_metadata_client(client_id).await
        {
            return self.refuse_authorization(AuthorizeRefusal::Page(error));
        }
        let pending = {
            let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.begin_authorization(&form)
        };
        let pending = match pending {
            Ok(pending) => pending,
            Err(refusal) => return self.refuse_authorization(refusal),
        };
        // The page comes back with what was chosen, so a slip costs one
        // field, not the whole form.
        let again = |problem: &str| {
            let chosen: Vec<ProjectChoice> = field(&form, "project")
                .filter(|p| !p.trim().is_empty())
                .map(|p| ProjectChoice {
                    root: PathBuf::from(p),
                    name: Path::new(p)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.to_owned()),
                })
                .into_iter()
                .chain(self.default_project.iter().map(|p| ProjectChoice {
                    root: p.root.clone(),
                    name: p.name(),
                }))
                .fold(Vec::new(), |mut choices: Vec<ProjectChoice>, choice| {
                    if !choices.iter().any(|c| c.root == choice.root) {
                        choices.push(choice);
                    }
                    choices
                });
            self.consent_html(&pending, &chosen, Some(problem))
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
                again("That pairing code is not the one this connector shows."),
            );
        }
        let project = match self.chosen_project(&form) {
            Ok(Some(project)) => project,
            Ok(None) => {
                return HttpResponse::html(400, again("Choose the project this agent joins."));
            }
            Err(problem) => return HttpResponse::html(400, again(problem)),
        };
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
                again("That name is already a live agent's here; pick another."),
            );
        }
        let redirect = {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.complete_authorization(
                &pending,
                name.clone(),
                generated,
                project.root.clone(),
                Utc::now(),
            )
        };
        eprintln!(
            "agentdocker connector: {} consented to join {} as {}; waiting for the token exchange",
            pending.vendor.label(),
            project.name(),
            name
        );
        HttpResponse::redirect(&redirect)
    }

    /// One external, pidless agent per redeemed consent, in the project
    /// the consent chose. It stays live until revoked.
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
                workdir: Some(consent.project.clone()),
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
                        project: consent.project.clone(),
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
                                consent.project.display()
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
            // The daemon answered: the agent is over. Only its answer ends
            // a grant; a daemon that is not answering ends nothing.
            Ok(Response::Agent { .. })
            | Ok(Response::Error {
                code: ErrorCode::NotFound,
                ..
            }) => {
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
            Ok(other) => {
                eprintln!("agentdocker connector: unexpected reply to inspect: {other:?}");
                return HttpResponse::json(
                    503,
                    &json!({"error": "daemon_unavailable", "error_description": "the daemon gave an unexpected answer; try again"}),
                )
                .header("Retry-After", "5");
            }
            Err(error) => {
                eprintln!("agentdocker connector: the daemon is not answering: {error:#}");
                return HttpResponse::json(
                    503,
                    &json!({"error": "daemon_unavailable", "error_description": "the daemon is not answering; the grant stands, try again"}),
                )
                .header("Retry-After", "5");
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
                        .remote(grant.project.clone()),
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
    let default_project = match &args.project {
        Some(path) => {
            if !path.is_dir() {
                bail!("--project {} is not a directory here", path.display());
            }
            Some(agentdocker_host::project::discover(path))
        }
        None => None,
    };
    if let Response::Error { message, .. } = client.call(&Request::Ping).await? {
        bail!("the daemon is not answering: {message}");
    }
    let allowlist = net::Allowlist::parse(&args.allow_from, &args.client_ip_header())?;
    if let Some(list) = &allowlist {
        // Fail before opening a tunnel when the explicitly selected preset
        // cannot establish its initial admission list.
        list.refresh_openai().await?;
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
    // The tunnel first, so the public name is known before anything is
    // said or written; it dies with this process and this process with it.
    let mut tunnel = match args.tunnel.as_deref() {
        Some("tailscale") => {
            let binary = tunnel::find_tailscale(args.tailscale.as_deref())?;
            Some(tunnel::funnel_tailscale(&binary, local.port(), args.tunnel_port).await?)
        }
        Some("cloudflared") => {
            let binary = tunnel::find_cloudflared(args.cloudflared.as_deref())?;
            let checked = args.public_url.as_deref().map(public_url).transpose()?;
            Some(
                tunnel::spawn_cloudflared(
                    &binary,
                    local.port(),
                    args.tunnel_name.as_deref(),
                    checked.as_deref(),
                )
                .await?,
            )
        }
        Some(other) => bail!("unknown tunnel `{other}`"),
        None => None,
    };
    let public = match (&tunnel, &args.public_url) {
        (Some(tunnel), _) => tunnel.public_url.clone(),
        (None, Some(url)) => public_url(url)?,
        (None, None) => bail!(
            "--public-url is required without --tunnel: the HTTPS address your tunnel publishes this connector at"
        ),
    };
    let prefixes = allowlist.as_ref().map(net::Allowlist::len).unwrap_or(0);
    let home = agentdocker_host::dirs::home();
    let path = state_path();
    let store = load_store(&path)?;
    let pairing = oauth::pairing_code();
    let connector = Arc::new(
        Connector::new(
            client,
            public.clone(),
            default_project.clone(),
            args.allow_callbacks.clone(),
            pairing.clone(),
            store,
            Some(path),
        )
        .with_allowlist(allowlist),
    );
    let pid = std::process::id();
    let serving = service::Serving {
        pid,
        public_url: public.clone(),
        bind: local.to_string(),
        default_project: default_project.as_ref().map(|p| p.root.clone()),
        pairing_code: pairing.clone(),
        started_at: Utc::now(),
        tunnel: tunnel.as_ref().map(|t| service::TunnelStatus {
            provider: t.provider.to_owned(),
            pid: t.pid(),
            name: t.name.clone(),
        }),
        allowlist_prefixes: prefixes,
    };
    if let Err(error) = service::write_status(&home, &serving) {
        eprintln!("agentdocker connector: could not write the status file: {error:#}");
    }
    eprintln!(
        "AgentDocker connector{}\n  listening on http://{local}, published as {public}{}\n  MCP URL to add as a custom connector: {public}/mcp\n  pairing code: {pairing}   (typed on the consent page; new each time this runs)\n  admitted addresses: {}\n  Claude:  Settings › Connectors › Add custom connector › paste the URL › Connect\n  ChatGPT: Settings › Connectors › Advanced › Developer mode › Create › paste the URL, OAuth{}",
        match &default_project {
            Some(p) => format!(
                ", proposing project {} ({}) at consent",
                p.name(),
                p.root.display()
            ),
            None => ", every project on this machine (chosen at consent)".to_owned(),
        },
        match &tunnel {
            Some(t) => match t.pid() {
                Some(pid) => format!(" by {} (pid {pid})", t.provider),
                None => format!(" by {} funnel", t.provider),
            },
            None => String::new(),
        },
        if prefixes == 0 {
            "any (no --allow-from)".to_owned()
        } else {
            format!("{prefixes} prefixes (--allow-from)")
        },
        if tunnel.is_none() {
            format!("\n  Tunnel example: cloudflared tunnel --url http://{local}")
        } else {
            String::new()
        }
    );
    let limit = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut shutdown = shutdown_signal();
    let mut tunnel_check = tokio::time::interval(std::time::Duration::from_secs(2));
    let refresh = refresh_allowlist(connector.allowlist.as_ref(), &home, serving.clone());
    tokio::pin!(refresh);
    let outcome = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        // One refused connection (descriptors exhausted, a
                        // reset) is not a reason to stop serving the rest.
                        eprintln!("agentdocker connector: accept failed: {error}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let Ok(permit) = limit.clone().acquire_owned().await else {
                    break Ok(());
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
            _ = tunnel_check.tick() => {
                if let Some(t) = tunnel.as_mut()
                    && let Some(status) = t.exited()
                {
                    break Err(anyhow::anyhow!(
                        "the tunnel exited ({status}); the connector stops with it"
                    ));
                }
            }
            _ = &mut refresh => unreachable!("allowlist refresh loop does not terminate"),
            _ = &mut shutdown => {
                eprintln!("agentdocker connector: stopping");
                break Ok(());
            }
        }
    };
    service::clear_status(&home, pid);
    if let Some(t) = tunnel.as_mut() {
        t.stop().await;
    }
    outcome
}

/// Polled beside HTTP acceptance, rather than detached: shutdown drops the
/// loop, so an in-flight bounded fetch cannot later rewrite the status file.
async fn refresh_allowlist(
    list: Option<&net::Allowlist>,
    home: &Path,
    mut serving: service::Serving,
) {
    let Some(list) = list.filter(|list| list.auto_refresh()) else {
        return std::future::pending().await;
    };
    loop {
        // Sleep after each attempt: one request at a time, no catch-up burst
        // after sleep/wake, and no immediate retry loop during an outage.
        tokio::time::sleep(net::REFRESH_INTERVAL).await;
        match list.refresh_openai().await {
            Ok(()) => {
                serving.allowlist_prefixes = list.len();
                if let Err(error) = service::write_status(home, &serving) {
                    eprintln!("agentdocker connector: could not update prefix count: {error}");
                }
            }
            Err(error) => eprintln!(
                "agentdocker connector: OpenAI egress refresh failed; keeping last valid list: {error:#}"
            ),
        }
    }
}

/// Ctrl-C, or the SIGTERM a service manager sends.
fn shutdown_signal() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async {
        #[cfg(unix)]
        {
            let mut term =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(term) => term,
                    Err(_) => {
                        let _ = tokio::signal::ctrl_c().await;
                        return;
                    }
                };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    })
}

/// What is serving here, from the status file and the process table.
async fn status(client: &Client) -> Result<()> {
    let home = agentdocker_host::dirs::home();
    let Some(serving) = service::read_status(&home) else {
        eprintln!(
            "no connector is serving here (no status file); `agentdocker connector serve` or `connector install`"
        );
        return Ok(());
    };
    let alive = agentdocker_host::procinfo::alive(serving.pid);
    if !alive {
        eprintln!(
            "the connector that started {} (pid {}) is not running; its last address was {}",
            crate::format::ago(serving.started_at),
            serving.pid,
            serving.public_url
        );
        return Ok(());
    }
    println!(
        "serving since {} (pid {})",
        crate::format::ago(serving.started_at),
        serving.pid
    );
    println!(
        "  project:      {}",
        match &serving.default_project {
            Some(p) => format!(
                "{} proposed; any project on this machine at consent",
                p.display()
            ),
            None => "chosen at consent, any project on this machine".to_owned(),
        }
    );
    println!("  MCP URL:      {}/mcp", serving.public_url);
    println!("  listening on: http://{}", serving.bind);
    println!("  pairing code: {}", serving.pairing_code);
    match &serving.tunnel {
        Some(t) => println!(
            "  tunnel:       {}{}{}",
            t.provider,
            t.name
                .as_deref()
                .map(|n| format!(" `{n}`"))
                .unwrap_or_default(),
            t.pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default()
        ),
        None => println!("  tunnel:       yours, in front of the listening address"),
    }
    println!(
        "  admitted:     {}",
        if serving.allowlist_prefixes == 0 {
            "any address".to_owned()
        } else {
            format!("{} prefixes", serving.allowlist_prefixes)
        }
    );
    let store = load_store(&state_path())?;
    let mut active = 0;
    for grant in store.grants.values().filter(|g| g.active()) {
        if let Ok(Response::Agent { agent }) = client
            .call(&Request::Inspect {
                agent: grant.agent_id.clone(),
            })
            .await
            && agent.status.is_live()
        {
            active += 1;
        }
    }
    println!(
        "  browser agents: {active} live of {} consents (`connector grants`)",
        store.grants.len()
    );
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
    /// for each browser agent's MCP server; `down` makes every call fail
    /// the way a stopped daemon's socket does.
    #[derive(Clone)]
    struct Shared(Arc<Mock>, Arc<std::sync::atomic::AtomicBool>);

    impl Backend for Shared {
        fn call(&self, request: Request) -> impl std::future::Future<Output = Result<Response>> {
            let inner = self.0.clone();
            let down = self.1.load(Ordering::Relaxed);
            async move {
                if down {
                    anyhow::bail!("connection refused");
                }
                inner.call(request).await
            }
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
        let (connector, mock, _) = connector_with_switch(responses);
        (connector, mock)
    }

    fn connector_with_switch(
        responses: Vec<Response>,
    ) -> (
        Connector<Shared>,
        Arc<Mock>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let mock = Arc::new(Mock::with(responses));
        let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let connector = Connector::new(
            Shared(mock.clone(), down.clone()),
            "https://x.trycloudflare.com".into(),
            Some(agentdocker_core::ProjectRef {
                root: "/p/keel".into(),
                worktree: None,
                fingerprint: None,
                source: agentdocker_core::ProjectSource::Directory,
            }),
            vec![],
            "ABCD-EFGH".into(),
            Store::default(),
            None,
        );
        (connector, mock, down)
    }

    /// Consent and redeem a code for a generated name, as a vendor would,
    /// and return the access token.
    async fn connected(connector: &Connector<Shared>, callback: &str) -> String {
        let registered = json_body(
            &connector
                .handle(post(
                    "/register",
                    "application/json",
                    &format!(r#"{{"redirect_uris":["{callback}"]}}"#),
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
        assert_eq!(
            consent.status,
            302,
            "{}",
            String::from_utf8_lossy(&consent.body)
        );
        let location = consent
            .headers
            .iter()
            .find(|(n, _)| n == "Location")
            .unwrap()
            .1
            .clone();
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
        issued["access_token"].as_str().unwrap().to_owned()
    }

    /// A daemon that is not answering is a `503` and nothing more: the
    /// grant stands, and the same token works once the daemon is back.
    #[tokio::test]
    async fn a_silent_daemon_does_not_end_a_grant() {
        let (connector, mock, down) = connector_with_switch(vec![
            live_agent("b"), // Register
            live_agent("b"), // Inspect, once the daemon is back
            Response::Ok,    // Heartbeat
        ]);
        let access = connected(&connector, "https://claude.ai/api/mcp/auth_callback").await;
        down.store(true, Ordering::Relaxed);
        let outage = connector
            .handle(post(
                "/mcp",
                "application/json",
                r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
                Some(&access),
            ))
            .await;
        assert_eq!(outage.status, 503);
        assert!(
            outage
                .headers
                .iter()
                .any(|(n, v)| n == "Retry-After" && v == "5")
        );
        assert_eq!(json_body(&outage)["error"], "daemon_unavailable");
        down.store(false, Ordering::Relaxed);
        let back = connector
            .handle(post(
                "/mcp",
                "application/json",
                r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
                Some(&access),
            ))
            .await;
        assert_eq!(back.status, 200, "{}", String::from_utf8_lossy(&back.body));
        assert!(
            mock.requests()
                .iter()
                .any(|r| matches!(r, Request::Heartbeat { .. })),
            "the daemon saw the agent again"
        );
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
            Response::Agents {
                agents: vec![],
                aliases: Default::default(),
            }, // List, for the consent page's project chooser
            Response::error(ErrorCode::NotFound, "no such agent"), // Inspect: the typed name is free
            live_agent("claude-browser-test"), // Register, at the token exchange
            live_agent("claude-browser-test"), // Inspect before initialize
            Response::Ok,                      // Heartbeat
            live_agent("claude-browser-test"), // Inspect before tools/list
            Response::Ok,                      // Heartbeat
            live_agent("claude-browser-test"), // Inspect before send_message
            Response::Ok,                      // Heartbeat
            Response::Ok,                      // Send
            live_agent("claude-browser-test"), // Inspect before the broadcast
            Response::Ok,                      // Heartbeat
            Response::Ok,                      // Send to the project
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
        assert!(html.contains("<b>Claude (Claude)</b>"), "{html}");
        assert!(
            html.contains("<option value=\"/p/keel\" selected>keel — /p/keel</option>")
                && html.contains("name=\"project_path\""),
            "the default project is proposed and any folder may be typed: {html}"
        );
        assert!(html.contains("name=\"pairing_code\""));
        assert!(html.contains(&format!("value=\"{challenge}\"")));
        assert!(
            matches!(&mock.requests()[..], [Request::List { all: true, .. }]),
            "the page asks the daemon which projects it knows"
        );
        mock.requests.lock().unwrap().clear();

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
        assert!(
            location.ends_with("&state=s1&iss=https%3A%2F%2Fx.trycloudflare.com"),
            "the response names its issuer (RFC 9207): {location}"
        );
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
        let broadcast = connector
            .handle(post(
                "/mcp",
                "application/json",
                r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"project","text":"to everyone in keel"}}}"#,
                Some(&access),
            ))
            .await;
        assert_eq!(broadcast.status, 200);
        assert!(
            matches!(
                mock.requests().last().unwrap(),
                Request::Send { to, .. } if to == "project:/p/keel"
            ),
            "a project broadcast names the served project, not this process's directory"
        );
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

    /// A vendor whose `client_id` is its metadata document's URL needs no
    /// registration: the document is fetched from the vendor's host once
    /// an hour, the page names that host, the code goes back with the
    /// issuer named, and the exchange works with the URL as client_id.
    /// Any other host is refused before a byte is fetched.
    #[tokio::test]
    async fn a_vendor_connects_through_its_metadata_document() {
        let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = fetches.clone();
        let fetcher: Arc<cimd::Fetcher> = Arc::new(move |url: &str| {
            counted.fetch_add(1, Ordering::Relaxed);
            match url {
                "https://chatgpt.com/oauth/client.json" => Ok(json!({
                    "client_id": url,
                    "client_name": "ChatGPT",
                    "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect"],
                    "token_endpoint_auth_method": "none",
                })),
                "https://claude.ai/oauth/elsewhere.json" => Ok(json!({
                    "client_id": "https://claude.ai/oauth/other.json",
                    "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"],
                })),
                _ => Err(OAuthError::new(
                    "invalid_client",
                    "not fetched in this test",
                )),
            }
        });
        let (connector, mock) = connector(vec![
            Response::Agents {
                agents: vec![],
                aliases: Default::default(),
            }, // List, first consent page
            Response::Agents {
                agents: vec![],
                aliases: Default::default(),
            }, // List, second consent page
            Response::error(ErrorCode::NotFound, "no such agent"), // Inspect: the typed name is free
            live_agent("chatgpt-browser-test"), // Register, at the token exchange
        ]);
        let connector = connector.with_fetcher(fetcher);
        let server = json_body(
            &connector
                .handle(get("/.well-known/oauth-authorization-server"))
                .await,
        );
        assert_eq!(server["client_id_metadata_document_supported"], true);
        assert_eq!(
            server["authorization_response_iss_parameter_supported"],
            true
        );

        // Not a vendor host: refused on the page, nothing fetched.
        let foreign = connector
            .handle(get(
                "/authorize?response_type=code&client_id=https%3A%2F%2Fevil.example%2Fclient.json&redirect_uri=https%3A%2F%2Fclaude.ai%2Fapi%2Fmcp%2Fauth_callback&code_challenge=cccccccccccccccccccccccccccccccccccccccccccccc&code_challenge_method=S256",
            ))
            .await;
        assert_eq!(foreign.status, 400);
        assert_eq!(fetches.load(Ordering::Relaxed), 0);
        // A vendor host whose document names another URL: refused.
        let mismatched = connector
            .handle(get(
                "/authorize?response_type=code&client_id=https%3A%2F%2Fclaude.ai%2Foauth%2Felsewhere.json&redirect_uri=https%3A%2F%2Fclaude.ai%2Fapi%2Fmcp%2Fauth_callback&code_challenge=cccccccccccccccccccccccccccccccccccccccccccccc&code_challenge_method=S256",
            ))
            .await;
        assert_eq!(mismatched.status, 400);
        assert!(
            String::from_utf8_lossy(&mismatched.body).contains("does not name its own URL"),
            "{}",
            String::from_utf8_lossy(&mismatched.body)
        );
        assert_eq!(fetches.load(Ordering::Relaxed), 1);

        let client_id = "https%3A%2F%2Fchatgpt.com%2Foauth%2Fclient.json";
        let verifier = "v".repeat(60);
        let challenge = oauth::s256(&verifier);
        let query = format!(
            "response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fchatgpt.com%2Fconnector_platform_oauth_redirect&code_challenge={challenge}&code_challenge_method=S256&state=s2"
        );
        let form = connector.handle(get(&format!("/authorize?{query}"))).await;
        assert_eq!(form.status, 200, "{}", String::from_utf8_lossy(&form.body));
        let html = String::from_utf8_lossy(&form.body);
        assert!(
            html.contains("<b>ChatGPT (chatgpt.com)</b>"),
            "the host, not the document's name: {html}"
        );
        assert_eq!(fetches.load(Ordering::Relaxed), 2);
        let again = connector.handle(get(&format!("/authorize?{query}"))).await;
        assert_eq!(again.status, 200);
        assert_eq!(
            fetches.load(Ordering::Relaxed),
            2,
            "a fresh document is not fetched again"
        );

        let consent = connector
            .handle(post(
                "/authorize",
                "application/x-www-form-urlencoded",
                &format!("{query}&pairing_code=ABCD-EFGH&agent_name=chatgpt-browser-test&project=%2Fp%2Fkeel"),
                None,
            ))
            .await;
        assert_eq!(
            consent.status,
            302,
            "{}",
            String::from_utf8_lossy(&consent.body)
        );
        let location = consent
            .headers
            .iter()
            .find(|(n, _)| n == "Location")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(
            location.starts_with("https://chatgpt.com/connector_platform_oauth_redirect?code=")
        );
        assert!(
            location.ends_with("&state=s2&iss=https%3A%2F%2Fx.trycloudflare.com"),
            "{location}"
        );
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
                    "grant_type=authorization_code&code={code}&client_id={client_id}&redirect_uri=https%3A%2F%2Fchatgpt.com%2Fconnector_platform_oauth_redirect&code_verifier={verifier}"
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
        assert!(
            matches!(
                mock.requests().last(),
                Some(Request::Register { spec, .. })
                    if spec.runtime == "chatgpt-browser"
                        && spec.workdir.as_deref() == Some(Path::new("/p/keel"))
            ),
            "{:?}",
            mock.requests().last()
        );
        let store = connector.store.lock().unwrap();
        let grant = store.grants.values().next().unwrap();
        assert_eq!(grant.client_id, "https://chatgpt.com/oauth/client.json");
        assert_eq!(grant.vendor, Vendor::ChatGpt);
    }

    /// One connector, every project: the consent page proposes the
    /// default and takes any folder on this machine, refuses one that is
    /// not here, and the browser agent is registered where the person
    /// said, with `project` broadcasts going there.
    #[tokio::test]
    async fn consent_chooses_the_project_the_browser_agent_joins() {
        let here = tempfile::tempdir().unwrap();
        let root = here.path().canonicalize().unwrap();
        let (connector, mock) = connector(vec![
            live_agent("claude-browser-here"), // Register
            live_agent("claude-browser-here"), // Inspect before send_message
            Response::Ok,                      // Heartbeat
            Response::Ok,                      // Send
        ]);
        let registered = json_body(
            &connector
                .handle(post(
                    "/register",
                    "application/json",
                    r#"{"redirect_uris":["https://claude.ai/api/mcp/auth_callback"]}"#,
                    None,
                ))
                .await,
        );
        let client_id = registered["client_id"].as_str().unwrap().to_owned();
        let verifier = "v".repeat(60);
        let consent = |project: &str| {
            post(
                "/authorize",
                "application/x-www-form-urlencoded",
                &format!(
                    "response_type=code&client_id={client_id}&code_challenge={}&code_challenge_method=S256&pairing_code=ABCD-EFGH&project=%2Fp%2Fkeel&project_path={}",
                    oauth::s256(&verifier),
                    http::percent_encode(project)
                ),
                None,
            )
        };
        let missing = connector.handle(consent("/no/such/folder/here")).await;
        assert_eq!(missing.status, 400);
        let html = String::from_utf8_lossy(&missing.body);
        assert!(
            html.contains("That folder is not on this machine."),
            "{html}"
        );
        assert!(
            html.contains("<option value=\"/p/keel\" selected>"),
            "the page comes back with its choices: {html}"
        );
        let relative = connector.handle(consent("keel")).await;
        assert_eq!(relative.status, 400);
        assert!(
            mock.requests().is_empty(),
            "a refused project asks the daemon nothing"
        );

        let chosen = connector.handle(consent(&root.to_string_lossy())).await;
        assert_eq!(
            chosen.status,
            302,
            "{}",
            String::from_utf8_lossy(&chosen.body)
        );
        let location = chosen
            .headers
            .iter()
            .find(|(n, _)| n == "Location")
            .map(|(_, v)| v.clone())
            .unwrap();
        let code = location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned();
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
        assert!(
            matches!(
                &mock.requests()[0],
                Request::Register { spec, .. } if spec.workdir.as_deref() == Some(root.as_path())
            ),
            "{:?}",
            mock.requests()[0]
        );
        let sent = connector
            .handle(post(
                "/mcp",
                "application/json",
                r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"project","text":"hello"}}}"#,
                Some(&access),
            ))
            .await;
        assert_eq!(sent.status, 200);
        assert!(
            matches!(
                mock.requests().last(),
                Some(Request::Send { to, .. }) if *to == format!("project:{}", root.display())
            ),
            "a project broadcast goes to the project the consent chose: {:?}",
            mock.requests().last()
        );
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
