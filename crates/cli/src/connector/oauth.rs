//! The connector's own authorization server, as small as OAuth 2.1 lets
//! it be: dynamic client registration that admits only the vendors'
//! callback URLs, an authorization code bound to a PKCE challenge and to
//! one browser-agent identity, short access tokens and rotating refresh
//! tokens. Pure: every step takes `now`, nothing here does I/O, and what
//! is kept at rest is a hash, never a token.

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::http::{field, percent_encode};

/// How long an authorization code may wait to be exchanged.
pub const CODE_LIFETIME: Duration = Duration::minutes(5);
/// How long an access token is honoured; the vendor refreshes on a 401.
pub const ACCESS_LIFETIME: Duration = Duration::hours(1);
/// The one scope there is.
pub const SCOPE: &str = "agentdocker";

/// Where a hosted surface sends the person back after consent. Only these
/// may be registered: a client that names any other callback is refused,
/// so no one else's OAuth client can complete the flow against this
/// server, whatever it knows.
pub const VENDOR_CALLBACKS: &[(&str, Vendor)] = &[
    ("https://claude.ai/api/mcp/auth_callback", Vendor::Claude),
    (
        "https://chatgpt.com/connector_platform_oauth_redirect",
        Vendor::ChatGpt,
    ),
];
/// ChatGPT also uses a per-connection callback under this prefix.
pub const CHATGPT_CALLBACK_PREFIX: &str = "https://chatgpt.com/connector/oauth/";
/// Where a vendor hosts its Client ID Metadata Document: a URL-formatted
/// `client_id` is fetched only from these hosts (or the host of a
/// callback the person allowed), never from an address a request names.
pub const VENDOR_METADATA_HOSTS: &[(&str, Vendor)] = &[
    ("claude.ai", Vendor::Claude),
    ("chatgpt.com", Vendor::ChatGpt),
];
/// A metadata document larger than this is not one; the vendors' are
/// a few hundred bytes.
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
/// How long a fetched metadata document stands before it is fetched
/// again at the next authorization.
pub const METADATA_LIFETIME: Duration = Duration::hours(1);

/// Whose hosted surface a client is, by the callback it registered: it
/// decides the runtime the browser agent is recorded as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Claude,
    ChatGpt,
    /// A callback the person allowed explicitly.
    Other,
}

impl Vendor {
    pub fn runtime(self) -> &'static str {
        match self {
            Self::Claude => "claude-browser",
            Self::ChatGpt => "chatgpt-browser",
            Self::Other => "browser",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::ChatGpt => "ChatGPT",
            Self::Other => "a browser agent",
        }
    }

    /// The vendor a callback belongs to, when it is one this server admits.
    pub fn of_callback(uri: &str, extra: &[String]) -> Option<Self> {
        if let Some((_, vendor)) = VENDOR_CALLBACKS.iter().find(|(known, _)| *known == uri) {
            return Some(*vendor);
        }
        if uri.starts_with(CHATGPT_CALLBACK_PREFIX) && uri.len() > CHATGPT_CALLBACK_PREFIX.len() {
            return Some(Self::ChatGpt);
        }
        extra
            .iter()
            .any(|allowed| allowed == uri)
            .then_some(Self::Other)
    }

    /// The vendor whose metadata document a host may serve, when it is
    /// one this server fetches from.
    pub fn of_metadata_host(host: &str, extra: &[String]) -> Option<Self> {
        if let Some((_, vendor)) = VENDOR_METADATA_HOSTS
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(host))
        {
            return Some(*vendor);
        }
        extra
            .iter()
            .filter_map(|allowed| host_of(allowed))
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
            .then_some(Self::Other)
    }
}

/// The host of an `https://` URL, without userinfo or port games: a
/// `client_id` is a plain URL or it is not one.
pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty() || host.contains(['@', ':', '[', ']']) {
        return None;
    }
    Some(host)
}

/// Whether `client_id` has the shape of a Client ID Metadata Document
/// URL (draft-ietf-oauth-client-id-metadata-document): `https`, a host,
/// a path, no fragment, and a length a document URL has.
pub fn is_metadata_client_id(client_id: &str) -> bool {
    client_id.len() <= 512
        && !client_id.contains('#')
        && host_of(client_id).is_some_and(|host| {
            client_id["https://".len() + host.len()..].starts_with('/')
                && client_id["https://".len() + host.len()..].len() > 1
        })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Client {
    pub redirect_uris: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub vendor: Vendor,
    pub created_at: DateTime<Utc>,
    /// Set for a client whose `client_id` is its metadata document's
    /// URL: when that document was last fetched and admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_fetched_at: Option<DateTime<Utc>>,
}

impl Client {
    /// The name the consent page shows. A metadata document is
    /// self-asserted, so its `client_name` is never the relying party:
    /// the host the document was fetched from is.
    pub fn shown_as(&self, client_id: &str) -> String {
        match (self.metadata_fetched_at, host_of(client_id)) {
            (Some(_), Some(host)) => format!("{} ({host})", self.vendor.label()),
            _ => match &self.name {
                Some(name) => format!("{} ({name})", self.vendor.label()),
                None => self.vendor.label().to_owned(),
            },
        }
    }
}

/// One consent: one browser-agent identity, and the refresh token that
/// keeps it reachable across access tokens.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub agent_id: String,
    pub agent_name: String,
    pub runtime: String,
    pub project: PathBuf,
    pub client_id: String,
    pub vendor: Vendor,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Grant {
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

#[derive(Clone, Debug)]
struct Code {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    expires_at: DateTime<Utc>,
    consent: Consent,
}

/// What the person decided on the consent page. The agent is created when
/// the code is redeemed, not here: a code that is never exchanged, or
/// exchanged with the wrong verifier, must leave no agent behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consent {
    pub vendor: Vendor,
    pub client_id: String,
    pub agent_name: String,
    /// The name was made up here rather than typed, so a collision may
    /// be retried with another.
    pub generated: bool,
    /// The project the agent joins: chosen on the consent page.
    pub project: PathBuf,
}

#[derive(Clone, Debug)]
struct Access {
    grant: String,
    expires_at: DateTime<Utc>,
}

/// Clients and grants persist; codes and access tokens are minutes or an
/// hour old at most and live in memory, so a restart costs one refresh.
#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub clients: BTreeMap<String, Client>,
    #[serde(default)]
    pub grants: BTreeMap<String, Grant>,
    #[serde(skip)]
    codes: BTreeMap<String, Code>,
    #[serde(skip)]
    access: BTreeMap<String, Access>,
    /// This server's issuer identifier, named in every authorization
    /// response (RFC 9207) so a client can tell which server answered.
    /// Set when the server starts; empty in a bare store.
    #[serde(skip)]
    pub issuer: String,
}

/// An OAuth error, with the code the vendor's client understands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthError {
    pub code: &'static str,
    pub description: String,
}

impl OAuthError {
    pub fn new(code: &'static str, description: impl Into<String>) -> Self {
        Self {
            code,
            description: description.into(),
        }
    }

    pub fn json(&self) -> Value {
        json!({ "error": self.code, "error_description": self.description })
    }
}

/// An authorization request that passed validation and waits for the
/// person's consent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub client_id: String,
    pub client_name: Option<String>,
    /// The client as the consent page names it.
    pub shown_as: String,
    pub vendor: Vendor,
    pub redirect_uri: String,
    pub state: Option<String>,
    pub code_challenge: String,
    /// The issuer to name in the response; empty names none.
    pub issuer: String,
}

impl Pending {
    /// Everything needed to resume this request after the consent form
    /// comes back, as query pairs the form carries in hidden fields.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![
            ("client_id", self.client_id.clone()),
            ("redirect_uri", self.redirect_uri.clone()),
            ("code_challenge", self.code_challenge.clone()),
            ("code_challenge_method", "S256".to_owned()),
            ("response_type", "code".to_owned()),
        ];
        if let Some(state) = &self.state {
            fields.push(("state", state.clone()));
        }
        fields
    }

    /// Where to send the person with an error the client can read.
    pub fn error_redirect(&self, error: &OAuthError) -> String {
        let mut url = format!(
            "{}{}error={}&error_description={}",
            self.redirect_uri,
            if self.redirect_uri.contains('?') {
                "&"
            } else {
                "?"
            },
            percent_encode(error.code),
            percent_encode(&error.description)
        );
        if let Some(state) = &self.state {
            url.push_str("&state=");
            url.push_str(&percent_encode(state));
        }
        if !self.issuer.is_empty() {
            url.push_str("&iss=");
            url.push_str(&percent_encode(&self.issuer));
        }
        url
    }
}

/// A validation failure before consent. `Page` cannot be sent back to the
/// client, because the client or its callback is what failed; `Redirect`
/// can.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorizeRefusal {
    Page(OAuthError),
    Redirect(Box<Pending>, OAuthError),
}

/// Who the consent made the connection into.
#[derive(Clone, Debug)]
pub struct AgentIdentity {
    pub id: String,
    pub name: String,
    pub runtime: String,
    pub project: PathBuf,
}

/// Tokens as handed to the client, once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issued {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

impl Issued {
    pub fn json(&self) -> Value {
        json!({
            "access_token": self.access_token,
            "token_type": "Bearer",
            "expires_in": self.expires_in,
            "refresh_token": self.refresh_token,
            "scope": SCOPE,
        })
    }
}

/// 32 bytes from the process's random source, URL-safe. Two v4 UUIDs
/// carry 244 random bits between them, which is what the OS gave `uuid`;
/// no separate generator to audit.
pub fn random_token() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Eight characters the person types from the terminal into the consent
/// page, from an alphabet without look-alikes.
pub fn pairing_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = uuid::Uuid::new_v4();
    let code: String = bytes
        .as_bytes()
        .iter()
        .take(8)
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect();
    format!("{}-{}", &code[..4], &code[4..])
}

/// Codes compare without case, spaces or the dash, so typing is forgiving.
pub fn pairing_code_matches(expected: &str, typed: &str) -> bool {
    let normal = |s: &str| {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect::<String>()
    };
    normal(expected) == normal(typed)
}

pub fn hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `base64url(sha256(verifier))`, the S256 transformation of RFC 7636.
pub fn s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

impl Store {
    /// RFC 7591 registration. The callbacks decide: a client whose
    /// redirect URIs are not all the vendors' (or the person's explicit
    /// extras) is refused, and so is any request for a confidential
    /// client or a grant this server does not issue.
    pub fn register_client(
        &mut self,
        body: &Value,
        extra_callbacks: &[String],
        now: DateTime<Utc>,
    ) -> Result<Value, OAuthError> {
        let uris: Vec<String> = body["redirect_uris"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if uris.is_empty() {
            return Err(OAuthError::new(
                "invalid_redirect_uri",
                "redirect_uris must name at least one callback",
            ));
        }
        let mut vendor = None;
        for uri in &uris {
            let Some(found) = Vendor::of_callback(uri, extra_callbacks) else {
                return Err(OAuthError::new(
                    "invalid_redirect_uri",
                    format!(
                        "{uri} is not a callback this connector admits; only the vendors' hosted surfaces can connect"
                    ),
                ));
            };
            vendor.get_or_insert(found);
        }
        if let Some(method) = body["token_endpoint_auth_method"].as_str()
            && method != "none"
        {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "only public clients (token_endpoint_auth_method \"none\") are issued",
            ));
        }
        if let Some(grants) = body["grant_types"].as_array()
            && grants
                .iter()
                .any(|g| !matches!(g.as_str(), Some("authorization_code" | "refresh_token")))
        {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "only authorization_code and refresh_token grants are issued",
            ));
        }
        if let Some(types) = body["response_types"].as_array()
            && types.iter().any(|t| t.as_str() != Some("code"))
        {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "only the code response type is issued",
            ));
        }
        let client_id = random_token();
        let name = body["client_name"]
            .as_str()
            .map(|n| n.chars().take(80).collect::<String>())
            .filter(|n| !n.trim().is_empty());
        self.clients.insert(
            client_id.clone(),
            Client {
                redirect_uris: uris.clone(),
                name: name.clone(),
                vendor: vendor.unwrap_or(Vendor::Other),
                created_at: now,
                metadata_fetched_at: None,
            },
        );
        Ok(json!({
            "client_id": client_id,
            "client_id_issued_at": now.timestamp(),
            "redirect_uris": uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "client_name": name,
            "scope": SCOPE,
        }))
    }

    /// Whether a metadata client's document was admitted recently enough
    /// to serve another authorization without fetching it again.
    pub fn metadata_client_fresh(&self, client_id: &str, now: DateTime<Utc>) -> bool {
        self.clients
            .get(client_id)
            .and_then(|c| c.metadata_fetched_at)
            .is_some_and(|fetched| now - fetched < METADATA_LIFETIME)
    }

    /// A Client ID Metadata Document, fetched from `client_id` by the
    /// caller, becomes (or refreshes) the client it describes. The same
    /// rules as registration, plus the document's: it names itself
    /// exactly, it comes from a vendor's host, and its callbacks are that
    /// vendor's. What it says about itself otherwise is not trusted: the
    /// consent page shows the host, and the client is public whatever
    /// authentication method it prefers, since this server issues no
    /// secrets and accepts none.
    pub fn admit_metadata_client(
        &mut self,
        client_id: &str,
        document: &Value,
        extra_callbacks: &[String],
        now: DateTime<Utc>,
    ) -> Result<&Client, OAuthError> {
        if !is_metadata_client_id(client_id) {
            return Err(OAuthError::new(
                "invalid_client",
                "client_id is not a metadata document URL",
            ));
        }
        let host = host_of(client_id).unwrap_or_default();
        let Some(host_vendor) = Vendor::of_metadata_host(host, extra_callbacks) else {
            return Err(OAuthError::new(
                "invalid_client",
                format!(
                    "{host} is not a host this connector fetches client metadata from; only the vendors' hosted surfaces can connect"
                ),
            ));
        };
        if !document.is_object() {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "the metadata document is not a JSON object",
            ));
        }
        if document["client_id"].as_str() != Some(client_id) {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "the metadata document does not name its own URL as client_id",
            ));
        }
        let uris: Vec<String> = document["redirect_uris"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if uris.is_empty() {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "the metadata document names no redirect_uris",
            ));
        }
        for uri in &uris {
            match Vendor::of_callback(uri, extra_callbacks) {
                Some(vendor) if vendor == host_vendor => {}
                Some(_) => {
                    return Err(OAuthError::new(
                        "invalid_client_metadata",
                        format!("{uri} is not a callback of the vendor at {host}"),
                    ));
                }
                None => {
                    return Err(OAuthError::new(
                        "invalid_client_metadata",
                        format!(
                            "{uri} is not a callback this connector admits; only the vendors' hosted surfaces can connect"
                        ),
                    ));
                }
            }
        }
        if let Some(grants) = document["grant_types"].as_array()
            && !grants
                .iter()
                .any(|g| g.as_str() == Some("authorization_code"))
        {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "the metadata document does not use the authorization_code grant",
            ));
        }
        if let Some(types) = document["response_types"].as_array()
            && !types.iter().any(|t| t.as_str() == Some("code"))
        {
            return Err(OAuthError::new(
                "invalid_client_metadata",
                "the metadata document does not use the code response type",
            ));
        }
        let name = document["client_name"]
            .as_str()
            .map(|n| n.chars().take(80).collect::<String>())
            .filter(|n| !n.trim().is_empty());
        let created_at = self
            .clients
            .get(client_id)
            .map(|c| c.created_at)
            .unwrap_or(now);
        self.clients.insert(
            client_id.to_owned(),
            Client {
                redirect_uris: uris,
                name,
                vendor: host_vendor,
                created_at,
                metadata_fetched_at: Some(now),
            },
        );
        Ok(&self.clients[client_id])
    }

    /// Validate an authorization request. The client and its callback are
    /// checked before anything is sent back to that callback.
    pub fn begin_authorization(
        &self,
        params: &[(String, String)],
    ) -> Result<Pending, AuthorizeRefusal> {
        let Some(client_id) = field(params, "client_id") else {
            return Err(AuthorizeRefusal::Page(OAuthError::new(
                "invalid_request",
                "client_id is missing",
            )));
        };
        let Some(client) = self.clients.get(client_id) else {
            return Err(AuthorizeRefusal::Page(OAuthError::new(
                "invalid_client",
                "unknown client; register first",
            )));
        };
        let redirect_uri = match field(params, "redirect_uri") {
            Some(uri) if client.redirect_uris.iter().any(|r| r == uri) => uri.to_owned(),
            Some(_) => {
                return Err(AuthorizeRefusal::Page(OAuthError::new(
                    "invalid_request",
                    "redirect_uri is not one this client registered",
                )));
            }
            None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
            None => {
                return Err(AuthorizeRefusal::Page(OAuthError::new(
                    "invalid_request",
                    "redirect_uri is required for a client with several",
                )));
            }
        };
        let pending = Pending {
            client_id: client_id.to_owned(),
            client_name: client.name.clone(),
            shown_as: client.shown_as(client_id),
            vendor: client.vendor,
            redirect_uri,
            state: field(params, "state").map(str::to_owned),
            code_challenge: String::new(),
            issuer: self.issuer.clone(),
        };
        let refuse = |code, why: &str| {
            AuthorizeRefusal::Redirect(Box::new(pending.clone()), OAuthError::new(code, why))
        };
        if field(params, "response_type") != Some("code") {
            return Err(refuse(
                "unsupported_response_type",
                "only response_type=code is issued",
            ));
        }
        if field(params, "code_challenge_method").is_some_and(|m| m != "S256") {
            return Err(refuse(
                "invalid_request",
                "code_challenge_method must be S256",
            ));
        }
        let Some(challenge) = field(params, "code_challenge") else {
            return Err(refuse("invalid_request", "code_challenge is required"));
        };
        if challenge.len() < 43
            || challenge.len() > 128
            || !challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(refuse("invalid_request", "code_challenge is malformed"));
        }
        if let Some(scope) = field(params, "scope")
            && scope.split_whitespace().any(|s| s != SCOPE)
        {
            return Err(refuse(
                "invalid_scope",
                &format!("the only scope is {SCOPE}"),
            ));
        }
        Ok(Pending {
            code_challenge: challenge.to_owned(),
            ..pending
        })
    }

    /// The person consented: the redirect carries a one-time code that
    /// stands for this consent until it is redeemed.
    pub fn complete_authorization(
        &mut self,
        pending: &Pending,
        agent_name: String,
        generated: bool,
        project: PathBuf,
        now: DateTime<Utc>,
    ) -> String {
        let code = random_token();
        self.codes.insert(
            hash(&code),
            Code {
                client_id: pending.client_id.clone(),
                redirect_uri: pending.redirect_uri.clone(),
                code_challenge: pending.code_challenge.clone(),
                expires_at: now + CODE_LIFETIME,
                consent: Consent {
                    vendor: pending.vendor,
                    client_id: pending.client_id.clone(),
                    agent_name,
                    generated,
                    project,
                },
            },
        );
        let mut url = format!(
            "{}{}code={}",
            pending.redirect_uri,
            if pending.redirect_uri.contains('?') {
                "&"
            } else {
                "?"
            },
            percent_encode(&code)
        );
        if let Some(state) = &pending.state {
            url.push_str("&state=");
            url.push_str(&percent_encode(state));
        }
        if !pending.issuer.is_empty() {
            url.push_str("&iss=");
            url.push_str(&percent_encode(&pending.issuer));
        }
        url
    }

    /// Redeem an authorization code: single use, bound to its client, its
    /// callback and its PKCE verifier. What comes back is the consent the
    /// caller now turns into an agent before asking for tokens.
    pub fn redeem_code(
        &mut self,
        form: &[(String, String)],
        now: DateTime<Utc>,
    ) -> Result<Consent, OAuthError> {
        self.prune(now);
        let Some(code) = field(form, "code") else {
            return Err(OAuthError::new("invalid_request", "code is missing"));
        };
        let Some(found) = self.codes.remove(&hash(code)) else {
            return Err(OAuthError::new(
                "invalid_grant",
                "unknown, used or expired code",
            ));
        };
        if field(form, "client_id").is_some_and(|id| id != found.client_id) {
            return Err(OAuthError::new(
                "invalid_grant",
                "code belongs to another client",
            ));
        }
        if field(form, "redirect_uri").is_some_and(|uri| uri != found.redirect_uri) {
            return Err(OAuthError::new(
                "invalid_grant",
                "redirect_uri does not match",
            ));
        }
        let Some(verifier) = field(form, "code_verifier") else {
            return Err(OAuthError::new(
                "invalid_request",
                "code_verifier is required",
            ));
        };
        if verifier.len() < 43 || verifier.len() > 128 || s256(verifier) != found.code_challenge {
            return Err(OAuthError::new(
                "invalid_grant",
                "code_verifier does not match",
            ));
        }
        Ok(found.consent)
    }

    /// The agent exists: this consent is now a grant, with its first tokens.
    pub fn issue(&mut self, consent: &Consent, agent: AgentIdentity, now: DateTime<Utc>) -> Issued {
        let grant_id = random_token();
        self.grants.insert(
            grant_id.clone(),
            Grant {
                agent_id: agent.id,
                agent_name: agent.name,
                runtime: agent.runtime,
                project: agent.project,
                client_id: consent.client_id.clone(),
                vendor: consent.vendor,
                created_at: now,
                refresh_hash: None,
                last_used_at: None,
                revoked_at: None,
            },
        );
        self.tokens_for(&grant_id, now)
            .expect("a grant just inserted is active")
    }

    /// The refresh grant: single use, and a token that has already been
    /// rotated is `invalid_grant`, which is what tells the vendor to
    /// reconnect.
    pub fn refresh(
        &mut self,
        form: &[(String, String)],
        now: DateTime<Utc>,
    ) -> Result<Issued, OAuthError> {
        self.prune(now);
        let Some(token) = field(form, "refresh_token") else {
            return Err(OAuthError::new(
                "invalid_request",
                "refresh_token is missing",
            ));
        };
        let wanted = hash(token);
        let Some((id, grant)) = self
            .grants
            .iter()
            .find(|(_, g)| g.refresh_hash.as_deref() == Some(wanted.as_str()))
        else {
            return Err(OAuthError::new(
                "invalid_grant",
                "unknown, rotated or revoked refresh token",
            ));
        };
        if field(form, "client_id").is_some_and(|c| c != grant.client_id) {
            return Err(OAuthError::new(
                "invalid_grant",
                "refresh token belongs to another client",
            ));
        }
        let id = id.clone();
        self.tokens_for(&id, now)
    }

    /// New tokens for an active grant; the refresh token rotates.
    fn tokens_for(&mut self, grant_id: &str, now: DateTime<Utc>) -> Result<Issued, OAuthError> {
        let Some(grant) = self.grants.get_mut(grant_id) else {
            return Err(OAuthError::new("invalid_grant", "the grant is gone"));
        };
        if !grant.active() {
            return Err(OAuthError::new("invalid_grant", "the grant was revoked"));
        }
        let access_token = random_token();
        let refresh_token = random_token();
        grant.refresh_hash = Some(hash(&refresh_token));
        grant.last_used_at = Some(now);
        self.access.insert(
            hash(&access_token),
            Access {
                grant: grant_id.to_owned(),
                expires_at: now + ACCESS_LIFETIME,
            },
        );
        Ok(Issued {
            access_token,
            refresh_token,
            expires_in: ACCESS_LIFETIME.num_seconds(),
        })
    }

    /// The grant behind a bearer token, while the token and the grant hold.
    pub fn authenticate(&mut self, bearer: &str, now: DateTime<Utc>) -> Option<(String, Grant)> {
        let wanted = hash(bearer);
        let access = self.access.get(&wanted)?;
        if access.expires_at <= now {
            self.access.remove(&wanted);
            return None;
        }
        let id = access.grant.clone();
        let grant = self.grants.get_mut(&id)?;
        if !grant.active() {
            return None;
        }
        grant.last_used_at = Some(now);
        Some((id, grant.clone()))
    }

    /// End a grant: its tokens stop working at once.
    pub fn revoke(&mut self, grant_id: &str, now: DateTime<Utc>) -> Option<Grant> {
        let grant = self.grants.get_mut(grant_id)?;
        grant.revoked_at.get_or_insert(now);
        grant.refresh_hash = None;
        self.access.retain(|_, a| a.grant != grant_id);
        Some(grant.clone())
    }

    /// The grant for an agent, by its id or name.
    pub fn grant_for_agent(&self, reference: &str) -> Option<(String, &Grant)> {
        self.grants
            .iter()
            .find(|(id, g)| {
                g.active()
                    && (g.agent_id == reference
                        || g.agent_name == reference
                        || id.as_str() == reference)
            })
            .map(|(id, g)| (id.clone(), g))
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        self.codes.retain(|_, c| c.expires_at > now);
        self.access.retain(|_, a| a.expires_at > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::http::parse_query;

    fn pairs(query: &str) -> Vec<(String, String)> {
        parse_query(query)
    }

    fn now() -> DateTime<Utc> {
        "2026-09-17T22:00:00Z".parse().unwrap()
    }

    fn registered(store: &mut Store, callback: &str) -> String {
        let reply = store
            .register_client(
                &json!({"redirect_uris": [callback], "client_name": "Claude"}),
                &[],
                now(),
            )
            .unwrap();
        reply["client_id"].as_str().unwrap().to_owned()
    }

    fn agent() -> AgentIdentity {
        AgentIdentity {
            id: "a1".into(),
            name: "claude-browser-ab12".into(),
            runtime: "claude-browser".into(),
            project: "/p/keel".into(),
        }
    }

    #[test]
    fn registration_admits_the_vendors_callbacks_and_nothing_else() {
        let mut store = Store::default();
        let reply = store
            .register_client(
                &json!({
                    "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"],
                    "client_name": "Claude",
                    "token_endpoint_auth_method": "none",
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"],
                }),
                &[],
                now(),
            )
            .unwrap();
        assert_eq!(reply["token_endpoint_auth_method"], "none");
        let id = reply["client_id"].as_str().unwrap();
        assert_eq!(store.clients[id].vendor, Vendor::Claude);
        let chatgpt = store
            .register_client(
                &json!({"redirect_uris": ["https://chatgpt.com/connector/oauth/abc123"]}),
                &[],
                now(),
            )
            .unwrap();
        assert_eq!(
            store.clients[chatgpt["client_id"].as_str().unwrap()].vendor,
            Vendor::ChatGpt
        );
        for (body, code) in [
            (json!({"redirect_uris": []}), "invalid_redirect_uri"),
            (
                json!({"redirect_uris": ["https://evil.example/cb"]}),
                "invalid_redirect_uri",
            ),
            (
                json!({"redirect_uris": ["https://chatgpt.com/connector/oauth/"]}),
                "invalid_redirect_uri",
            ),
            (
                json!({"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"], "token_endpoint_auth_method": "client_secret_post"}),
                "invalid_client_metadata",
            ),
            (
                json!({"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"], "grant_types": ["client_credentials"]}),
                "invalid_client_metadata",
            ),
        ] {
            assert_eq!(
                store.register_client(&body, &[], now()).unwrap_err().code,
                code,
                "{body}"
            );
        }
        let extra = vec!["https://mine.example/cb".to_owned()];
        let mine = store
            .register_client(
                &json!({"redirect_uris": ["https://mine.example/cb"]}),
                &extra,
                now(),
            )
            .unwrap();
        assert_eq!(
            store.clients[mine["client_id"].as_str().unwrap()].vendor,
            Vendor::Other
        );
    }

    /// Redeem a code and, as the connector does once the daemon has the
    /// agent, turn the consent into a grant with tokens.
    fn redeem(store: &mut Store, form: &str, at: DateTime<Utc>) -> Result<Issued, OAuthError> {
        let consent = store.redeem_code(&pairs(form), at)?;
        Ok(store.issue(&consent, agent(), at))
    }

    fn code_of(redirect: &str) -> String {
        redirect
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn the_code_flow_binds_a_pkce_verifier_and_a_code_is_used_once() {
        let mut store = Store::default();
        let client = registered(&mut store, "https://claude.ai/api/mcp/auth_callback");
        let verifier = "v".repeat(50);
        let challenge = s256(&verifier);
        let pending = store
            .begin_authorization(&pairs(&format!(
                "response_type=code&client_id={client}&redirect_uri=https%3A%2F%2Fclaude.ai%2Fapi%2Fmcp%2Fauth_callback&code_challenge={challenge}&code_challenge_method=S256&state=xyz&scope=agentdocker"
            )))
            .unwrap();
        assert_eq!(pending.vendor, Vendor::Claude);
        assert_eq!(pending.state.as_deref(), Some("xyz"));
        let redirect = store.complete_authorization(
            &pending,
            "claude-browser-ab12".into(),
            false,
            "/p/keel".into(),
            now(),
        );
        assert!(redirect.starts_with("https://claude.ai/api/mcp/auth_callback?code="));
        assert!(redirect.ends_with("&state=xyz"));
        assert!(store.grants.is_empty(), "consent alone makes no grant");
        let code = code_of(&redirect);

        let wrong = store.redeem_code(
            &pairs(&format!(
                "grant_type=authorization_code&code={code}&client_id={client}&code_verifier={}",
                "w".repeat(50)
            )),
            now(),
        );
        assert_eq!(wrong.unwrap_err().code, "invalid_grant");
        // A failed exchange consumes the code: no second guess, and no agent.
        let again = store.redeem_code(
            &pairs(&format!(
                "grant_type=authorization_code&code={code}&client_id={client}&code_verifier={verifier}"
            )),
            now(),
        );
        assert_eq!(again.unwrap_err().code, "invalid_grant");
        assert!(store.grants.is_empty());

        let redirect = store.complete_authorization(
            &pending,
            "claude-browser-ab12".into(),
            false,
            "/p/keel".into(),
            now(),
        );
        let code = code_of(&redirect);
        let consent = store
            .redeem_code(
                &pairs(&format!(
                    "grant_type=authorization_code&code={code}&client_id={client}&redirect_uri=https%3A%2F%2Fclaude.ai%2Fapi%2Fmcp%2Fauth_callback&code_verifier={verifier}"
                )),
                now(),
            )
            .unwrap();
        assert_eq!(
            consent,
            Consent {
                vendor: Vendor::Claude,
                client_id: client.clone(),
                agent_name: "claude-browser-ab12".into(),
                generated: false,
                project: "/p/keel".into(),
            }
        );
        let issued = store.issue(&consent, agent(), now());
        assert_eq!(issued.expires_in, 3600);
        let (id, grant) = store.authenticate(&issued.access_token, now()).unwrap();
        assert_eq!(grant.agent_name, "claude-browser-ab12");
        assert_eq!(store.grant_for_agent("a1").map(|(g, _)| g), Some(id));
        assert!(
            store
                .authenticate(&issued.access_token, now() + ACCESS_LIFETIME)
                .is_none(),
            "access expires"
        );
        assert!(store.authenticate("not-a-token", now()).is_none());
        // An expired code is gone too.
        let redirect = store.complete_authorization(
            &pending,
            "claude-browser-ab12".into(),
            false,
            "/p/keel".into(),
            now(),
        );
        let code = code_of(&redirect);
        let late = store.redeem_code(
            &pairs(&format!(
                "grant_type=authorization_code&code={code}&code_verifier={verifier}"
            )),
            now() + CODE_LIFETIME,
        );
        assert_eq!(late.unwrap_err().code, "invalid_grant");
    }

    #[test]
    fn refresh_rotates_and_revocation_ends_every_token() {
        let mut store = Store::default();
        let client = registered(&mut store, "https://claude.ai/api/mcp/auth_callback");
        let verifier = "v".repeat(50);
        let pending = store
            .begin_authorization(&pairs(&format!(
                "response_type=code&client_id={client}&code_challenge={}&code_challenge_method=S256",
                s256(&verifier)
            )))
            .unwrap();
        let redirect = store.complete_authorization(
            &pending,
            "claude-browser-ab12".into(),
            true,
            "/p/keel".into(),
            now(),
        );
        let code = code_of(&redirect);
        let first = redeem(
            &mut store,
            &format!("grant_type=authorization_code&code={code}&code_verifier={verifier}"),
            now(),
        )
        .unwrap();
        let (grant_id, _) = store.grant_for_agent("claude-browser-ab12").unwrap();
        let second = store
            .refresh(
                &pairs(&format!(
                    "grant_type=refresh_token&refresh_token={}&client_id={client}",
                    first.refresh_token
                )),
                now() + Duration::minutes(30),
            )
            .unwrap();
        assert_ne!(second.refresh_token, first.refresh_token);
        let replay = store.refresh(
            &pairs(&format!(
                "grant_type=refresh_token&refresh_token={}",
                first.refresh_token
            )),
            now(),
        );
        assert_eq!(replay.unwrap_err().code, "invalid_grant", "rotated");
        assert!(store.authenticate(&first.access_token, now()).is_some());
        assert!(store.authenticate(&second.access_token, now()).is_some());

        let revoked = store.revoke(&grant_id, now()).unwrap();
        assert!(revoked.revoked_at.is_some());
        assert!(store.authenticate(&first.access_token, now()).is_none());
        assert!(store.authenticate(&second.access_token, now()).is_none());
        let after = store.refresh(
            &pairs(&format!(
                "grant_type=refresh_token&refresh_token={}",
                second.refresh_token
            )),
            now(),
        );
        assert_eq!(after.unwrap_err().code, "invalid_grant");
        assert!(store.grant_for_agent("claude-browser-ab12").is_none());
        assert_eq!(
            store
                .refresh(&pairs("grant_type=refresh_token"), now())
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }

    #[test]
    fn authorization_refusals_redirect_only_when_the_callback_is_trusted() {
        let mut store = Store::default();
        let client = registered(&mut store, "https://claude.ai/api/mcp/auth_callback");
        assert!(matches!(
            store.begin_authorization(&pairs("response_type=code")),
            Err(AuthorizeRefusal::Page(e)) if e.code == "invalid_request"
        ));
        assert!(matches!(
            store.begin_authorization(&pairs("client_id=nobody&response_type=code")),
            Err(AuthorizeRefusal::Page(e)) if e.code == "invalid_client"
        ));
        assert!(matches!(
            store.begin_authorization(&pairs(&format!(
                "client_id={client}&redirect_uri=https%3A%2F%2Fevil.example%2Fcb&response_type=code"
            ))),
            Err(AuthorizeRefusal::Page(e)) if e.code == "invalid_request"
        ));
        let Err(AuthorizeRefusal::Redirect(pending, error)) = store.begin_authorization(&pairs(
            &format!("client_id={client}&response_type=token&state=s1"),
        )) else {
            panic!("a bad response type goes back to the trusted callback");
        };
        assert_eq!(error.code, "unsupported_response_type");
        let url = pending.error_redirect(&error);
        assert!(url.starts_with(
            "https://claude.ai/api/mcp/auth_callback?error=unsupported_response_type&error_description="
        ));
        assert!(url.ends_with("&state=s1"));
        for query in [
            format!("client_id={client}&response_type=code"),
            format!(
                "client_id={client}&response_type=code&code_challenge={}&code_challenge_method=plain",
                "a".repeat(43)
            ),
            format!("client_id={client}&response_type=code&code_challenge=short"),
            format!(
                "client_id={client}&response_type=code&code_challenge={}&scope=admin",
                "a".repeat(43)
            ),
        ] {
            assert!(
                matches!(
                    store.begin_authorization(&pairs(&query)),
                    Err(AuthorizeRefusal::Redirect(..))
                ),
                "{query}"
            );
        }
        let fields = store
            .begin_authorization(&pairs(&format!(
                "client_id={client}&response_type=code&code_challenge={}&state=s2",
                "a".repeat(43)
            )))
            .unwrap()
            .fields();
        assert!(fields.contains(&("state", "s2".to_owned())));
        assert!(fields.contains(&("code_challenge_method", "S256".to_owned())));
    }

    #[test]
    fn the_store_persists_clients_and_grants_as_hashes_only() {
        let mut store = Store::default();
        let client = registered(&mut store, "https://claude.ai/api/mcp/auth_callback");
        let verifier = "v".repeat(50);
        let pending = store
            .begin_authorization(&pairs(&format!(
                "client_id={client}&response_type=code&code_challenge={}",
                s256(&verifier)
            )))
            .unwrap();
        let redirect = store.complete_authorization(
            &pending,
            "claude-browser-ab12".into(),
            true,
            "/p/keel".into(),
            now(),
        );
        let code = code_of(&redirect);
        let issued = redeem(
            &mut store,
            &format!("grant_type=authorization_code&code={code}&code_verifier={verifier}"),
            now(),
        )
        .unwrap();
        let text = serde_json::to_string(&store).unwrap();
        assert!(!text.contains(&issued.refresh_token) && !text.contains(&issued.access_token));
        assert!(text.contains(&hash(&issued.refresh_token)));
        let mut reloaded: Store = serde_json::from_str(&text).unwrap();
        assert_eq!(reloaded.clients, store.clients);
        assert_eq!(reloaded.grants, store.grants);
        assert!(
            reloaded.authenticate(&issued.access_token, now()).is_none(),
            "access tokens do not survive a restart"
        );
        let renewed = reloaded
            .refresh(
                &pairs(&format!(
                    "grant_type=refresh_token&refresh_token={}",
                    issued.refresh_token
                )),
                now(),
            )
            .unwrap();
        assert!(
            reloaded
                .authenticate(&renewed.access_token, now())
                .is_some()
        );
    }

    #[test]
    fn pairing_codes_are_readable_and_forgiving() {
        let code = pairing_code();
        assert_eq!(code.len(), 9);
        assert!(
            code.chars()
                .all(|c| c == '-' || "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(c))
        );
        assert!(pairing_code_matches("ABCD-EFGH", " abcd efgh "));
        assert!(!pairing_code_matches("ABCD-EFGH", "ABCD-EFGJ"));
        assert_eq!(random_token().len(), 43);
        assert_ne!(random_token(), random_token());
        assert_eq!(
            s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "the RFC 7636 appendix B vector"
        );
    }

    /// A vendor's metadata document stands in for registration: it is
    /// admitted from the vendor's host with the vendor's callbacks and
    /// nothing else, the consent page names the host rather than the
    /// document's own `client_name`, and it goes stale after an hour.
    #[test]
    fn a_metadata_document_is_admitted_from_a_vendor_host_with_its_own_callbacks() {
        assert!(is_metadata_client_id(
            "https://chatgpt.com/oauth/client.json"
        ));
        assert!(is_metadata_client_id(
            "https://claude.ai/oauth/client-metadata"
        ));
        for bad in [
            "https://chatgpt.com",
            "https://chatgpt.com/",
            "http://chatgpt.com/oauth/client.json",
            "https://chatgpt.com/oauth/client.json#x",
            "https://user@chatgpt.com/oauth/client.json",
            "https://chatgpt.com:443/oauth/client.json",
            "opaque-client-id",
        ] {
            assert!(!is_metadata_client_id(bad), "{bad}");
        }
        let mut store = Store::default();
        let url = "https://chatgpt.com/oauth/client.json";
        let document = json!({
            "client_id": url,
            "client_name": "ChatGPT, says the document",
            "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect"],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "private_key_jwt",
        });
        let admitted = store
            .admit_metadata_client(url, &document, &[], now())
            .unwrap();
        assert_eq!(admitted.vendor, Vendor::ChatGpt);
        assert_eq!(admitted.metadata_fetched_at, Some(now()));
        assert_eq!(
            admitted.shown_as(url),
            "ChatGPT (chatgpt.com)",
            "the host, never the self-asserted name"
        );
        assert!(store.metadata_client_fresh(url, now() + Duration::minutes(59)));
        assert!(!store.metadata_client_fresh(url, now() + Duration::minutes(61)));
        assert!(!store.metadata_client_fresh("https://chatgpt.com/other.json", now()));
        // A refresh keeps the first admission's date and renews the fetch.
        let later = now() + Duration::hours(2);
        let refreshed = store
            .admit_metadata_client(url, &document, &[], later)
            .unwrap();
        assert_eq!(refreshed.created_at, now());
        assert_eq!(refreshed.metadata_fetched_at, Some(later));

        // Refusals: the wrong host, a document that names another URL, a
        // callback of another vendor, no callbacks, a foreign callback.
        let refused = |store: &mut Store, url: &str, document: Value| {
            store
                .admit_metadata_client(url, &document, &[], now())
                .map(|_| ())
                .unwrap_err()
        };
        assert_eq!(
            refused(
                &mut store,
                "https://evil.example/client.json",
                json!({"client_id": "https://evil.example/client.json", "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"]})
            )
            .code,
            "invalid_client"
        );
        assert_eq!(
            refused(
                &mut store,
                "https://claude.ai/oauth/x.json",
                json!({"client_id": "https://claude.ai/oauth/y.json", "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"]})
            )
            .code,
            "invalid_client_metadata"
        );
        assert_eq!(
            refused(
                &mut store,
                "https://claude.ai/oauth/x.json",
                json!({"client_id": "https://claude.ai/oauth/x.json", "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect"]})
            )
            .description,
            "https://chatgpt.com/connector_platform_oauth_redirect is not a callback of the vendor at claude.ai"
        );
        assert_eq!(
            refused(
                &mut store,
                "https://claude.ai/oauth/x.json",
                json!({"client_id": "https://claude.ai/oauth/x.json", "redirect_uris": []})
            )
            .code,
            "invalid_client_metadata"
        );
        assert_eq!(
            refused(
                &mut store,
                "https://claude.ai/oauth/x.json",
                json!({"client_id": "https://claude.ai/oauth/x.json", "redirect_uris": ["https://evil.example/cb"]})
            )
            .code,
            "invalid_client_metadata"
        );
        assert_eq!(
            refused(
                &mut store,
                "https://claude.ai/oauth/x.json",
                json!("not an object")
            )
            .code,
            "invalid_client_metadata"
        );
        assert_eq!(
            refused(&mut store, "opaque", json!({})).code,
            "invalid_client"
        );
        // A callback the person allowed brings its host along.
        let extra = vec!["https://agents.example/oauth/cb".to_owned()];
        let other = store
            .admit_metadata_client(
                "https://agents.example/client.json",
                &json!({"client_id": "https://agents.example/client.json", "redirect_uris": ["https://agents.example/oauth/cb"]}),
                &extra,
                now(),
            )
            .unwrap();
        assert_eq!(other.vendor, Vendor::Other);

        // The admitted document authorizes like a registered client, and
        // the code goes back with the issuer named (RFC 9207).
        store.issuer = "https://node.example.ts.net".into();
        let verifier = "v".repeat(50);
        let pending = store
            .begin_authorization(&pairs(&format!(
                "response_type=code&client_id={}&redirect_uri=https%3A%2F%2Fchatgpt.com%2Fconnector_platform_oauth_redirect&code_challenge={}&code_challenge_method=S256&state=s",
                percent_encode(url),
                s256(&verifier)
            )))
            .unwrap();
        assert_eq!(pending.shown_as, "ChatGPT (chatgpt.com)");
        assert_eq!(pending.issuer, "https://node.example.ts.net");
        let redirect = store.complete_authorization(
            &pending,
            "chatgpt-browser-ab12".into(),
            true,
            "/p/keel".into(),
            now(),
        );
        assert!(
            redirect.ends_with("&state=s&iss=https%3A%2F%2Fnode.example.ts.net"),
            "{redirect}"
        );
        let error = pending.error_redirect(&OAuthError::new("access_denied", "no"));
        assert!(
            error.ends_with("&state=s&iss=https%3A%2F%2Fnode.example.ts.net"),
            "{error}"
        );
        let code = code_of(&redirect);
        let consent = redeem_consent(
            &mut store,
            &format!(
                "grant_type=authorization_code&code={code}&client_id={}&code_verifier={verifier}",
                percent_encode(url)
            ),
        );
        assert_eq!(consent.client_id, url);
        assert_eq!(consent.project, PathBuf::from("/p/keel"));
    }

    fn redeem_consent(store: &mut Store, form: &str) -> Consent {
        store.redeem_code(&pairs(form), now()).unwrap()
    }
}
