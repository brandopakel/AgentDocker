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
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agentdocker_core::config::{DaemonConfig, WebhookConfig, WebhookFormat};
use agentdocker_core::{Event, EventKind, ProjectId};
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::Daemon;

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
    started_from: Vec<WebhookConfig>,
    workers: Vec<tokio::task::JoinHandle<()>>,
}

impl Sinks {
    fn stop(&mut self) {
        for worker in self.workers.drain(..) {
            worker.abort();
        }
        self.started_from.clear();
    }
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
fn project(event: &Event) -> Option<Projection> {
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
/// least sixteen bytes of content once trimmed. Anything else is refused
/// so a world-readable or empty secret never signs anything.
pub fn read_secret(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("secret_file: {}", error.kind()))?;
    if !metadata.is_file() {
        return Err("secret_file must be a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe_uid() {
            return Err("secret_file must be owned by this user".into());
        }
        if metadata.mode() & 0o077 != 0 {
            return Err("secret_file must be mode 0600".into());
        }
    }
    if metadata.len() > 4096 {
        return Err("secret_file is too large for a secret".into());
    }
    let content = std::fs::read(path).map_err(|error| format!("secret_file: {}", error.kind()))?;
    let trimmed: Vec<u8> = content
        .iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip_while(u8::is_ascii_whitespace)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if trimmed.len() < SECRET_MIN_BYTES {
        return Err(format!(
            "secret_file holds fewer than {SECRET_MIN_BYTES} bytes"
        ));
    }
    Ok(trimmed)
}

#[cfg(unix)]
fn unsafe_uid() -> u32 {
    agentdocker_host::dirs::current_uid()
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

/// One POST, blocking, with the whole request under one deadline. The
/// address is never in the error.
fn post(agent: &ureq::Agent, url: &str, headers: &[(&str, String)], body: &[u8]) -> Attempt {
    let mut request = agent.post(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    match request.send(body) {
        Ok(response) => classify(response.status().as_u16(), retry_after(&response)),
        Err(ureq::Error::StatusCode(code)) => classify(code, None),
        Err(_) => Attempt::Again(Duration::ZERO),
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

/// One sink's task: filter, queue, deliver, retry, give up, say so.
async fn run(
    daemon: Arc<Daemon>,
    generation: u64,
    sink: WebhookConfig,
    project_id: Option<ProjectId>,
    secret: Vec<u8>,
    mut events: broadcast::Receiver<Event>,
) {
    let name = sink.name.clone();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_DEADLINE))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let agent = Arc::new(agent);
    let mut queue: VecDeque<(Projection, Vec<u8>)> = VecDeque::new();
    let mut queued_bytes = 0usize;
    let mut dropped: u64 = 0;
    let mut last_notice: Option<tokio::time::Instant> = None;
    loop {
        // Receive until the bus is quiet or the queue holds something.
        let received = if queue.is_empty() {
            events.recv().await
        } else {
            match events.try_recv() {
                Ok(event) => Ok(event),
                Err(broadcast::error::TryRecvError::Empty) => {
                    Err(broadcast::error::RecvError::Lagged(0))
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    Err(broadcast::error::RecvError::Closed)
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    Err(broadcast::error::RecvError::Lagged(n))
                }
            }
        };
        match received {
            Ok(event) => {
                if !sink
                    .events
                    .iter()
                    .any(|kind| *kind == kind_name(&event.kind))
                {
                    continue;
                }
                if let Some(wanted) = &project_id
                    && let Some(Value::String(project)) = serde_json::to_value(&event.kind)
                        .ok()
                        .and_then(|v| v.get("project").cloned())
                    && project != wanted.as_str()
                {
                    continue;
                }
                let Some(projection) = project(&event) else {
                    continue;
                };
                let bytes = body(&projection, sink.format);
                if bytes.len() > BODY_BYTES {
                    dropped += 1;
                    continue;
                }
                while queue.len() >= QUEUE_EVENTS || queued_bytes + bytes.len() > QUEUE_BYTES {
                    let Some((_, old)) = queue.pop_front() else {
                        break;
                    };
                    queued_bytes -= old.len();
                    dropped += 1;
                }
                queued_bytes += bytes.len();
                queue.push_back((projection, bytes));
                if queue.len() > 1 {
                    continue;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                // Lagged(0) is our own "nothing new" marker above.
                dropped += n;
            }
        }
        // Deliver the head, with retries, then say what was lost.
        let Some((projection, bytes)) = queue.pop_front() else {
            continue;
        };
        queued_bytes -= bytes.len();
        let mut reason = None;
        for attempt in 0..=RETRY_DELAYS.len() {
            let timestamp = chrono::Utc::now().timestamp().to_string();
            let headers = [
                ("content-type", "application/json".to_owned()),
                ("x-agentdocker-event", projection.event.clone()),
                ("x-agentdocker-delivery", projection.delivery.clone()),
                ("x-agentdocker-timestamp", timestamp.clone()),
                (
                    "x-agentdocker-signature",
                    signature(&secret, &timestamp, &bytes),
                ),
            ];
            let agent = agent.clone();
            let url = sink.url.clone();
            let body = bytes.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let headers: Vec<(&str, String)> =
                    headers.iter().map(|(n, v)| (*n, v.clone())).collect();
                post(&agent, &url, &headers, &body)
            })
            .await
            .unwrap_or(Attempt::Again(Duration::ZERO));
            match outcome {
                Attempt::Delivered => {
                    reason = None;
                    break;
                }
                Attempt::Refused(status) => {
                    reason = Some(format!("refused:{status}"));
                    break;
                }
                Attempt::Again(after) => {
                    reason = Some("unreachable".to_owned());
                    if let Some(delay) = RETRY_DELAYS.get(attempt) {
                        tokio::time::sleep((*delay).max(after)).await;
                    }
                }
            }
        }
        if let Some(reason) = reason {
            dropped += 1;
            let due = last_notice.is_none_or(|at| at.elapsed() >= FAILURE_NOTICE_EVERY);
            if due {
                warn!(sink = %name, %reason, dropped, generation, "webhook delivery failed");
                daemon.emit(EventKind::WebhookFailed {
                    name: name.clone(),
                    kind: projection.event.clone(),
                    reason,
                    dropped,
                });
                dropped = 0;
                last_notice = Some(tokio::time::Instant::now());
            }
        }
    }
}

impl Daemon {
    /// Start, restart or stop the sinks from `agentd.toml`. Called at
    /// start and every few seconds: an unchanged configuration does
    /// nothing; a changed one stops every running sink and starts anew
    /// under the next generation; an unreadable one keeps the last good
    /// sinks running and says so once.
    pub async fn reload_webhooks(self: &Arc<Self>) {
        use agentdocker_core::config::FILE_NAME;
        let path = self.home.join(FILE_NAME);
        let read = tokio::task::spawn_blocking({
            let path = path.clone();
            move || -> Result<Vec<WebhookConfig>, String> {
                let text = match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(Vec::new());
                    }
                    Err(error) => {
                        return Err(format!("cannot read {}: {}", path.display(), error.kind()));
                    }
                };
                let config: DaemonConfig =
                    toml::from_str(&text).map_err(|error| error.to_string())?;
                config.webhooks().map(<[WebhookConfig]>::to_vec)
            }
        })
        .await
        .unwrap_or_else(|_| Err("configuration read did not complete".into()));
        // An unreadable file keeps the last good sinks running and is
        // said once per distinct notice.
        let wanted = match read {
            Ok(wanted) => wanted,
            Err(notice) => {
                self.config_notice(notice);
                return;
            }
        };
        // Everything the sinks need before any of them starts, so a
        // half-readable set never runs half.
        let mut prepared = Vec::new();
        for sink in &wanted {
            let secret = match read_secret(&sink.secret_file) {
                Ok(secret) => secret,
                Err(reason) => {
                    self.config_notice(format!("webhook {}: {reason}", sink.name));
                    return;
                }
            };
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
            prepared.push((sink.clone(), project, secret));
        }
        let mut sinks = lock_sinks(self);
        if sinks.generation > 0 && sinks.started_from == wanted {
            return;
        }
        sinks.stop();
        sinks.generation += 1;
        let generation = sinks.generation;
        sinks.started_from = wanted.clone();
        for (sink, project, secret) in prepared {
            let events = self.subscribe_events();
            info!(sink = %sink.name, generation, events = sink.events.len(), "webhook sink started");
            sinks.workers.push(tokio::spawn(run(
                self.clone(),
                generation,
                sink,
                project,
                secret,
                events,
            )));
        }
        if wanted.is_empty() && generation > 1 {
            info!(generation, "webhook sinks stopped: none configured");
        }
    }

    /// How many sinks run, for tests and status.
    pub fn webhook_sinks(&self) -> usize {
        lock_sinks(self).workers.len()
    }
}

fn lock_sinks(daemon: &Daemon) -> std::sync::MutexGuard<'_, Sinks> {
    daemon
        .webhooks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
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
                log.lock().unwrap().push((headers, body));
                let mut stream = stream;
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
        });
        (url, seen)
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
                message: agentdocker_core::MessageId::from("26beba01be15456fdeadbeef".to_owned()),
                from: "0a68fb5752f7".into(),
                to: agentdocker_core::Destination::Agent("ac5c138c2b3f4d8f90c5924988419055".into()),
                kind: "chat".into(),
            },
            chrono::Utc::now(),
        );
        let projection = project(&event).unwrap();
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
        assert!(project(&own).is_none());
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
        assert!(
            read_secret(dir.path())
                .unwrap_err()
                .contains("regular file")
        );
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
        // The file goes away: the sinks stop.
        std::fs::remove_file(home.join("agentd.toml")).unwrap();
        daemon.reload_webhooks().await;
        assert_eq!(daemon.webhook_sinks(), 0);
    }
}
