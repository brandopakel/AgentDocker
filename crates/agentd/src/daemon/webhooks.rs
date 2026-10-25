//! Webhooks: a signed copy of chosen events posted to an address the
//! person configured in `agentd.toml`. Best effort, by design: there is
//! no outbox, so a sink that is down for longer than its retries, a
//! daemon restart or a handover loses deliveries — a webhook is a way to
//! hear about the floor, never the record of it. The record stays in the
//! event log and the journal.
//!
//! Every sink runs as its own task off the state lock, with its own
//! bounded queue, one request at a time, a total deadline on each, and a
//! generation: a change to the file stops the old sinks (nothing they
//! hold is carried over) and starts new ones, so a queued event can never
//! go to an address or with a secret it was not queued under. What is
//! posted is a projection — the kind, the sequence, the time, a one-line
//! summary and a few identifiers — never the raw record, whose payloads
//! may hold what the person typed. A sink's own failure event is committed
//! through the ordinary path and never posted, so a failing sink cannot
//! feed itself.
//!
//! Every attempt is written to the sink's delivery log in the store —
//! which delivery, which attempt, what came of it, the status code or a
//! word for why there was none, how long it took and when the next is
//! due — bounded per sink and in all. The log has no event of its own:
//! one per attempt would let a failing sink fill the event log the
//! throttle on `webhook_failed` is there to protect. An attempt is made
//! and logged as one piece off the runtime, so one already on its way
//! when a reload stops its sink is still logged when it ends, and is
//! never tried again.
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use agentdocker_core::config::{WebhookConfig, WebhookFormat};
use agentdocker_core::webhook::{self, AttemptResult, WebhookAttempt};
use agentdocker_core::{ErrorCode, Event, EventKind, ProjectId, Response};
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::{Daemon, lock};

/// How many events one sink holds while a delivery is on its way; past
/// that the oldest is dropped and counted.
pub const QUEUE_EVENTS: usize = 256;
/// The bytes one sink's queue holds at most.
pub const QUEUE_BYTES: usize = 1024 * 1024;
/// A body larger than this is not sent: dropped as `too_large`.
pub const BODY_BYTES: usize = 64 * 1024;
/// Connecting and the whole request, together.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// Retry delays after the first attempt: three attempts in all.
pub const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(5)];
/// The most a `Retry-After` header can ask for.
pub const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);
/// A sink's failure is announced at most this often.
pub const FAILURE_NOTICE_EVERY: Duration = Duration::from_secs(60);
/// A secret shorter than this is refused: a signature over a short secret
/// is a signature in name only.
pub const SECRET_MIN_BYTES: usize = 16;

/// The sinks now running, with the configuration they were started from
/// so an unchanged file does not restart them.
#[derive(Default)]
pub(super) struct Sinks {
    generation: u64,
    /// Set when this generation's sinks stop, for the attempts already on
    /// their way, which aborting their tasks does not stop.
    stopped: Arc<AtomicBool>,
    started_from: Vec<Started>,
    workers: Vec<tokio::task::JoinHandle<()>>,
}

impl Sinks {
    fn stop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        for worker in self.workers.drain(..) {
            worker.abort();
        }
        self.started_from.clear();
    }
}

/// A generation of sinks as their senders see it: its number, and
/// whether it has been stopped.
#[derive(Clone)]
struct Generation {
    number: u64,
    stopped: Arc<AtomicBool>,
}

impl Drop for Sinks {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What a sink posts: never the raw record.
#[derive(Clone, Debug, Serialize)]
struct Projection {
    delivery: String,
    event: String,
    seq: u64,
    at: String,
    /// One line about the event, from its identifiers: what kind of thing
    /// happened to whom, no free text of the person's or an agent's.
    text: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    ids: BTreeMap<&'static str, Value>,
}

/// The identifier fields an event may carry, and nothing else: an id is
/// safe to post; a payload, an answer, an environment or a reason is not.
const ID_FIELDS: [&str; 12] = [
    "agent",
    "project",
    "by",
    "from",
    "to",
    "resource",
    "message",
    "task",
    "question",
    "requester",
    "held_by",
    "channel",
];

fn kind_name(kind: &EventKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| {
            value
                .get("event")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The projection of one event, or `None` for an event that is never
/// posted (a webhook's own).
fn project_event(event: &Event) -> Option<Projection> {
    let name = kind_name(&event.kind);
    if name.starts_with("webhook_") {
        return None;
    }
    let raw = serde_json::to_value(&event.kind).unwrap_or(Value::Null);
    let mut ids = BTreeMap::new();
    for field in ID_FIELDS {
        match raw.get(field) {
            Some(Value::String(s)) => {
                ids.insert(field, Value::String(s.clone()));
            }
            Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
                ids.insert(field, Value::Array(items.clone()));
            }
            // A lease carries its holder and resource one level down.
            _ => {}
        }
    }
    if let Some(lease) = raw.get("lease") {
        for field in ["holder", "resource"] {
            if let Some(Value::String(s)) = lease.get(field) {
                ids.insert(
                    if field == "holder" {
                        "agent"
                    } else {
                        "resource"
                    },
                    json!(s),
                );
            }
        }
    }
    let mut text = name.replace('_', " ");
    for (field, value) in &ids {
        let shown = match value {
            Value::String(s) => short(s),
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_str)
                .map(short)
                .collect::<Vec<_>>()
                .join(","),
            _ => continue,
        };
        text.push_str(&format!(" {field}={shown}"));
    }
    Some(Projection {
        delivery: uuid::Uuid::new_v4().simple().to_string(),
        event: name,
        seq: event.seq,
        at: event.at.to_rfc3339(),
        text,
        ids,
    })
}

fn short(id: &str) -> String {
    if id.len() > 12 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        id[..12].to_owned()
    } else {
        id.to_owned()
    }
}

/// The body for a format: the projection as JSON, or Slack's one line.
fn body(projection: &Projection, format: WebhookFormat) -> Vec<u8> {
    match format {
        WebhookFormat::Json => serde_json::to_vec(projection).unwrap_or_default(),
        WebhookFormat::Slack => serde_json::to_vec(&json!({
            "text": format!("agentdocker: {}", projection.text),
        }))
        .unwrap_or_default(),
    }
}

/// `sha256=<hex>` of HMAC-SHA256 over `"{timestamp}.{body}"`, so a
/// receiver can check both who sent it and that it is not a replay.
pub fn signature(secret: &[u8], timestamp: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("any key length");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    let digest = mac.finalize().into_bytes();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

/// The secret, from a regular file of this user with mode 0600 and at
/// least sixteen bytes of content once trimmed, read through the opened
/// handle (no symlink is followed, and what is checked is what is read)
/// and never more than 4 KiB of it. Anything else is refused so a
/// world-readable, replaced or empty secret never signs anything.
pub fn read_secret(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = agentdocker_host::dirs::open_private(path)
        .map_err(|error| format!("secret_file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("secret_file: {}", error.kind()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            return Err("secret_file must be mode 0600".into());
        }
    }
    if metadata.len() > 4096 {
        return Err("secret_file is too large for a secret".into());
    }
    let mut content = Vec::new();
    file.take(4097)
        .read_to_end(&mut content)
        .map_err(|error| format!("secret_file: {}", error.kind()))?;
    if content.len() > 4096 {
        return Err("secret_file is too large for a secret".into());
    }
    let start = content
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(content.len());
    let end = content
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    let trimmed = content[start..end].to_vec();
    if trimmed.len() < SECRET_MIN_BYTES {
        return Err(format!(
            "secret_file holds fewer than {SECRET_MIN_BYTES} bytes"
        ));
    }
    Ok(trimmed)
}

/// What one attempt came to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Attempt {
    Delivered,
    /// Try again after this long: a 429, a 5xx or the network.
    Again(Duration),
    /// Do not try again: a 4xx other than 429.
    Refused(u16),
}

/// One attempt's outcome with what the log keeps of it: the receiver's
/// status code, or a word for why there was none.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answer {
    outcome: Attempt,
    status: Option<u16>,
    error: Option<&'static str>,
}

impl Answer {
    fn status(status: u16, retry_after: Option<Duration>) -> Self {
        Self {
            outcome: classify(status, retry_after),
            status: Some(status),
            error: None,
        }
    }

    fn failed(error: &'static str) -> Self {
        Self {
            outcome: Attempt::Again(Duration::ZERO),
            status: None,
            error: Some(error),
        }
    }
}

/// The client a sink posts with: `deadline` for connecting and the whole
/// request together, no redirects, every status an answer rather than an
/// error, and TLS verified as every outbound client's is, against the
/// roots the person's system trusts ([`agentdocker_host::http`]).
fn sender(deadline: Duration) -> ureq::Agent {
    agentdocker_host::http::Policy {
        deadline,
        max_redirects: 0,
        https_only: false,
        status_as_error: false,
        user_agent: None,
    }
    .agent()
}

/// One POST, blocking, with the whole request under one deadline. The
/// address is never in the error.
fn post(agent: &ureq::Agent, url: &str, headers: &[(&str, String)], body: &[u8]) -> Answer {
    let mut request = agent.post(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    match request.send(body) {
        Ok(response) => Answer::status(response.status().as_u16(), retry_after(&response)),
        Err(ureq::Error::StatusCode(code)) => Answer::status(code, None),
        Err(error) => Answer::failed(error_kind(&error)),
    }
}

/// A word for why a request had no answer, never the error's own text,
/// which can carry the address.
fn error_kind(error: &ureq::Error) -> &'static str {
    use std::io::ErrorKind;
    match error {
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::HostNotFound => "dns",
        ureq::Error::ConnectionFailed => "connect",
        ureq::Error::Io(io) => match io.kind() {
            ErrorKind::TimedOut => "timeout",
            ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
            | ErrorKind::AddrNotAvailable
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkUnreachable => "connect",
            _ => "io",
        },
        ureq::Error::Tls(_) | ureq::Error::Pem(_) | ureq::Error::Rustls(_) => "tls",
        ureq::Error::Protocol(_) | ureq::Error::Http(_) | ureq::Error::BadUri(_) => "protocol",
        _ => "other",
    }
}

/// What an attempt's outcome makes of its delivery, given which attempt
/// it was (from 0) and the delays between attempts: the result, and how
/// long to wait before the next attempt while one is due — the delay, or
/// a longer `Retry-After`.
fn settle(
    outcome: &Attempt,
    index: usize,
    delays: &[Duration],
) -> (AttemptResult, Option<Duration>) {
    match outcome {
        Attempt::Delivered => (AttemptResult::Delivered, None),
        Attempt::Refused(_) => (AttemptResult::GaveUp, None),
        Attempt::Again(after) => match delays.get(index) {
            Some(delay) => (AttemptResult::Retrying, Some((*delay).max(*after))),
            None => (AttemptResult::GaveUp, None),
        },
    }
}

fn classify(status: u16, retry_after: Option<Duration>) -> Attempt {
    match status {
        200..=299 => Attempt::Delivered,
        429 | 500..=599 => Attempt::Again(retry_after.unwrap_or(Duration::ZERO)),
        other => Attempt::Refused(other),
    }
}

fn retry_after(response: &ureq::http::Response<ureq::Body>) -> Option<Duration> {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|secs| Duration::from_secs(secs).min(RETRY_AFTER_CAP))
}

/// One attempt, made and logged off the runtime as one piece. A reload
/// aborts the sender's task while it waits, but not the request that task
/// started, so whatever comes of the request is logged here rather than
/// by the task. Nothing starts once the sink's generation has stopped,
/// and an attempt that ends after the stop is never tried again: a
/// failure then is `gave_up`.
struct Attempting {
    daemon: Weak<Daemon>,
    stopped: Arc<AtomicBool>,
    agent: Arc<ureq::Agent>,
    url: String,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    webhook: String,
    delivery: String,
    seq: u64,
    event: String,
    /// Which attempt, from 0.
    index: usize,
}

impl Attempting {
    /// The outcome, and how long to wait before the next attempt while
    /// one is due; `None` when the sink had stopped before it began.
    fn run(self) -> Option<(Attempt, Option<Duration>)> {
        if self.stopped.load(Ordering::SeqCst) {
            return None;
        }
        let at = chrono::Utc::now();
        let started = std::time::Instant::now();
        let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            post(&self.agent, &self.url, &self.headers, &self.body)
        }))
        .unwrap_or_else(|_| Answer::failed("other"));
        let took = started.elapsed();
        let (mut result, mut wait) = settle(&answer.outcome, self.index, &RETRY_DELAYS);
        if self.stopped.load(Ordering::SeqCst) {
            // The sink stopped meanwhile: nothing will try this again.
            if result == AttemptResult::Retrying {
                result = AttemptResult::GaveUp;
            }
            wait = None;
        }
        if let Some(daemon) = self.daemon.upgrade() {
            daemon.log_webhook_attempt(&WebhookAttempt {
                webhook: self.webhook,
                delivery: self.delivery,
                seq: self.seq,
                event: self.event,
                attempt: u32::try_from(self.index + 1).unwrap_or(u32::MAX),
                at,
                duration_ms: u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
                result,
                status: answer.status,
                error: answer.error.map(str::to_owned),
                next_retry: wait.map(|wait| {
                    chrono::Utc::now() + chrono::Duration::from_std(wait).unwrap_or_default()
                }),
            });
        }
        Some((answer.outcome, wait))
    }
}

/// A sink's queue between intake and delivery: bounded by events and
/// bytes, the oldest dropped and counted when full, so a receiver that
/// stalls can never make the daemon hold more than this.
#[derive(Default)]
struct Queue {
    items: VecDeque<(Projection, Vec<u8>)>,
    bytes: usize,
    /// Deliveries lost since the last notice: dropped for room, too
    /// large, or the bus overtook the intake.
    dropped: u64,
}

impl Queue {
    fn push(&mut self, projection: Projection, body: Vec<u8>) {
        while self.items.len() >= QUEUE_EVENTS || self.bytes + body.len() > QUEUE_BYTES {
            let Some((_, old)) = self.items.pop_front() else {
                break;
            };
            self.bytes -= old.len();
            self.dropped += 1;
        }
        self.bytes += body.len();
        self.items.push_back((projection, body));
    }

    fn pop(&mut self) -> Option<(Projection, Vec<u8>)> {
        let item = self.items.pop_front()?;
        self.bytes -= item.1.len();
        Some(item)
    }
}

/// Whether an event is for this sink: one of its kinds, and — for a
/// project-scoped sink — of that project. An event that names no project
/// is not that project's and is not posted to a scoped sink.
fn wanted(sink: &WebhookConfig, project: Option<&ProjectId>, event: &Event) -> bool {
    if !sink
        .events
        .iter()
        .any(|kind| *kind == kind_name(&event.kind))
    {
        return false;
    }
    match project {
        None => true,
        Some(wanted) => serde_json::to_value(&event.kind)
            .ok()
            .and_then(|value| {
                value
                    .get("project")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|project| project == wanted.as_str()),
    }
}

/// The intake half of a sink: receives from the bus without ever
/// waiting on the receiver, keeps the bounded queue, counts what it had
/// to drop.
async fn intake(
    sink: WebhookConfig,
    project: Option<ProjectId>,
    queue: Arc<std::sync::Mutex<Queue>>,
    wake: Arc<tokio::sync::Notify>,
    mut events: broadcast::Receiver<Event>,
) {
    loop {
        match events.recv().await {
            Ok(event) => {
                if !wanted(&sink, project.as_ref(), &event) {
                    continue;
                }
                let Some(projection) = project_event(&event) else {
                    continue;
                };
                let bytes = body(&projection, sink.format);
                let mut queue = lock(&queue);
                if bytes.len() > BODY_BYTES {
                    queue.dropped += 1;
                } else {
                    queue.push(projection, bytes);
                }
                drop(queue);
                wake.notify_one();
            }
            Err(broadcast::error::RecvError::Closed) => return,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                lock(&queue).dropped += n;
                wake.notify_one();
            }
        }
    }
}

/// The delivery half of a sink: one request at a time, retries, and a
/// notice — at most once every `notice_every` — for whatever was lost,
/// whether a receiver refused it or the queue had no room, even after
/// deliveries succeed again, and even when nothing else arrives. It
/// holds the daemon weakly: the daemon owns its sinks, and a worker
/// must not keep it alive.
async fn deliver(
    daemon: Weak<Daemon>,
    generation: Generation,
    sink: WebhookConfig,
    secret: Vec<u8>,
    queue: Arc<std::sync::Mutex<Queue>>,
    wake: Arc<tokio::sync::Notify>,
    notice_every: Duration,
) {
    let name = sink.name.clone();
    let agent = Arc::new(sender(REQUEST_DEADLINE));
    let mut last_notice: Option<tokio::time::Instant> = None;
    let mut last_reason = String::new();
    loop {
        let next = lock(&queue).pop();
        let Some((projection, bytes)) = next else {
            // Nothing queued: say what was lost meanwhile, then wait —
            // until the next event, or until a loss the throttle held
            // back may be said, whichever comes first.
            let Some(daemon) = daemon.upgrade() else {
                return;
            };
            announce(
                &daemon,
                &name,
                &queue,
                "",
                &last_reason,
                &mut last_notice,
                generation.number,
                notice_every,
            );
            drop(daemon);
            let held_back = lock(&queue).dropped > 0;
            match last_notice {
                Some(at) if held_back => {
                    tokio::select! {
                        _ = wake.notified() => {}
                        _ = tokio::time::sleep_until(at + notice_every) => {}
                    }
                }
                _ => wake.notified().await,
            }
            continue;
        };
        let mut reason = None;
        for index in 0..=RETRY_DELAYS.len() {
            let timestamp = chrono::Utc::now().timestamp().to_string();
            let headers = vec![
                ("content-type", "application/json".to_owned()),
                ("x-agentdocker-event", projection.event.clone()),
                ("x-agentdocker-delivery", projection.delivery.clone()),
                ("x-agentdocker-timestamp", timestamp.clone()),
                (
                    "x-agentdocker-signature",
                    signature(&secret, &timestamp, &bytes),
                ),
            ];
            let attempt = Attempting {
                daemon: daemon.clone(),
                stopped: generation.stopped.clone(),
                agent: agent.clone(),
                url: sink.url.clone(),
                headers,
                body: bytes.clone(),
                webhook: name.clone(),
                delivery: projection.delivery.clone(),
                seq: projection.seq,
                event: projection.event.clone(),
                index,
            };
            let Ok(Some((outcome, wait))) =
                tokio::task::spawn_blocking(move || attempt.run()).await
            else {
                // The sink stopped before the attempt began, or the
                // runtime is going away.
                return;
            };
            match outcome {
                Attempt::Delivered => {
                    reason = None;
                    break;
                }
                Attempt::Refused(status) => {
                    reason = Some(format!("refused:{status}"));
                    break;
                }
                Attempt::Again(_) => {
                    reason = Some("unreachable".to_owned());
                    if let Some(wait) = wait {
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        }
        if let Some(reason) = reason {
            lock(&queue).dropped += 1;
            last_reason = reason;
        }
        let Some(daemon) = daemon.upgrade() else {
            return;
        };
        announce(
            &daemon,
            &name,
            &queue,
            &projection.event,
            &last_reason,
            &mut last_notice,
            generation.number,
            notice_every,
        );
    }
}

/// Say what was lost, at most once every `notice_every` per sink: the
/// count since the last notice and the last reason (or `dropped` when
/// the queue had no room and nothing was refused), through the ordinary
/// event path. The count is taken under the one guard that decides to
/// say it, so a loss the intake counts meanwhile is never lost twice
/// over.
#[allow(clippy::too_many_arguments)]
fn announce(
    daemon: &Daemon,
    name: &str,
    queue: &std::sync::Mutex<Queue>,
    kind: &str,
    last_reason: &str,
    last_notice: &mut Option<tokio::time::Instant>,
    generation: u64,
    notice_every: Duration,
) {
    let dropped = {
        let mut queue = lock(queue);
        if queue.dropped == 0 || last_notice.is_some_and(|at| at.elapsed() < notice_every) {
            return;
        }
        std::mem::take(&mut queue.dropped)
    };
    *last_notice = Some(tokio::time::Instant::now());
    let reason = if last_reason.is_empty() {
        "dropped".to_owned()
    } else {
        last_reason.to_owned()
    };
    warn!(sink = %name, %reason, dropped, generation, "webhook deliveries lost");
    daemon.emit(EventKind::WebhookFailed {
        name: name.to_owned(),
        kind: kind.to_owned(),
        reason,
        dropped,
    });
}

/// The identity of a sink as started: its configuration and the digest
/// of its secret, so rotating the secret restarts the sink.
type Started = (WebhookConfig, [u8; 32]);

fn digest(secret: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    Sha256::digest(secret).into()
}

/// A TOML problem without its snippet: the file may hold an address
/// whose query carries a credential, and a log is not the place for it.
impl Daemon {
    /// Start, restart or stop the sinks from `agentd.toml`. Called at
    /// start and every few seconds: an unchanged configuration and
    /// secrets do nothing; a change to either stops every running sink
    /// and starts anew under the next generation; an unreadable file or
    /// secret keeps the last good sinks running and says so once.
    pub async fn reload_webhooks(self: &Arc<Self>) {
        let home = self.home.clone();
        // Everything that touches the disk happens off the runtime: the
        // file, and every secret through a verified handle.
        let read = tokio::task::spawn_blocking(
            move || -> Result<Vec<(WebhookConfig, Vec<u8>)>, String> {
                let config = crate::config::read(&home)?;
                let sinks = config.webhooks()?;
                sinks
                    .iter()
                    .map(|sink| {
                        read_secret(&sink.secret_file)
                            .map(|secret| (sink.clone(), secret))
                            .map_err(|reason| format!("webhook {}: {reason}", sink.name))
                    })
                    .collect()
            },
        )
        .await
        .unwrap_or_else(|_| Err("configuration read did not complete".into()));
        let wanted = match read {
            Ok(wanted) => wanted,
            Err(notice) => {
                self.config_notice(notice);
                return;
            }
        };
        let mut prepared = Vec::new();
        for (sink, secret) in wanted {
            let project = match &sink.project {
                Some(selector) => match self.resolve_project(selector).await {
                    Ok(id) => Some(id),
                    Err(_) => {
                        self.config_notice(format!(
                            "webhook {}: project `{selector}` is not known here",
                            sink.name
                        ));
                        return;
                    }
                },
                None => None,
            };
            prepared.push((sink, project, secret));
        }
        let started: Vec<Started> = prepared
            .iter()
            .map(|(sink, _, secret)| (sink.clone(), digest(secret)))
            .collect();
        let mut sinks = lock(&self.webhooks);
        if sinks.generation > 0 && sinks.started_from == started {
            return;
        }
        sinks.stop();
        sinks.generation += 1;
        sinks.stopped = Arc::new(AtomicBool::new(false));
        let generation = Generation {
            number: sinks.generation,
            stopped: sinks.stopped.clone(),
        };
        sinks.started_from = started;
        for (sink, project, secret) in prepared {
            let events = self.subscribe_events();
            let queue = Arc::new(std::sync::Mutex::new(Queue::default()));
            let wake = Arc::new(tokio::sync::Notify::new());
            info!(sink = %sink.name, generation = generation.number, events = sink.events.len(), "webhook sink started");
            sinks.workers.push(tokio::spawn(intake(
                sink.clone(),
                project,
                queue.clone(),
                wake.clone(),
                events,
            )));
            sinks.workers.push(tokio::spawn(deliver(
                Arc::downgrade(self),
                generation.clone(),
                sink,
                secret,
                queue,
                wake,
                FAILURE_NOTICE_EVERY,
            )));
        }
        if sinks.started_from.is_empty() && generation.number > 1 {
            info!(
                generation = generation.number,
                "webhook sinks stopped: none configured"
            );
        }
    }

    /// How many sinks run, for tests and status.
    pub fn webhook_sinks(&self) -> usize {
        lock(&self.webhooks).started_from.len()
    }

    /// Keep one attempt in its sink's delivery log, bounded per sink and
    /// in all. A write the handover fence skips is not kept; a store that
    /// fails is the daemon's storage failure, as for any other write.
    fn log_webhook_attempt(&self, attempt: &WebhookAttempt) {
        let _ = lock(&self.state).persist("webhook delivery log", |store| {
            store.log_webhook_attempt(attempt, webhook::LOG_PER_WEBHOOK, webhook::LOG_TOTAL)
        });
    }

    /// A sink's newest attempts, oldest first. A name that no running sink
    /// has and no logged attempt carries is not found.
    pub(super) fn webhook_deliveries(&self, name: &str, limit: Option<usize>) -> Response {
        let limit = webhook::log_limit(limit);
        let running = lock(&self.webhooks)
            .started_from
            .iter()
            .any(|(sink, _)| sink.name == name);
        let mut state = lock(&self.state);
        let Some(attempts) = state.store_read("webhook delivery log", |store| {
            store.webhook_attempts(name, limit)
        }) else {
            return state
                .storage_failure()
                .unwrap_or_else(|| Response::error(ErrorCode::Internal, "storage failed"));
        };
        if attempts.is_empty() && !running {
            return Response::error(
                ErrorCode::NotFound,
                format!("no webhook named `{name}` runs here or has a logged delivery"),
            );
        }
        Response::WebhookDeliveries {
            webhook: name.to_owned(),
            attempts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::Request;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    type Seen = Arc<Mutex<Vec<(BTreeMap<String, String>, Vec<u8>)>>>;

    /// A receiver on this machine that records what it was sent and
    /// answers as told: one status per request, in order.
    fn receiver(answers: Vec<u16>) -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/hook",
            listener.local_addr().unwrap().port()
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for status in answers {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                // A status of 0 is a receiver that stalls for two
                // seconds before answering 200: pressure on the queue.
                let status = if status == 0 {
                    std::thread::sleep(Duration::from_secs(2));
                    200
                } else {
                    status
                };
                log.lock().unwrap().push(read_request(&stream));
                reply(stream, status);
            }
        });
        (url, seen)
    }

    /// A receiver that holds its first request open, once read, until it
    /// is told what to answer, and answers every later one 200 at once;
    /// it records each request's delivery id.
    fn held_receiver() -> (
        String,
        Arc<Mutex<Vec<String>>>,
        std::sync::mpsc::Sender<u16>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/hook",
            listener.local_addr().unwrap().port()
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let (release, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut first = true;
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    return;
                };
                let (headers, _) = read_request(&stream);
                log.lock()
                    .unwrap()
                    .push(headers["x-agentdocker-delivery"].clone());
                let status = if std::mem::take(&mut first) {
                    answer.recv().unwrap_or(200)
                } else {
                    200
                };
                reply(stream, status);
            }
        });
        (url, seen, release)
    }

    /// One request's headers, by lowercase name, and its body.
    fn read_request(stream: &std::net::TcpStream) -> (BTreeMap<String, String>, Vec<u8>) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut headers = BTreeMap::new();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (headers, body)
    }

    /// Answer with a status and no body; a 429 asks for a second's wait.
    fn reply(mut stream: std::net::TcpStream, status: u16) {
        let reply = format!(
            "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n{}\r\n",
            if status == 429 {
                "retry-after: 1\r\n"
            } else {
                ""
            }
        );
        stream.write_all(reply.as_bytes()).unwrap();
    }

    fn secret_file(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("hook.secret");
        std::fs::write(&path, "0123456789abcdef0123456789abcdef\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }

    /// The projection carries identifiers and a line, never a payload; a
    /// webhook's own event is never projected; the signature covers the
    /// timestamp and the body.
    #[test]
    fn a_projection_is_identifiers_and_a_line_and_the_signature_covers_them() {
        let event = Event::new(
            EventKind::MessageSent {
                message: agentdocker_core::MessageId::from("446acd890d51301fdeadbeef".to_owned()),
                from: "c7bd0b9da841".into(),
                to: agentdocker_core::Destination::Agent("9b8a7c6d5e4f40312a1b2c3d4e5f6a7b".into()),
                kind: "chat".into(),
            },
            chrono::Utc::now(),
        );
        let projection = project_event(&event).unwrap();
        assert_eq!(projection.event, "message_sent");
        assert!(
            projection.text.starts_with("message sent "),
            "{}",
            projection.text
        );
        assert!(projection.ids.contains_key("message") && projection.ids.contains_key("from"));
        let raw = serde_json::to_string(&projection).unwrap();
        assert!(!raw.contains("payload") && !raw.contains("text\":\"hi"));
        let own = Event::new(
            EventKind::WebhookFailed {
                name: "x".into(),
                kind: "y".into(),
                reason: "z".into(),
                dropped: 1,
            },
            chrono::Utc::now(),
        );
        assert!(project_event(&own).is_none());
        let a = signature(b"secret", "1", b"body");
        assert!(a.starts_with("sha256=") && a.len() == 7 + 64);
        assert_ne!(a, signature(b"secret", "2", b"body"));
        assert_ne!(a, signature(b"other", "1", b"body"));
        let slack = body(&projection, WebhookFormat::Slack);
        let slack: Value = serde_json::from_slice(&slack).unwrap();
        assert!(
            slack["text"]
                .as_str()
                .unwrap()
                .starts_with("agentdocker: message sent")
        );
    }

    /// A secret file is refused unless it is a private regular file of
    /// this user with enough in it.
    #[test]
    fn the_secret_file_must_be_private_and_long_enough() {
        let dir = tempfile::tempdir().unwrap();
        let path = secret_file(dir.path());
        assert_eq!(read_secret(&path).unwrap().len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_secret(&path).unwrap_err().contains("0600"));
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(&path, "short").unwrap();
        assert!(read_secret(&path).unwrap_err().contains("fewer"));
        assert!(read_secret(&dir.path().join("missing")).is_err());
        assert!(read_secret(dir.path()).is_err(), "a directory");
        // Replaced by a symlink to a real secret: refused, not followed.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let real = secret_file(&elsewhere);
        let link = dir.path().join("link.secret");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(read_secret(&link).is_err(), "a symlink");
        }
        // Grown past what a secret can be: refused.
        std::fs::write(&path, "x".repeat(5000)).unwrap();
        assert!(read_secret(&path).unwrap_err().contains("too large"));
    }

    /// A project-scoped sink posts only that project's events: another
    /// project's are not its, and an event that names no project is not
    /// its either.
    #[test]
    fn a_scoped_sink_takes_only_its_projects_events() {
        let sink = WebhookConfig {
            name: "alpha".into(),
            url: "https://example.com/hook".into(),
            secret_file: "/tmp/x".into(),
            events: vec!["journal_pruned".into(), "policy_updated".into()],
            project: Some("alpha".into()),
            format: WebhookFormat::Json,
        };
        let alpha = ProjectId::from("alpha");
        let of = |project: &str| {
            Event::new(
                EventKind::JournalPruned {
                    project: ProjectId::from(project),
                    before_seq: 1,
                    removed: 0,
                    reason: "request".into(),
                },
                chrono::Utc::now(),
            )
        };
        assert!(wanted(&sink, Some(&alpha), &of("alpha")));
        assert!(!wanted(&sink, Some(&alpha), &of("beta")));
        let projectless = Event::new(
            EventKind::PolicyUpdated {
                project: None,
                rules: 0,
                quotas: 0,
                error: None,
                using_last_good: false,
            },
            chrono::Utc::now(),
        );
        assert!(
            !wanted(&sink, Some(&alpha), &projectless),
            "no project: not this one's"
        );
        assert!(
            wanted(&sink, None, &projectless),
            "an unscoped sink takes it"
        );
        let other_kind = Event::new(
            EventKind::WebhookFailed {
                name: "x".into(),
                kind: "y".into(),
                reason: "z".into(),
                dropped: 1,
            },
            chrono::Utc::now(),
        );
        assert!(!wanted(&sink, None, &other_kind));
    }

    /// A configured sink posts only the kinds it asked for, signed, with
    /// a stable delivery id across a retry; a refused status is not
    /// retried; the failure is announced once through the daemon's own
    /// events and never posted; a changed file restarts the sinks under a
    /// new generation and an absent one stops them.
    #[tokio::test]
    async fn a_sink_posts_chosen_events_signed_retries_and_announces_failure() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
        let secret = secret_file(&home);
        // 500 then 200 for the first event (one retry), 404 for the second.
        let (url, seen) = receiver(vec![500, 200, 404]);
        std::fs::write(
            home.join("agentd.toml"),
            format!(
                "[[webhooks]]\nname = \"local\"\nurl = \"{url}\"\nsecret_file = \"{}\"\nevents = [\"policy_updated\", \"lease_deadlock\"]\n",
                secret.display()
            ),
        )
        .unwrap();
        daemon.reload_webhooks().await;
        assert_eq!(daemon.webhook_sinks(), 1);
        // Unchanged: nothing restarts.
        daemon.reload_webhooks().await;
        assert_eq!(daemon.webhook_sinks(), 1);
        let mut own = daemon.subscribe_events();
        // Not asked for: never posted.
        daemon.emit(EventKind::JournalPruned {
            project: agentdocker_core::ProjectId::from("p"),
            before_seq: 1,
            removed: 0,
            reason: "request".into(),
        });
        daemon.emit(EventKind::PolicyUpdated {
            project: None,
            rules: 0,
            quotas: 0,
            error: None,
            using_last_good: false,
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while seen.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let posted = seen.lock().unwrap().clone();
        assert_eq!(posted.len(), 2, "one event, one retry");
        let (first, body_a) = &posted[0];
        let (second, body_b) = &posted[1];
        assert_eq!(first["x-agentdocker-event"], "policy_updated");
        assert_eq!(
            first["x-agentdocker-delivery"], second["x-agentdocker-delivery"],
            "one id"
        );
        assert_eq!(body_a, body_b, "one body across the retry");
        let timestamp = &second["x-agentdocker-timestamp"];
        assert_eq!(
            second["x-agentdocker-signature"],
            signature(&read_secret(&secret).unwrap(), timestamp, body_b)
        );
        let projection: Value = serde_json::from_slice(body_b).unwrap();
        assert_eq!(projection["event"], "policy_updated");
        assert!(projection["seq"].as_u64().unwrap() > 0);
        // A refused delivery is not retried and is announced once.
        daemon.emit(EventKind::PolicyUpdated {
            project: None,
            rules: 0,
            quotas: 0,
            error: None,
            using_last_good: false,
        });
        let mut failed = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while failed.is_none() && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(5), own.recv()).await {
                Ok(Ok(Event {
                    kind:
                        EventKind::WebhookFailed {
                            name,
                            kind,
                            reason,
                            dropped,
                        },
                    ..
                })) => failed = Some((name, kind, reason, dropped)),
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
        let (name, event, reason, dropped) = failed.expect("announced");
        assert_eq!(
            (name.as_str(), event.as_str(), reason.as_str(), dropped),
            ("local", "policy_updated", "refused:404", 1)
        );
        assert_eq!(seen.lock().unwrap().len(), 3, "no retry after a refusal");
        // Every attempt is in the log, oldest first: the 500 with its
        // retry due, the 200 under the same delivery, the refusal given up.
        let Response::WebhookDeliveries { webhook, attempts } = daemon
            .handle(Request::WebhookDeliveries {
                webhook: "local".into(),
                limit: None,
            })
            .await
        else {
            panic!("a delivery log");
        };
        assert_eq!(webhook, "local");
        let lines: Vec<_> = attempts
            .iter()
            .map(|a| (a.attempt, a.result, a.status, a.next_retry.is_some()))
            .collect();
        assert_eq!(
            lines,
            vec![
                (1, AttemptResult::Retrying, Some(500), true),
                (2, AttemptResult::Delivered, Some(200), false),
                (1, AttemptResult::GaveUp, Some(404), false),
            ]
        );
        assert_eq!(attempts[0].delivery, attempts[1].delivery, "one delivery");
        assert_ne!(attempts[1].delivery, attempts[2].delivery);
        assert_eq!(
            attempts[0].delivery, posted[0].0["x-agentdocker-delivery"],
            "the id the receiver saw"
        );
        assert!(attempts.iter().all(|a| a.event == "policy_updated"
            && a.seq > 0
            && a.error.is_none()
            && a.webhook == "local"));
        let retry = attempts[0].next_retry.unwrap();
        assert!(
            retry >= attempts[0].at + chrono::Duration::seconds(1),
            "{retry}"
        );
        // Nothing of the address, the secret or the body is kept.
        let raw = serde_json::to_string(&attempts).unwrap();
        let secret_text = String::from_utf8(read_secret(&secret).unwrap()).unwrap();
        for kept_out in [
            url.as_str(),
            "127.0.0.1",
            secret_text.as_str(),
            "ids",
            "text",
        ] {
            assert!(!raw.contains(kept_out), "{kept_out} in {raw}");
        }
        // A limit takes the newest, still oldest first.
        let Response::WebhookDeliveries {
            attempts: newest, ..
        } = daemon
            .handle(Request::WebhookDeliveries {
                webhook: "local".into(),
                limit: Some(1),
            })
            .await
        else {
            panic!("a delivery log");
        };
        assert_eq!(newest.as_slice(), &attempts[2..]);
        // A name nobody runs and nothing logged is not found.
        assert!(matches!(
            daemon
                .handle(Request::WebhookDeliveries {
                    webhook: "nobody".into(),
                    limit: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
        // The file goes away: the sinks stop, and their log stays readable.
        std::fs::remove_file(home.join("agentd.toml")).unwrap();
        daemon.reload_webhooks().await;
        assert_eq!(daemon.webhook_sinks(), 0);
        assert!(matches!(
            daemon
                .handle(Request::WebhookDeliveries {
                    webhook: "local".into(),
                    limit: None,
                })
                .await,
            Response::WebhookDeliveries { attempts, .. } if attempts.len() == 3
        ));
    }

    /// A sink's delivery log as the daemon answers it, or `None` when it
    /// answers otherwise.
    async fn logged(daemon: &Arc<Daemon>, webhook: &str) -> Option<Vec<WebhookAttempt>> {
        match daemon
            .handle(Request::WebhookDeliveries {
                webhook: webhook.into(),
                limit: None,
            })
            .await
        {
            Response::WebhookDeliveries { attempts, .. } => Some(attempts),
            _ => None,
        }
    }

    /// An attempt on its way when its sink stops — the sink removed, or
    /// its secret rotated — is logged once when it ends and never tried
    /// again: a reload aborts the sender's task, not the request it
    /// started, and nothing of an old generation posts after the reload.
    #[tokio::test]
    async fn an_attempt_on_its_way_when_its_sink_stops_is_logged_once_and_never_retried() {
        // A 200 after the sink is removed; a 503 after its secret rotates.
        for (status, removed) in [(200, true), (503, false)] {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().to_path_buf();
            let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
            let secret = secret_file(&home);
            let (url, seen, release) = held_receiver();
            std::fs::write(
                home.join("agentd.toml"),
                format!(
                    "[[webhooks]]\nname = \"local\"\nurl = \"{url}\"\nsecret_file = \"{}\"\nevents = [\"policy_updated\"]\n",
                    secret.display()
                ),
            )
            .unwrap();
            daemon.reload_webhooks().await;
            daemon.emit(EventKind::PolicyUpdated {
                project: None,
                rules: 0,
                quotas: 0,
                error: None,
                using_last_good: false,
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while seen.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let delivery = seen
                .lock()
                .unwrap()
                .first()
                .cloned()
                .expect("the POST is held open");
            if removed {
                std::fs::remove_file(home.join("agentd.toml")).unwrap();
            } else {
                std::fs::write(&secret, "fedcba9876543210fedcba9876543210\n").unwrap();
            }
            daemon.reload_webhooks().await;
            assert_eq!(daemon.webhook_sinks(), usize::from(!removed));
            release.send(status).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while logged(&daemon, "local")
                .await
                .unwrap_or_default()
                .is_empty()
                && std::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            // Past the first retry's delay: still one line, one request.
            tokio::time::sleep(RETRY_DELAYS[0] + Duration::from_secs(1)).await;
            let attempts = logged(&daemon, "local").await.expect("a delivery log");
            assert_eq!(attempts.len(), 1, "{status}: logged once: {attempts:?}");
            let attempt = &attempts[0];
            assert_eq!(attempt.delivery, delivery, "{status}");
            let settled = if status == 200 {
                AttemptResult::Delivered
            } else {
                AttemptResult::GaveUp
            };
            assert_eq!(
                (
                    attempt.attempt,
                    attempt.status,
                    attempt.result,
                    attempt.next_retry
                ),
                (1, Some(status), settled, None),
                "{status}"
            );
            assert_eq!(
                seen.lock().unwrap().len(),
                1,
                "{status}: no retry after the reload"
            );
        }
    }

    /// An outcome settles a delivery: delivered and refused end it; a
    /// failure is retried after its delay, or a longer `Retry-After`,
    /// until the delays run out, and then it gives up.
    #[test]
    fn an_outcome_settles_the_delivery_and_when_it_is_tried_again() {
        let delays = [Duration::from_secs(1), Duration::from_secs(5)];
        assert_eq!(
            settle(&Attempt::Delivered, 0, &delays),
            (AttemptResult::Delivered, None)
        );
        assert_eq!(
            settle(&Attempt::Refused(404), 0, &delays),
            (AttemptResult::GaveUp, None)
        );
        assert_eq!(
            settle(&Attempt::Again(Duration::ZERO), 0, &delays),
            (AttemptResult::Retrying, Some(Duration::from_secs(1)))
        );
        assert_eq!(
            settle(&Attempt::Again(Duration::from_secs(30)), 1, &delays),
            (AttemptResult::Retrying, Some(Duration::from_secs(30))),
            "a longer Retry-After wins"
        );
        assert_eq!(
            settle(&Attempt::Again(Duration::ZERO), 2, &delays),
            (AttemptResult::GaveUp, None),
            "the last attempt"
        );
        assert_eq!(classify(204, None), Attempt::Delivered);
        assert_eq!(classify(429, None), Attempt::Again(Duration::ZERO));
        assert_eq!(classify(503, None), Attempt::Again(Duration::ZERO));
        assert_eq!(classify(410, None), Attempt::Refused(410));
    }

    /// A request with no answer is logged by a word for why, never by the
    /// error's text: nobody listening is `connect`, a receiver that never
    /// answers is `timeout`.
    #[test]
    fn an_unanswered_attempt_is_named_by_a_word() {
        let agent = sender(Duration::from_millis(300));
        let closed = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let answer = post(
            &agent,
            &format!("http://127.0.0.1:{closed}/hook"),
            &[],
            b"{}",
        );
        assert_eq!(answer, Answer::failed("connect"));
        assert_eq!(answer.outcome, Attempt::Again(Duration::ZERO));
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        let held = std::thread::spawn(move || {
            let (stream, _) = silent.accept().unwrap();
            std::thread::sleep(Duration::from_secs(1));
            drop(stream);
        });
        let answer = post(&agent, &format!("http://127.0.0.1:{port}/hook"), &[], b"{}");
        assert_eq!(answer, Answer::failed("timeout"));
        held.join().unwrap();
        assert_eq!(error_kind(&ureq::Error::HostNotFound), "dns");
        assert_eq!(error_kind(&ureq::Error::Tls("handshake")), "tls");
    }

    /// A sink verifies its receiver's certificate with the platform's own
    /// verifier and trust roots, not the bundled Mozilla set ureq would
    /// use unasked, and with verification on; it follows no redirect and
    /// takes every status as an answer.
    #[test]
    fn a_sink_trusts_the_platforms_roots() {
        use ureq::tls::RootCerts;
        let agent = sender(REQUEST_DEADLINE);
        let config = agent.config();
        let tls = config.tls_config();
        assert!(
            matches!(tls.root_certs(), RootCerts::PlatformVerifier),
            "roots: {:?}",
            tls.root_certs()
        );
        assert!(!tls.disable_verification());
        assert_eq!(config.max_redirects(), 0);
        assert!(!config.https_only());
        assert!(!config.http_status_as_error());
        assert_eq!(config.timeouts().global, Some(REQUEST_DEADLINE));

        // An https attempt builds that verifier before its first message
        // (ureq panics there when the feature providing it is missing):
        // a receiver that hangs up mid-handshake is a failure named by a
        // word and tried again, not a crash.
        let hangs_up = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = hangs_up.local_addr().unwrap().port();
        let receiver = std::thread::spawn(move || drop(hangs_up.accept().unwrap()));
        let answer = post(
            &sender(Duration::from_secs(5)),
            &format!("https://127.0.0.1:{port}/hook"),
            &[],
            b"{}",
        );
        receiver.join().unwrap();
        assert_eq!(answer.status, None);
        assert!(answer.error.is_some(), "{answer:?}");
        assert_eq!(answer.outcome, Attempt::Again(Duration::ZERO));
    }

    /// A receiver that stalls does not stall the intake: the queue takes
    /// events up to its room, drops the oldest past it and counts them,
    /// and once deliveries succeed again what was lost is still said.
    /// Rotating the secret restarts the sink under a new generation.
    #[tokio::test]
    async fn a_stalled_receiver_costs_bounded_room_and_the_loss_is_said_after_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
        let secret = secret_file(&home);
        // The first answer stalls; then enough 200s for the queue's room.
        let mut answers = vec![0];
        answers.extend(std::iter::repeat_n(200, QUEUE_EVENTS + 2));
        let (url, seen) = receiver(answers);
        let config = format!(
            "[[webhooks]]\nname = \"local\"\nurl = \"{url}\"\nsecret_file = \"{}\"\nevents = [\"policy_updated\"]\n",
            secret.display()
        );
        std::fs::write(home.join("agentd.toml"), &config).unwrap();
        daemon.reload_webhooks().await;
        let mut own = daemon.subscribe_events();
        let burst = QUEUE_EVENTS + 40;
        for _ in 0..burst {
            daemon.emit(EventKind::PolicyUpdated {
                project: None,
                rules: 0,
                quotas: 0,
                error: None,
                using_last_good: false,
            });
        }
        // Everything the receiver takes arrives: the first (stalled) plus
        // what the queue held, never the whole burst.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while seen.lock().unwrap().len() < QUEUE_EVENTS + 1 && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let delivered = seen.lock().unwrap().len();
        assert!(
            delivered > QUEUE_EVENTS && delivered < burst,
            "{delivered} delivered of {burst}"
        );
        let mut lost = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while lost.is_none() && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(5), own.recv()).await {
                Ok(Ok(Event {
                    kind:
                        EventKind::WebhookFailed {
                            reason, dropped, ..
                        },
                    ..
                })) => lost = Some((reason, dropped)),
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
        let (reason, dropped) = lost.expect("the loss is said after recovery");
        assert_eq!(reason, "dropped");
        assert!(dropped >= (burst - QUEUE_EVENTS - 1) as u64, "{dropped}");
        // A rotated secret restarts the sink even with the file unchanged.
        std::fs::write(&secret, "fedcba9876543210fedcba9876543210\n").unwrap();
        daemon.reload_webhooks().await;
        assert_eq!(
            lock(&daemon.webhooks).generation,
            2,
            "rotation is a new generation"
        );
        daemon.reload_webhooks().await;
        assert_eq!(
            lock(&daemon.webhooks).generation,
            2,
            "unchanged: no restart"
        );
    }

    /// A loss inside the notice throttle is said when the throttle
    /// lifts, even when nothing else arrives afterwards: the delivery
    /// half waits on the next event or the throttle, whichever is first.
    #[tokio::test]
    async fn a_loss_held_back_by_the_throttle_is_said_after_silence() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
        let (url, seen) = receiver(vec![404, 404]);
        let sink = WebhookConfig {
            name: "local".into(),
            url,
            secret_file: secret_file(&home),
            events: vec!["policy_updated".into()],
            project: None,
            format: WebhookFormat::Json,
        };
        let queue = Arc::new(std::sync::Mutex::new(Queue::default()));
        let wake = Arc::new(tokio::sync::Notify::new());
        let notice_every = Duration::from_secs(2);
        let worker = tokio::spawn(deliver(
            Arc::downgrade(&daemon),
            Generation {
                number: 1,
                stopped: Arc::default(),
            },
            sink.clone(),
            b"0123456789abcdef0123456789abcdef".to_vec(),
            queue.clone(),
            wake.clone(),
            notice_every,
        ));
        let mut own = daemon.subscribe_events();
        let event = Event::new(
            EventKind::PolicyUpdated {
                project: None,
                rules: 0,
                quotas: 0,
                error: None,
                using_last_good: false,
            },
            chrono::Utc::now(),
        );
        let projection = project_event(&event).unwrap();
        let bytes = body(&projection, sink.format);
        // Two refusals in a row: the first is said at once, the second
        // falls inside the throttle and is held back.
        for _ in 0..2 {
            lock(&queue).push(projection.clone(), bytes.clone());
            wake.notify_one();
        }
        let mut notices = Vec::new();
        let started = tokio::time::Instant::now();
        let deadline = started + Duration::from_secs(20);
        while notices.len() < 2 && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(5), own.recv()).await {
                Ok(Ok(Event {
                    kind: EventKind::WebhookFailed { dropped, .. },
                    ..
                })) => notices.push((dropped, started.elapsed())),
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
        assert_eq!(seen.lock().unwrap().len(), 2, "both were posted once");
        assert_eq!(
            notices
                .iter()
                .map(|(dropped, _)| *dropped)
                .collect::<Vec<_>>(),
            vec![1, 1],
            "the held-back loss is said on its own: {notices:?}"
        );
        assert!(
            notices[1].1 >= notice_every,
            "not before the throttle lifts: {notices:?}"
        );
        worker.abort();
    }

    /// The daemon owns its sinks and no worker owns the daemon: when the
    /// last handle goes, the sinks stop with it, without the file
    /// having to be removed first.
    #[tokio::test]
    async fn the_last_owner_dropping_the_daemon_stops_its_sinks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
        let secret = secret_file(&home);
        let (url, _seen) = receiver(Vec::new());
        std::fs::write(
            home.join("agentd.toml"),
            format!(
                "[[webhooks]]\nname = \"local\"\nurl = \"{url}\"\nsecret_file = \"{}\"\nevents = [\"policy_updated\"]\n",
                secret.display()
            ),
        )
        .unwrap();
        daemon.reload_webhooks().await;
        assert_eq!(daemon.webhook_sinks(), 1);
        let workers: Vec<tokio::task::AbortHandle> = lock(&daemon.webhooks)
            .workers
            .iter()
            .map(tokio::task::JoinHandle::abort_handle)
            .collect();
        assert_eq!(workers.len(), 2, "an intake and a delivery per sink");
        let weak = Arc::downgrade(&daemon);
        drop(daemon);
        assert!(weak.upgrade().is_none(), "a worker kept the daemon alive");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while workers.iter().any(|worker| !worker.is_finished())
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            workers.iter().all(tokio::task::AbortHandle::is_finished),
            "the sinks stopped with the daemon"
        );
    }
}
