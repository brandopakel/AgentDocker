//! The local OTLP/HTTP receiver for Claude Code's telemetry.
//!
//! Off unless `[usage] otlp_listen` names a loopback address. Then the
//! daemon takes `POST /v1/logs` and `POST /v1/metrics` in the OTLP/JSON
//! encoding (`OTEL_EXPORTER_OTLP_PROTOCOL=http/json`) from exporters on
//! this machine, keeps the `api_request` events as usage samples and
//! acknowledges everything else without reading it. An `api_request` event
//! it cannot account is answered as OTLP's partial success
//! (`rejectedLogRecords`, `errorMessage`) and kept as a source gap. One small HTTP/1.1
//! server of its own, as the webhook and judge tests stand one up: the
//! daemon's HTTP otherwise is outbound, and a dependency for two routes
//! would be weight without use.
//!
//! The receiver is re-read with the usage configuration every thirty
//! seconds: a new address rebinds, a removed one closes the listener, and
//! a configuration that does not check, or an address that cannot be
//! bound, leaves none, with the reason kept as the receiver's [`Status`]
//! for the usage report and logged once when it changes. Payloads are
//! bounded (4 MiB, 16 KiB of headers, ten seconds to arrive) and never
//! retained; a payload that is not an OTLP export is refused with 400 and
//! nothing recorded, and storage that cannot take a batch answers 503 so
//! the exporter tries again.

use std::net::SocketAddr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use super::Daemon;
use super::usage::Received;

/// How often the configuration is read again.
const TICK: Duration = Duration::from_secs(30);
/// The most a request's head may take, including the request line.
const MAX_HEAD: usize = 16 * 1024;
/// The most a body may be: an export is seconds of events, not a log.
pub(crate) const MAX_BODY: usize = 4 * 1024 * 1024;
/// How long one request has to arrive whole and be answered.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections served at once; the exporter opens one or two.
const MAX_CONNECTIONS: usize = 16;

/// Start the receiver's keeper: it follows the configuration for as long
/// as the daemon lives.
pub(crate) fn spawn(daemon: Arc<Daemon>) {
    let weak = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::spawn(keep(weak, TICK));
}

/// What the receiver is doing, as the usage report says it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Status {
    /// No address is configured; nothing listens.
    #[default]
    Off,
    /// Taking exports on this address.
    Listening(SocketAddr),
    /// Configured, and not listening, for this reason: a configuration
    /// that does not check, or an address that cannot be bound. Retried
    /// on every tick.
    Refused(String),
}

/// A bound listener and the task serving it; dropping it stops both.
struct Receiver {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn keep(weak: Weak<Daemon>, tick: Duration) {
    let mut current: Option<Receiver> = None;
    let mut status = Status::Off;
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let Some(daemon) = weak.upgrade() else {
            return;
        };
        let home = daemon.home.clone();
        let wanted = tokio::task::spawn_blocking(move || {
            super::usage::configuration(&home).and_then(|config| config.otlp_listen())
        })
        .await
        .unwrap_or_else(|_| Err("usage configuration read did not complete".into()));
        let next = match wanted {
            Err(reason) => {
                current = None;
                Status::Refused(reason)
            }
            Ok(None) => {
                current = None;
                Status::Off
            }
            Ok(Some(address)) if current.as_ref().is_some_and(|r| r.address == address) => {
                Status::Listening(address)
            }
            Ok(Some(address)) => {
                current = None;
                match bind(address).await {
                    Ok(listener) => {
                        let task = tokio::spawn(serve(weak.clone(), listener));
                        current = Some(Receiver { address, task });
                        Status::Listening(address)
                    }
                    Err(error) => Status::Refused(format!("cannot listen on {address}: {error}")),
                }
            }
        };
        if next != status {
            match &next {
                Status::Listening(address) => {
                    info!(%address, "usage telemetry receiver listening")
                }
                Status::Refused(reason) => {
                    warn!(%reason, "usage telemetry receiver not listening")
                }
                Status::Off => info!("usage telemetry receiver off"),
            }
            super::lock(&daemon.state).otlp_status = next.clone();
            status = next;
        }
    }
}

/// Bind the configured address and nothing else: a non-loopback address
/// is refused here too, in case the configuration check is ever bypassed.
pub(crate) async fn bind(address: SocketAddr) -> std::io::Result<TcpListener> {
    if !address.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the telemetry receiver listens on loopback only",
        ));
    }
    TcpListener::bind(address).await
}

/// Serve the listener until the daemon is gone or the task is dropped.
pub(crate) async fn serve(weak: Weak<Daemon>, listener: TcpListener) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(%error, "usage telemetry receiver accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        // Only this machine's processes reach a loopback listener, but a
        // listener that somehow is not loopback must still refuse peers.
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let weak = weak.clone();
        tokio::spawn(async move {
            let _slot = slot;
            if let Err(error) =
                tokio::time::timeout(REQUEST_TIMEOUT, connection(weak, stream)).await
            {
                debug!(%error, "usage telemetry request timed out");
            }
        });
    }
}

/// One HTTP/1.1 request on a fresh connection, answered and closed.
async fn connection(weak: Weak<Daemon>, mut stream: TcpStream) {
    let request = match read_request(&mut stream).await {
        Ok(request) => request,
        Err(reply) => {
            let _ = stream.write_all(reply.render().as_bytes()).await;
            let _ = stream.shutdown().await;
            return;
        }
    };
    let reply = match weak.upgrade() {
        Some(daemon) => respond(&daemon, &request),
        None => Reply::new(503, "the daemon is shutting down"),
    };
    let _ = stream.write_all(reply.render().as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// What the receiver needs of a request.
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    /// Whether the request named an `Origin`: a browser's does, an
    /// exporter's never.
    pub origin: bool,
    pub body: Vec<u8>,
}

/// A response: a status, a JSON body, and the connection closed after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    fn new(status: u16, message: &str) -> Self {
        // The OTLP/HTTP failure body is a `google.rpc.Status`: a code and
        // a message. 3 is INVALID_ARGUMENT, 14 UNAVAILABLE.
        let code = match status {
            200 => 0,
            503 => 14,
            _ => 3,
        };
        Self {
            status,
            body: serde_json::json!({"code": code, "message": message}).to_string(),
        }
    }

    /// Everything accepted: OTLP leaves `partialSuccess` unset.
    fn accepted() -> Self {
        Self {
            status: 200,
            body: "{}".to_owned(),
        }
    }

    /// Accepted but for `rejected` accounting events, which were not
    /// counted: OTLP's partial success, with why. A 64-bit count is a
    /// decimal string in the protobuf JSON mapping.
    fn partial(rejected: usize, reason: &str) -> Self {
        Self {
            status: 200,
            body: serde_json::json!({"partialSuccess": {
                "rejectedLogRecords": rejected.to_string(),
                "errorMessage": format!(
                    "{rejected} api_request event(s) not accounted: {reason}"
                ),
            }})
            .to_string(),
        }
    }

    fn render(&self) -> String {
        let reason = match self.status {
            200 => "OK",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Payload Too Large",
            415 => "Unsupported Media Type",
            503 => "Service Unavailable",
            _ => "Error",
        };
        format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.status,
            self.body.len(),
            self.body
        )
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, Reply> {
    let mut head = Vec::with_capacity(1024);
    let mut buffer = [0u8; 4096];
    let mut body = loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|_| Reply::new(400, "the request did not arrive"))?;
        if read == 0 {
            return Err(Reply::new(400, "the request ended before its headers"));
        }
        head.extend_from_slice(&buffer[..read]);
        if let Some(at) = head.windows(4).position(|w| w == b"\r\n\r\n") {
            let body = head.split_off(at + 4);
            head.truncate(at);
            break body;
        }
        if head.len() > MAX_HEAD {
            return Err(Reply::new(413, "request headers exceed 16 KiB"));
        }
    };
    let head = String::from_utf8(head).map_err(|_| Reply::new(400, "headers are not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut content_length = None;
    let mut content_type = None;
    let mut content_encoding = None;
    let mut origin = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| Reply::new(400, "Content-Length is not a number"))?,
                );
            }
            "content-type" => content_type = Some(value.to_ascii_lowercase()),
            "content-encoding" => content_encoding = Some(value.to_ascii_lowercase()),
            "origin" => origin = true,
            "transfer-encoding" => {
                return Err(Reply::new(
                    400,
                    "chunked requests are not taken; send Content-Length",
                ));
            }
            _ => {}
        }
    }
    let length = content_length.unwrap_or(0);
    if length > MAX_BODY {
        return Err(Reply::new(413, "the export exceeds 4 MiB"));
    }
    if body.len() > length {
        return Err(Reply::new(400, "the body is longer than Content-Length"));
    }
    while body.len() < length {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|_| Reply::new(400, "the body did not arrive"))?;
        if read == 0 {
            return Err(Reply::new(400, "the body ended before Content-Length"));
        }
        body.extend_from_slice(&buffer[..read]);
        if body.len() > length {
            return Err(Reply::new(400, "the body is longer than Content-Length"));
        }
    }
    Ok(Request {
        method,
        path,
        content_type,
        content_encoding,
        origin,
        body,
    })
}

/// The answer to one request, with its recording done. Synchronous: the
/// recording takes the state lock once and holds it across no await.
pub(crate) fn respond(daemon: &Daemon, request: &Request) -> Reply {
    if request.method != "POST" {
        return Reply::new(405, "the receiver takes POST only");
    }
    let path = request.path.split('?').next().unwrap_or("");
    if !matches!(path, "/v1/logs" | "/v1/metrics") {
        return Reply::new(404, "the receiver serves /v1/logs and /v1/metrics");
    }
    // A page any browser shows can reach this loopback port, and what it
    // sends would be taken as the provider's word. A browser names the
    // page's origin on every cross-site request and an exporter never
    // does; and without a declared JSON type a browser posts a body with
    // no preflight, so the type is required, not merely checked when
    // given (the preflight the typed request needs is an OPTIONS, refused
    // above).
    if request.origin {
        return Reply::new(
            403,
            "the receiver takes exports from an OTLP exporter, not from a web page",
        );
    }
    if request
        .content_encoding
        .as_deref()
        .is_some_and(|encoding| encoding != "identity")
    {
        return Reply::new(
            415,
            "compressed exports are not taken; leave OTEL_EXPORTER_OTLP_COMPRESSION unset",
        );
    }
    if !request
        .content_type
        .as_deref()
        .is_some_and(|kind| kind.starts_with("application/json"))
    {
        return Reply::new(
            415,
            "the receiver takes OTLP/JSON; set OTEL_EXPORTER_OTLP_PROTOCOL=http/json",
        );
    }
    let payload: serde_json::Value = match serde_json::from_slice(&request.body) {
        Ok(payload) => payload,
        Err(_) => return Reply::new(400, "the body is not JSON"),
    };
    if path == "/v1/metrics" {
        return match agentdocker_host::usage::otlp::count_metrics(&payload) {
            Ok(points) => {
                debug!(points, "usage telemetry metrics acknowledged, not recorded");
                Reply::accepted()
            }
            Err(reason) => Reply::new(400, &reason),
        };
    }
    let decoded = match agentdocker_host::usage::otlp::decode_logs(&payload) {
        Ok(decoded) => decoded,
        Err(reason) => return Reply::new(400, &reason),
    };
    // Only accounting events can be rejected: every other event is
    // deliberately not read, and is neither a rejection nor a gap.
    if decoded.samples.is_empty() && decoded.skipped.is_empty() {
        return Reply::accepted();
    }
    match daemon.record_telemetry(&decoded.samples, &decoded.skipped) {
        Received::Recorded { accepted } => {
            debug!(
                records = decoded.records,
                samples = decoded.samples.len(),
                accepted,
                rejected = decoded.skipped.len(),
                "usage telemetry recorded"
            );
            match decoded.skipped.first() {
                None => Reply::accepted(),
                Some(reason) => Reply::partial(decoded.skipped.len(), reason),
            }
        }
        Received::Unavailable => Reply::new(503, "usage accounting is not taking records now"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::usage::{
        self,
        report::{Group, Report},
    };
    use agentdocker_core::{AgentSpec, Request as Rq, Response};
    use serde_json::json;
    use std::io::{Read, Write};

    fn export(session: &str, request: &str, cost: f64) -> String {
        let now = chrono::Utc::now().timestamp_nanos_opt().unwrap();
        json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[
            {"timeUnixNano": now.to_string(),
             "body":{"stringValue":"claude_code.api_request"},
             "attributes":[
                {"key":"event.name","value":{"stringValue":"api_request"}},
                {"key":"session.id","value":{"stringValue":session}},
                {"key":"request_id","value":{"stringValue":request}},
                {"key":"model","value":{"stringValue":"fixture-model"}},
                {"key":"cost_usd","value":{"doubleValue":cost}},
                {"key":"input_tokens","value":{"intValue":"10"}},
                {"key":"output_tokens","value":{"intValue":"20"}},
                {"key":"cache_read_tokens","value":{"intValue":"30"}},
                {"key":"cache_creation_tokens","value":{"intValue":"0"}}]},
            {"timeUnixNano":"1766000001000000000","body":{"stringValue":"claude_code.tool_decision"},
             "attributes":[{"key":"event.name","value":{"stringValue":"tool_decision"}}]}
        ]}]}]})
        .to_string()
    }

    /// A loopback address nothing listens on, as far as anyone can know.
    fn free_address() -> SocketAddr {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap()
    }

    fn post(address: SocketAddr, path: &str, kind: &str, body: &str) -> (u16, String) {
        send(
            address,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    /// One request as written, and the status and body it gets.
    fn send(address: SocketAddr, request: &str) -> (u16, String) {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        let status = reply
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap();
        let body = reply.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
        (status, body)
    }

    async fn report(daemon: &Arc<Daemon>, by: Group) -> Report {
        match daemon
            .handle(Rq::Usage {
                project: None,
                agent: None,
                since: Some("24h".into()),
                until: None,
                by,
            })
            .await
        {
            Response::Usage { report } => report,
            other => panic!("{other:?}"),
        }
    }

    /// Whether the keeper comes to say what `wanted` accepts, within a
    /// few seconds.
    async fn status_becomes(daemon: &Arc<Daemon>, wanted: impl Fn(&Status) -> bool) -> bool {
        for _ in 0..400 {
            let now = super::super::lock(&daemon.state).otlp_status.clone();
            if wanted(&now) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        false
    }

    /// Over the wire: an export's `api_request` events are recorded for
    /// the agent whose session they name, charged to the card it pulled
    /// and its role, with its provider-reported cost, and the card's detail says what it
    /// has cost; a replay adds nothing; what is not an export, not JSON,
    /// not a known route or not a POST is refused and nothing changes;
    /// metrics are acknowledged and not counted; the report says where
    /// the receiver listens.
    #[tokio::test]
    async fn the_receiver_records_priced_events_once_and_refuses_the_rest() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let address = free_address();
        std::fs::write(
            temp.path().join("agentd.toml"),
            format!("[usage]\nenabled = false\notlp_listen = \"{address}\"\n"),
        )
        .unwrap();
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        let Response::Agent { agent } = daemon
            .handle(Rq::Register {
                spec: AgentSpec {
                    name: "claude-fixture".into(),
                    runtime: "claude-code".into(),
                    workdir: Some(checkout.clone()),
                    labels: [
                        ("session_id".to_owned(), "sess-otlp".to_owned()),
                        ("role".to_owned(), "reviewer".to_owned()),
                    ]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("registration")
        };
        let Response::Task { task: card } = daemon
            .handle(Rq::TaskCreate {
                from: agent.id.to_string(),
                project: Some(checkout.display().to_string()),
                title: "Price the work".to_owned(),
                acceptance: "its cost is known".to_owned(),
                column: Some(agentdocker_core::Column::Ready),
                links: Vec::new(),
                board: None,
                lane: None,
                description: String::new(),
                priority: None,
                size: None,
                due: None,
                external_blocker: String::new(),
                parent: None,
                blocked_by: Vec::new(),
                tags: Vec::new(),
                plan: None,
            })
            .await
        else {
            panic!("a card is filed")
        };
        let pulled = daemon
            .handle(Rq::TaskPull {
                agent: agent.id.to_string(),
                task: Some(card.id.to_string()),
                take_over_from: None,
                project: None,
                board: None,
            })
            .await;
        assert!(
            matches!(
                &pulled,
                Response::TaskDetail {
                    lease: Some(_),
                    usage: None,
                    ..
                }
            ),
            "the card is held and has cost nothing yet: {pulled:?}"
        );

        // A `task:` lease taken after the card that is not a card (a build
        // campaign) is not what the work is charged to.
        assert!(matches!(
            daemon
                .handle(Rq::Claim {
                    agent: agent.id.to_string(),
                    resource: "task:local-build".into(),
                    mode: agentdocker_core::LeaseMode::Exclusive,
                    amount: None,
                    ttl_secs: 600,
                    note: None,
                    wait_secs: 0,
                    priority: 0,
                    automatic: false,
                    actor: None,
                })
                .await,
            Response::Lease { .. }
        ));

        let keeper = tokio::spawn(keep(Arc::downgrade(&daemon), Duration::from_millis(25)));
        assert!(
            status_becomes(&daemon, |s| *s == Status::Listening(address)).await,
            "the keeper listens on the configured address"
        );
        let seq_before = super::super::lock(&daemon.state).next_seq;
        let body = export("sess-otlp", "req_1", 0.0125);
        let (status, reply) = tokio::task::spawn_blocking({
            let body = body.clone();
            move || post(address, "/v1/logs", "application/json", &body)
        })
        .await
        .unwrap();
        assert_eq!(
            (status, reply.as_str()),
            (200, "{}"),
            "everything taken: no partial success"
        );
        let seq_after = super::super::lock(&daemon.state).next_seq;
        assert_eq!(seq_after, seq_before + 1, "one usage_recorded event");

        let by_agent = report(&daemon, Group::Agent).await;
        assert_eq!(by_agent.rows.len(), 1);
        let row = &by_agent.rows[0];
        assert_eq!(row.key.as_deref(), Some(agent.id.as_str()));
        assert_eq!(row.samples, 1);
        assert_eq!(row.counters.input_tokens.sum, Some(40));
        assert_eq!(row.counters.output_tokens.sum, Some(20));
        assert_eq!(row.cost_usd_micros.sum, Some(12_500));
        assert_eq!(row.cache_read_share_percent, Some(75));
        assert_eq!(
            by_agent.coverage.collection.otlp_listen,
            Some(address.to_string())
        );
        assert_eq!(by_agent.coverage.collection.otlp_error, None);
        assert_eq!(
            report(&daemon, Group::Card).await.rows[0].key.as_deref(),
            Some(card.id.as_str())
        );
        assert_eq!(
            report(&daemon, Group::Role).await.rows[0].key.as_deref(),
            Some("reviewer")
        );
        let Response::TaskDetail {
            usage: Some(spent), ..
        } = daemon
            .handle(Rq::TaskGet {
                task: card.id.to_string(),
            })
            .await
        else {
            panic!("the card says what it has cost")
        };
        assert_eq!(spent.key.as_deref(), Some(card.id.as_str()));
        assert_eq!(spent.samples, 1);
        assert_eq!(spent.cost_usd_micros.sum, Some(12_500));

        // The same request again: acknowledged, counted once, nothing to say.
        let (status, _) = tokio::task::spawn_blocking(move || {
            post(address, "/v1/logs", "application/json", &body)
        })
        .await
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(report(&daemon, Group::Agent).await.rows[0].samples, 1);
        assert_eq!(
            super::super::lock(&daemon.state).next_seq,
            seq_after,
            "a replay appends no event"
        );

        // An accounting event that cannot be counted beside one that can:
        // the one is counted, the other is OTLP's partial success and a gap.
        let mut mixed: serde_json::Value =
            serde_json::from_str(&export("sess-otlp", "req_2", 0.5)).unwrap();
        let records = mixed["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
            .as_array_mut()
            .unwrap();
        let mut unnamed = records[0].clone();
        unnamed["attributes"]
            .as_array_mut()
            .unwrap()
            .retain(|pair| pair["key"] != "request_id");
        records.push(unnamed);
        let mixed = mixed.to_string();
        let (status, reply) = tokio::task::spawn_blocking({
            let mixed = mixed.clone();
            move || post(address, "/v1/logs", "application/json", &mixed)
        })
        .await
        .unwrap();
        assert_eq!(status, 200);
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["partialSuccess"]["rejectedLogRecords"], "1");
        assert!(
            reply["partialSuccess"]["errorMessage"]
                .as_str()
                .is_some_and(|m| m.contains("api_request without request_id")),
            "{reply}"
        );
        let after_mixed = report(&daemon, Group::Agent).await;
        assert_eq!(after_mixed.rows[0].samples, 2, "the readable one counts");
        assert_eq!(after_mixed.coverage.source_gaps, 1, "the other is a gap");
        assert!(after_mixed.rows[0].counters.input_tokens.coverage != usage::Coverage::Complete);
        // Only the unreadable one again: still partial success, the same
        // gap, and no event for a rejection already on record this hour.
        let seq_mixed = super::super::lock(&daemon.state).next_seq;
        let (status, reply) = tokio::task::spawn_blocking(move || {
            post(address, "/v1/logs", "application/json", &mixed)
        })
        .await
        .unwrap();
        assert_eq!(status, 200);
        assert!(reply.contains("rejectedLogRecords"), "{reply}");
        assert_eq!(report(&daemon, Group::Agent).await.coverage.source_gaps, 1);
        assert_eq!(super::super::lock(&daemon.state).next_seq, seq_mixed);

        // Metrics are taken and not counted; the rest is refused.
        let cases: Vec<(&str, &str, String, u16)> = vec![
            (
                "/v1/metrics",
                "application/json",
                json!({"resourceMetrics":[{"scopeMetrics":[{"metrics":[{"name":"claude_code.cost.usage","sum":{"dataPoints":[{"asDouble":9.0}]}}]}]}]}).to_string(),
                200,
            ),
            ("/v1/logs", "application/json", "not json".into(), 400),
            ("/v1/logs", "application/json", json!({"resourceMetrics":[]}).to_string(), 400),
            ("/v1/logs", "application/x-protobuf", "\u{1}\u{2}".into(), 415),
            ("/v1/traces", "application/json", "{}".into(), 404),
        ];
        for (path, kind, body, expected) in cases {
            let (status, _) = tokio::task::spawn_blocking(move || post(address, path, kind, &body))
                .await
                .unwrap();
            assert_eq!(status, expected, "{path} {kind}");
        }
        let (status, _) =
            tokio::task::spawn_blocking(move || send(address, "GET /v1/logs HTTP/1.1\r\n\r\n"))
                .await
                .unwrap();
        assert_eq!(status, 405);

        // What a web page can make a browser send to this port: a POST with
        // no Content-Type (a simple request, sent with no preflight), one
        // with the type behind its Origin, and the preflight itself. Each
        // is refused and the forged export is not counted.
        let forged = export("sess-otlp", "req_forged", 9.0);
        let browser: Vec<(String, u16)> = vec![
            (
                format!(
                    "POST /v1/logs HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{forged}",
                    forged.len()
                ),
                415,
            ),
            (
                format!(
                    "POST /v1/logs HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{forged}",
                    forged.len()
                ),
                403,
            ),
            (
                "OPTIONS /v1/logs HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:8080\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type\r\n\r\n".to_owned(),
                405,
            ),
        ];
        for (request, expected) in browser {
            let line = request.lines().next().unwrap_or("").to_owned();
            let (status, _) = tokio::task::spawn_blocking(move || send(address, &request))
                .await
                .unwrap();
            assert_eq!(status, expected, "{line}");
        }
        let after_refusals = report(&daemon, Group::Agent).await;
        assert_eq!(after_refusals.rows[0].samples, 2);
        assert_eq!(after_refusals.rows[0].cost_usd_micros.sum, Some(512_500));
        keeper.abort();
    }

    /// A store that cannot say what the agent held when the response was
    /// made takes nothing from it: the export is refused for the exporter
    /// to retry, storage is fenced as after any failed read, and no sample
    /// is counted without its card and role.
    #[tokio::test]
    async fn a_store_that_cannot_read_what_was_held_refuses_the_export() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        assert!(matches!(
            daemon
                .handle(Rq::Register {
                    spec: AgentSpec {
                        name: "claude-fixture".into(),
                        runtime: "claude-code".into(),
                        workdir: Some(checkout),
                        labels: [("session_id".to_owned(), "sess-otlp".to_owned())]
                            .into_iter()
                            .collect(),
                        ..Default::default()
                    },
                    pid: None,
                    session: None,
                })
                .await,
            Response::Agent { .. }
        ));
        let seq_before = {
            let state = super::super::lock(&daemon.state);
            state.store.reject_usage_holding_reads_for_test();
            state.next_seq
        };
        let request = Request {
            method: "POST".into(),
            path: "/v1/logs".into(),
            content_type: Some("application/json".into()),
            content_encoding: None,
            origin: false,
            body: export("sess-otlp", "req_1", 0.0125).into_bytes(),
        };
        let reply = respond(&daemon, &request);
        assert_eq!(reply.status, 503, "{}", reply.body);
        let state = super::super::lock(&daemon.state);
        assert!(
            state.storage_failure().is_some(),
            "the failed read fences storage"
        );
        assert_eq!(state.next_seq, seq_before, "nothing was recorded");
    }

    /// The keeper follows the file: an address, a listener on it; none, no
    /// listener; an address another program holds, or one off this
    /// machine, no listener and the reason, which the report carries.
    #[tokio::test]
    async fn the_keeper_binds_what_the_file_says_and_only_loopback() {
        assert!(bind("0.0.0.0:0".parse().unwrap()).await.is_err());
        let temp = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(temp.path().to_owned(), temp.path().join("sock")).unwrap());
        let config = |text: String| std::fs::write(temp.path().join("agentd.toml"), text).unwrap();
        let address = free_address();
        config(format!("[usage]\notlp_listen = \"{address}\"\n"));
        let keeper = tokio::spawn(keep(Arc::downgrade(&daemon), Duration::from_millis(25)));
        assert!(status_becomes(&daemon, |s| *s == Status::Listening(address)).await);
        assert!(std::net::TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_ok());

        config("[usage]\n".to_owned());
        assert!(status_becomes(&daemon, |s| *s == Status::Off).await);
        assert!(
            std::net::TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err(),
            "the listener closed when the address went"
        );

        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = held.local_addr().unwrap();
        config(format!("[usage]\notlp_listen = \"{taken}\"\n"));
        assert!(
            status_becomes(&daemon, |s| matches!(
                s,
                Status::Refused(reason) if reason.starts_with(&format!("cannot listen on {taken}"))
            ))
            .await,
            "an address another program holds is a reason, not a listener"
        );
        let usage = report(&daemon, Group::Agent).await;
        assert_eq!(usage.coverage.collection.otlp_listen, None);
        assert!(
            usage
                .coverage
                .collection
                .otlp_error
                .is_some_and(|reason| reason.contains("cannot listen"))
        );
        drop(held);
        assert!(
            status_becomes(&daemon, |s| *s == Status::Listening(taken)).await,
            "it is retried, and binds once the port is free"
        );

        config("[usage]\notlp_listen = \"0.0.0.0:4318\"\n".to_owned());
        assert!(
            status_becomes(&daemon, |s| matches!(
                s,
                Status::Refused(reason) if reason.starts_with("usage.otlp_listen must be loopback")
            ))
            .await,
            "a non-loopback address is refused, and nothing listens"
        );
        assert!(std::net::TcpStream::connect_timeout(&taken, Duration::from_secs(1)).is_err());
        keeper.abort();
    }
}
