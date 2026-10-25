//! A copy of chosen projects' memories in Supermemory, its hosted API or a
//! server of the person's own, turned on in `[memory.supermemory]` and off
//! otherwise. Nothing depends on it: the memory here is complete without
//! it, and a mirror that cannot be reached only falls behind.
//!
//! Once a minute the copy is brought in step with the project's current
//! memories, against Supermemory's v5 API. What may be there is kept here
//! per project *and destination* (the server and the account's key), so a
//! change of either starts a new copy rather than inheriting another's:
//!
//! - `ids` — confirmed there;
//! - `processing` — accepted and still being processed, checked again;
//! - `pending` — about to be sent, or sent without an answer: may be there.
//!
//! Each run first takes out what is no longer said (forgotten or
//! superseded) from all three, by the server's per-document answer, so a
//! failing addition never holds a forget back; then records the additions
//! it is about to send as pending before any is sent, so a lost answer or
//! a restart can never leave a forgotten memory behind unrecorded; then
//! sends them, reading each answer's status. Only memories go: kept on
//! purpose, refused when they read like a secret.
//!
//! A `dreaming` mode, when the person chose one, goes with every addition,
//! and then a document is confirmed only once its memories are formed
//! (`system.dreamingStatus` is `done`, or the server does not say); a
//! `supporting_context` goes with every addition too. Neither is sent
//! unless written.
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use super::*;
use agentdocker_core::config::SupermemoryConfig;
use agentdocker_core::memory::{self, Memory};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(in crate::daemon) const MIRROR: &str = "memory_mirror";
/// Memories sent in one run, so a long backlog drains over several.
const ADDS_PER_RUN: usize = 50;
/// What every request says it comes from, so the server can tell
/// AgentDocker's documents apart.
const SOURCE: &str = "agentdocker";
/// Documents in processing looked at again in one run.
const CHECKS_PER_RUN: usize = 20;
/// Ids taken out per request.
const REMOVALS_PER_REQUEST: usize = 100;
/// The whole of one request.
const REQUEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// What a project's copy at one destination may hold.
#[derive(Debug, Serialize, Deserialize)]
pub(in crate::daemon) struct Mirrored {
    pub project: ProjectId,
    pub destination: String,
    pub ids: BTreeSet<String>,
    #[serde(default)]
    pub processing: BTreeSet<String>,
    #[serde(default)]
    pub pending: BTreeSet<String>,
    /// The last processing id looked at again: the next run goes on from
    /// it, so every one is looked at in turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Mirrored {
    fn empty(project: &ProjectId, destination: &str) -> Self {
        Self {
            project: project.clone(),
            destination: destination.to_owned(),
            ids: BTreeSet::new(),
            processing: BTreeSet::new(),
            pending: BTreeSet::new(),
            checked: None,
            updated_at: None,
        }
    }

    /// Every id that may be there.
    fn held(&self) -> BTreeSet<String> {
        self.ids
            .iter()
            .chain(&self.processing)
            .chain(&self.pending)
            .cloned()
            .collect()
    }

    fn forget(&mut self, id: &str) {
        self.ids.remove(id);
        self.processing.remove(id);
        self.pending.remove(id);
    }
}

/// A destination's fingerprint: the server and the key's own fingerprint,
/// never the key.
fn destination(base_url: &str, key: &str) -> String {
    let key = Sha256::digest(key.as_bytes());
    let mut digest = Sha256::new();
    digest.update(base_url.as_bytes());
    digest.update(b"\n");
    digest.update(key);
    digest.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the server said of one document.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    Done,
    Processing,
    Failed,
}

/// A status the contract names, or none: an answer without one is not an
/// answer to act on.
fn status(word: Option<&str>) -> Option<Status> {
    match word? {
        "done" => Some(Status::Done),
        "failed" => Some(Status::Failed),
        "unknown" | "queued" | "extracting" | "chunking" | "embedding" | "indexing" => {
            Some(Status::Processing)
        }
        _ => None,
    }
}

/// What a check says of a document whose text is in (`system.status` is
/// `done`) when the mirror waits for its memories: still processing while
/// `system.dreamingStatus` is `dreaming`, done once it is `done` or when the
/// server does not say (one older than the field, or a server of one's
/// own), and not an answer to act on otherwise.
fn formed(dreaming: &Value) -> Option<Status> {
    match dreaming {
        Value::Null => Some(Status::Done),
        Value::String(word) if word == "done" => Some(Status::Done),
        Value::String(word) if word == "dreaming" => Some(Status::Processing),
        _ => None,
    }
}

/// The calls one destination is reached by.
struct Server {
    /// [`REQUEST_DEADLINE`] for each request, no redirects, every status
    /// an answer to read, and TLS verified as every outbound client's is,
    /// against the roots the person's system trusts, so a server of one's
    /// own behind an authority the system trusts is reached
    /// ([`agentdocker_host::http`]).
    agent: ureq::Agent,
    auth: String,
    documents: String,
    /// The `dreaming` mode sent with each addition; while one is chosen, a
    /// document is done only once its memories are formed.
    dreaming: Option<String>,
    /// The `supportingContext` sent with each addition.
    supporting_context: Option<String>,
}

impl Server {
    fn new(mirror: &SupermemoryConfig, namespace: &str, key: &str) -> Self {
        Self {
            agent: agentdocker_host::http::Policy {
                deadline: REQUEST_DEADLINE,
                max_redirects: 0,
                https_only: false,
                status_as_error: false,
                user_agent: None,
            }
            .agent(),
            auth: format!("Bearer {key}"),
            documents: format!("{}/ns/{namespace}/document", mirror.base_url()),
            dreaming: mirror.dreaming.clone(),
            supporting_context: mirror.supporting_context.clone(),
        }
    }

    /// What an addition sends: the memory under its own id, with the
    /// person's `dreaming` mode and `supportingContext` when written.
    fn addition(&self, memory: &Memory) -> Value {
        let mut body = json!({
            "content": memory.text,
            "id": memory.id,
            "taskType": "memory",
            "metadata": {
                "agentdocker_memory": memory.id,
                "project": memory.project,
                "author": memory.author,
                "kept_at": memory.created_at,
            },
        });
        if let Some(mode) = &self.dreaming {
            body["dreaming"] = json!(mode);
        }
        if let Some(context) = &self.supporting_context {
            body["supportingContext"] = json!(context);
        }
        body
    }

    /// Delete `ids`: those the server says it deleted, or never had.
    fn delete(&self, ids: &[String]) -> Result<Vec<String>, String> {
        let body = json!({ "ids": ids });
        let mut response = self
            .agent
            .delete(&self.documents)
            .header("Authorization", &self.auth)
            .header("x-sm-source", SOURCE)
            .header("Content-Type", "application/json")
            .force_send_body()
            .send(body.to_string().as_bytes())
            .map_err(|error| format!("removing memories failed ({})", failure(&error)))?;
        let code = response.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(format!("removing memories was refused (HTTP {code})"));
        }
        let answer: Value = response
            .body_mut()
            .read_to_string()
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .ok_or(())
            .map_err(|_| "removing memories gave an answer that is not JSON".to_owned())?;
        // The contract's answer, or nothing is taken as deleted.
        let (Some(_), Some(errors)) = (answer["count"].as_u64(), answer["errors"].as_array())
        else {
            return Err("removing memories gave an answer the contract does not".to_owned());
        };
        if errors
            .iter()
            .any(|e| !e["id"].is_string() || !e["error"].is_string())
        {
            return Err("removing memories gave an answer the contract does not".to_owned());
        }
        let failed: BTreeSet<&str> = errors
            .iter()
            .filter(|e| {
                !e["error"]
                    .as_str()
                    .is_some_and(|why| why.to_ascii_lowercase().contains("not found"))
            })
            .filter_map(|e| e["id"].as_str())
            .collect();
        Ok(ids
            .iter()
            .filter(|id| !failed.contains(id.as_str()))
            .cloned()
            .collect())
    }

    /// Add one memory under its id; the status the server answers with.
    /// While a `dreaming` mode is chosen, `done` here says only that the
    /// text is in: the answer carries no `dreamingStatus`, so the document
    /// is held as processing until a check reads it.
    fn add(&self, memory: &Memory) -> Result<Status, String> {
        let body = self.addition(memory);
        let mut response = self
            .agent
            .post(&self.documents)
            .header("Authorization", &self.auth)
            .header("x-sm-source", SOURCE)
            .header("Content-Type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|error| format!("adding a memory failed ({})", failure(&error)))?;
        let code = response.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(format!("adding a memory was refused (HTTP {code})"));
        }
        let answer: Value = response
            .body_mut()
            .read_to_string()
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .ok_or(())
            .map_err(|_| "adding a memory gave an answer that is not JSON".to_owned())?;
        match status(answer["status"].as_str()) {
            Some(Status::Done) if self.dreaming.is_some() => Ok(Status::Processing),
            Some(status) => Ok(status),
            None => Err("adding a memory gave no status the contract names".to_owned()),
        }
    }

    /// One document's status now; `None` when the server has no such one.
    fn check(&self, id: &str) -> Result<Option<Status>, String> {
        let mut response = self
            .agent
            .get(format!("{}/{id}", self.documents))
            .header("Authorization", &self.auth)
            .header("x-sm-source", SOURCE)
            .call()
            .map_err(|error| format!("checking a memory failed ({})", failure(&error)))?;
        match response.status().as_u16() {
            404 => Ok(None),
            200..=299 => {
                let answer: Value = response
                    .body_mut()
                    .read_to_string()
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .ok_or(())
                    .map_err(|_| "checking a memory gave an answer that is not JSON".to_owned())?;
                let found = match status(answer["system"]["status"].as_str()) {
                    Some(Status::Done) if self.dreaming.is_some() => {
                        formed(&answer["system"]["dreamingStatus"])
                    }
                    found => found,
                };
                found
                    .map(Some)
                    .ok_or_else(|| "checking a memory gave no status the contract names".to_owned())
            }
            code => Err(format!("checking a memory was refused (HTTP {code})")),
        }
    }
}

/// A transport failure, said without the address or anything sent.
fn failure(error: &ureq::Error) -> &'static str {
    match error {
        ureq::Error::Timeout(_) => "timed out",
        ureq::Error::Io(_) | ureq::Error::ConnectionFailed => "could not connect",
        ureq::Error::Tls(_) => "TLS failed",
        _ => "transport error",
    }
}

impl Daemon {
    /// Start a mirror run unless one is under way.
    pub fn mirror_memories(self: &Arc<Self>) {
        if self.mirroring.swap(true, Ordering::SeqCst) {
            return;
        }
        let daemon = self.clone();
        tokio::spawn(async move {
            daemon.mirror_once().await;
            daemon.mirroring.store(false, Ordering::SeqCst);
        });
    }

    /// One pass over every mirrored project.
    pub(in crate::daemon) async fn mirror_once(self: &Arc<Self>) {
        let home = self.home.clone();
        let read = tokio::task::spawn_blocking(
            move || -> Result<Option<(SupermemoryConfig, String)>, String> {
                let config = crate::config::read(&home)?;
                let Some(mirror) = config.supermemory()? else {
                    return Ok(None);
                };
                let key = super::super::webhooks::read_secret(&mirror.key_file)
                    .map_err(|reason| format!("memory.supermemory: {reason}"))?;
                let key = String::from_utf8(key)
                    .map_err(|_| "memory.supermemory: the key file is not text".to_owned())?;
                Ok(Some((mirror.clone(), key)))
            },
        )
        .await
        .unwrap_or_else(|_| Err("configuration read did not complete".into()));
        let (mirror, key) = match read {
            Ok(Some(found)) => found,
            Ok(None) => return,
            Err(notice) => {
                self.config_notice(notice);
                return;
            }
        };
        for selector in &mirror.projects {
            let project = match self.resolve_project(selector).await {
                Ok(project) => project,
                Err(_) => {
                    self.config_notice(format!("memory.supermemory: no project is {selector}"));
                    continue;
                }
            };
            self.mirror_project(&mirror, &key, project).await;
        }
    }

    fn mirror_state(self: &Arc<Self>, project: &ProjectId, destination: &str) -> Mirrored {
        let id = format!("{}/{destination}", project.as_str());
        lock(&self.state)
            .store_read("memory mirror", |store| {
                store.document::<Mirrored>(MIRROR, &id)
            })
            .flatten()
            .unwrap_or_else(|| Mirrored::empty(project, destination))
    }

    /// Record what the copy may hold now, with how much changed.
    fn record_mirror(self: &Arc<Self>, mut held: Mirrored, added: usize, removed: usize) -> bool {
        held.updated_at = Some(Utc::now());
        let id = format!("{}/{}", held.project.as_str(), held.destination);
        let kinds = if added + removed > 0 {
            vec![EventKind::MemoryMirrored {
                project: held.project.clone(),
                added,
                removed,
            }]
        } else {
            Vec::new()
        };
        lock(&self.state)
            .commit_documents("memory mirror", vec![doc(MIRROR, id, &held)], kinds)
            .is_none()
    }

    async fn mirror_project(
        self: &Arc<Self>,
        mirror: &SupermemoryConfig,
        key: &str,
        project: ProjectId,
    ) {
        let destination = destination(mirror.base_url(), key);
        let memories = match lock(&self.state).project_memories(&project) {
            Ok(memories) => memories,
            Err(_) => return,
        };
        let mut held = self.mirror_state(&project, &destination);
        let server = Arc::new(Server::new(
            mirror,
            &SupermemoryConfig::namespace(&project),
            key,
        ));
        let (_, gone) = memory::mirror_plan(&memories, &held.held(), Utc::now());
        let mut removed = 0;
        // What is no longer said goes first, and on its own.
        for ids in gone.chunks(REMOVALS_PER_REQUEST) {
            let (server, ids) = (server.clone(), ids.to_vec());
            match tokio::task::spawn_blocking(move || server.delete(&ids)).await {
                Ok(Ok(deleted)) => {
                    removed += deleted.len();
                    for id in &deleted {
                        held.forget(id);
                    }
                }
                Ok(Err(reason)) => {
                    warn!(project = %project.short(), %reason, "memory mirror fell behind; it tries again in a minute");
                    break;
                }
                Err(_) => break,
            }
        }
        // Additions are recorded as possibly there before any is sent.
        let present: BTreeSet<String> = held.ids.iter().chain(&held.processing).cloned().collect();
        let (lacking, _) = memory::mirror_plan(&memories, &present, Utc::now());
        let sending: Vec<Memory> = lacking.into_iter().take(ADDS_PER_RUN).cloned().collect();
        held.pending.extend(sending.iter().map(|m| m.id.clone()));
        if !self.record_mirror(held, 0, removed) {
            return;
        }
        let mut held = self.mirror_state(&project, &destination);
        // What was already processing there is looked at again, in turn
        // from where the last run stopped: what is sent below is left until
        // the next run.
        let after = held.checked.clone().unwrap_or_default();
        let checking: Vec<String> = held
            .processing
            .range::<String, _>((
                std::ops::Bound::Excluded(&after),
                std::ops::Bound::Unbounded,
            ))
            .chain(held.processing.iter())
            .take(CHECKS_PER_RUN.min(held.processing.len()))
            .cloned()
            .collect();
        for id in checking {
            // Past this one next time, whatever its answer: an error stops
            // this run but never holds the turn for ever on one document.
            held.checked = Some(id.clone());
            let (server, asked) = (server.clone(), id.clone());
            match tokio::task::spawn_blocking(move || server.check(&asked)).await {
                Ok(Ok(Some(Status::Done))) => {
                    held.processing.remove(&id);
                    held.ids.insert(id.clone());
                }
                Ok(Ok(Some(Status::Processing))) => {}
                // Failed there: it may still be there, and is sent again.
                Ok(Ok(Some(Status::Failed))) => {
                    held.processing.remove(&id);
                    held.pending.insert(id.clone());
                }
                // Confirmed absent: sent again if still said.
                Ok(Ok(None)) => {
                    held.processing.remove(&id);
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        let mut added = 0;
        for memory in sending {
            let server = server.clone();
            let id = memory.id.clone();
            match tokio::task::spawn_blocking(move || server.add(&memory)).await {
                Ok(Ok(Status::Done)) => {
                    held.pending.remove(&id);
                    held.ids.insert(id);
                    added += 1;
                }
                Ok(Ok(Status::Processing)) => {
                    held.pending.remove(&id);
                    held.processing.insert(id);
                    added += 1;
                }
                // Failed there: left pending, sent again next run.
                Ok(Ok(Status::Failed)) => {}
                Ok(Err(reason)) => {
                    warn!(project = %project.short(), %reason, "memory mirror fell behind; it tries again in a minute");
                    break;
                }
                Err(_) => break,
            }
        }
        self.record_mirror(held, added, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;

    /// How the fake server answers, and what it was sent: method and path,
    /// authorization, body.
    #[derive(Default)]
    struct Fake {
        post: (u16, &'static str),
        delete_errors: Vec<String>,
        /// Answer a delete with a body the contract does not have.
        delete_malformed: bool,
        get: &'static str,
        /// A status for one document's check, over `get`.
        get_one: std::collections::HashMap<String, &'static str>,
        /// A document whose check the server refuses.
        get_refused: Option<String>,
        /// The `system.dreamingStatus` every check answers with; null
        /// leaves it out, as a server older than the field does.
        get_dreaming: Value,
        seen: Vec<(String, String, String)>,
        /// The `x-sm-source` of every request, in order.
        sources: Vec<String>,
    }

    type Shared = Arc<StdMutex<Fake>>;

    fn server(post: (u16, &'static str)) -> (String, Shared) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let shared: Shared = Arc::new(StdMutex::new(Fake {
            post,
            get: "done",
            ..Fake::default()
        }));
        let fake = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let (mut length, mut auth, mut source) = (0usize, String::new(), String::new());
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "authorization" => auth = value.trim().to_owned(),
                        "x-sm-source" => source = value.trim().to_owned(),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                let request: Vec<&str> = line.split_whitespace().collect();
                let (method, path) = (request[0].to_owned(), request[1].to_owned());
                let mut fake = fake.lock().unwrap();
                let (code, reply) = match method.as_str() {
                    "POST" => (
                        fake.post.0,
                        json!({"id": "x", "status": fake.post.1}).to_string(),
                    ),
                    "DELETE" => {
                        let asked: Value = serde_json::from_str(&body).unwrap();
                        let errors: Vec<Value> = asked["ids"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|id| fake.delete_errors.iter().any(|e| id == &e))
                            // What the server says of one it is still processing.
                            .map(|id| json!({"id": id, "error": "Document is still processing"}))
                            .collect();
                        let count = asked["ids"].as_array().unwrap().len() - errors.len();
                        if fake.delete_malformed {
                            (200, json!({"count": 0}).to_string())
                        } else {
                            (200, json!({"count": count, "errors": errors}).to_string())
                        }
                    }
                    _ => {
                        let id = path.rsplit('/').next().unwrap_or("").to_owned();
                        if fake.get_refused.as_deref() == Some(id.as_str()) {
                            (500, json!({"error": "x", "details": "y"}).to_string())
                        } else {
                            let status = fake.get_one.get(&id).copied().unwrap_or(fake.get);
                            let mut system = json!({"status": status});
                            if !fake.get_dreaming.is_null() {
                                system["dreamingStatus"] = fake.get_dreaming.clone();
                            }
                            (200, json!({"system": system}).to_string())
                        }
                    }
                };
                fake.seen.push((format!("{method} {path}"), auth, body));
                fake.sources.push(source);
                drop(fake);
                write!(
                    stream,
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        (address, shared)
    }

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    async fn register(daemon: &Arc<Daemon>, workdir: &std::path::Path) -> AgentRecord {
        match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "keeper".into(),
                    workdir: Some(workdir.to_owned()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        }
    }

    fn configure(dir: &TempDir, url: &str, project: &str, enabled: bool, key: &str) {
        configure_with(dir, url, project, enabled, key, "");
    }

    /// As [`configure`], with more of `[memory.supermemory]` after it.
    fn configure_with(
        dir: &TempDir,
        url: &str,
        project: &str,
        enabled: bool,
        key: &str,
        extra: &str,
    ) {
        let file = dir.path().join("supermemory.key");
        std::fs::write(&file, format!("{key}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(
            dir.path().join(agentdocker_core::config::FILE_NAME),
            format!(
                "[memory.supermemory]\nenabled = {enabled}\nkey_file = {:?}\nprojects = [{project:?}]\nbase_url = {url:?}\n{extra}",
                file.display().to_string()
            ),
        )
        .unwrap();
    }

    async fn keep(daemon: &Arc<Daemon>, agent: &AgentRecord, text: &str) -> String {
        match daemon
            .handle(Request::MemoryAdd {
                from: agent.id.to_string(),
                project: None,
                text: text.into(),
                links: Vec::new(),
                supersedes: None,
                standing: false,
                forget_after: None,
                roles: Vec::new(),
                valid_from: None,
            })
            .await
        {
            Response::Memory { memory } => memory.id,
            other => panic!("{other:?}"),
        }
    }

    async fn forget(daemon: &Arc<Daemon>, agent: &AgentRecord, id: &str) {
        let forgotten = daemon
            .handle(Request::MemoryForget {
                from: agent.id.to_string(),
                memory: id.to_owned(),
            })
            .await;
        assert!(matches!(forgotten, Response::Ok), "{forgotten:?}");
    }

    async fn project(dir: &TempDir) -> (Arc<Daemon>, AgentRecord, ProjectId) {
        let daemon = open(dir);
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let agent = register(&daemon, &repo).await;
        let project = agent.project.as_ref().unwrap().id();
        (daemon, agent, project)
    }

    fn methods(fake: &Shared) -> Vec<String> {
        fake.lock()
            .unwrap()
            .seen
            .iter()
            .map(|(line, _, _)| line.split(' ').next().unwrap().to_owned())
            .collect()
    }

    fn held(daemon: &Arc<Daemon>, project: &ProjectId, url: &str, key: &str) -> Mirrored {
        daemon.mirror_state(project, &destination(url, key))
    }

    /// The body of the last addition sent.
    fn last_addition(fake: &Shared) -> Value {
        let fake = fake.lock().unwrap();
        let (_, _, body) = fake
            .seen
            .iter()
            .rfind(|(line, _, _)| line.starts_with("POST"))
            .unwrap();
        serde_json::from_str(body).unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_addition_carries_dreaming_and_context_only_when_written() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "done"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        keep(&daemon, &agent, "tests run with nextest").await;
        daemon.mirror_once().await;
        let plain = last_addition(&fake);
        let fields = plain.as_object().unwrap();
        assert!(
            !fields.contains_key("dreaming") && !fields.contains_key("supportingContext"),
            "nothing unwritten is sent: {plain}"
        );

        configure_with(
            &tmp,
            &url,
            project.as_str(),
            true,
            "sm-test-key-0123456789",
            "dreaming = \"instant\"\nsupporting_context = \"Decisions about the build, with owners.\"\n",
        );
        keep(&daemon, &agent, "the reviewer gives verdicts").await;
        daemon.mirror_once().await;
        let chosen = last_addition(&fake);
        assert_eq!(chosen["content"], "the reviewer gives verdicts");
        assert_eq!(chosen["dreaming"], "instant");
        assert_eq!(
            chosen["supportingContext"],
            "Decisions about the build, with owners."
        );
        assert_eq!(chosen["taskType"], "memory");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn with_a_dreaming_mode_a_document_is_done_once_its_memories_are_formed() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "done"));
        configure_with(
            &tmp,
            &url,
            project.as_str(),
            true,
            "sm-test-key-0123456789",
            "dreaming = \"dynamic\"\n",
        );
        let now = |daemon: &Arc<Daemon>| held(daemon, &project, &url, "sm-test-key-0123456789");

        // The addition's `done` says the text is in, not its memories.
        let id = keep(&daemon, &agent, "deploys go out on Mondays").await;
        daemon.mirror_once().await;
        assert!(now(&daemon).processing.contains(&id));

        // Text in, memories still dreaming: still processing.
        fake.lock().unwrap().get_dreaming = json!("dreaming");
        daemon.mirror_once().await;
        assert_eq!(methods(&fake), ["POST", "GET"]);
        assert!(now(&daemon).processing.contains(&id));

        // Memories formed: confirmed.
        fake.lock().unwrap().get_dreaming = json!("done");
        daemon.mirror_once().await;
        let state = now(&daemon);
        assert!(state.ids.contains(&id) && !state.processing.contains(&id));

        // A dreaming status the contract does not name settles nothing; a
        // server that does not say (older, or one's own) is taken at its
        // `system.status`, as before.
        let other = keep(&daemon, &agent, "the board has a review lane").await;
        daemon.mirror_once().await;
        fake.lock().unwrap().get_dreaming = json!("someday");
        daemon.mirror_once().await;
        assert!(now(&daemon).processing.contains(&other));
        fake.lock().unwrap().get_dreaming = Value::Null;
        daemon.mirror_once().await;
        assert!(now(&daemon).ids.contains(&other));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn without_a_dreaming_mode_the_text_being_in_is_enough() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "queued"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        let id = keep(&daemon, &agent, "deploys go out on Mondays").await;
        daemon.mirror_once().await;
        fake.lock().unwrap().get_dreaming = json!("dreaming");
        daemon.mirror_once().await;
        assert!(
            held(&daemon, &project, &url, "sm-test-key-0123456789")
                .ids
                .contains(&id),
            "off unless chosen: `system.status` alone decides"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_copy_gets_what_is_kept_and_loses_what_is_forgotten() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "done"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");

        let first = keep(&daemon, &agent, "tests run with nextest").await;
        let second = keep(&daemon, &agent, "the reviewer gives verdicts").await;
        daemon.mirror_once().await;
        let path = format!("/ns/agentdocker_{}/document", project.as_str());
        {
            let fake = fake.lock().unwrap();
            assert_eq!(fake.seen.len(), 2, "{:?}", fake.seen);
            for (line, auth, body) in &fake.seen {
                assert_eq!(line, &format!("POST {path}"));
                assert_eq!(auth, "Bearer sm-test-key-0123456789");
                assert_eq!(
                    serde_json::from_str::<Value>(body).unwrap()["taskType"],
                    "memory"
                );
            }
        }
        // In step: nothing more is sent.
        daemon.mirror_once().await;
        assert_eq!(methods(&fake).len(), 2);

        forget(&daemon, &agent, &first).await;
        daemon.mirror_once().await;
        let (line, _, body) = fake.lock().unwrap().seen.last().cloned().unwrap();
        assert_eq!(line, format!("DELETE {path}"));
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["ids"],
            json!([first])
        );
        let now = held(&daemon, &project, &url, "sm-test-key-0123456789");
        assert_eq!(now.ids, BTreeSet::from([second]));
        assert!(now.pending.is_empty() && now.processing.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn work_still_processing_there_is_checked_until_done() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "queued"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        let id = keep(&daemon, &agent, "deploys go out on Mondays").await;
        daemon.mirror_once().await;
        assert!(
            held(&daemon, &project, &url, "sm-test-key-0123456789")
                .processing
                .contains(&id)
        );
        daemon.mirror_once().await;
        assert_eq!(methods(&fake), ["POST", "GET"]);
        assert!(
            held(&daemon, &project, &url, "sm-test-key-0123456789")
                .ids
                .contains(&id)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nothing_leaves_unless_turned_on_and_a_refusal_stays_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((409, "x"));
        let id = keep(&daemon, &agent, "deploys go out on Mondays").await;

        // No file, and a file that does not turn it on: nothing is sent.
        daemon.mirror_once().await;
        configure(
            &tmp,
            &url,
            project.as_str(),
            false,
            "sm-test-key-0123456789",
        );
        daemon.mirror_once().await;
        assert!(methods(&fake).is_empty());

        // On, and refused (a conflict is not "already there"): recorded as
        // possibly there before it was sent, held as nothing more, tried again.
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        daemon.mirror_once().await;
        let now = held(&daemon, &project, &url, "sm-test-key-0123456789");
        assert!(now.pending.contains(&id) && now.ids.is_empty());
        daemon.mirror_once().await;
        assert_eq!(methods(&fake), ["POST", "POST"]);

        // Forgotten while only possibly there: it is still taken out, and
        // first, though additions keep failing.
        forget(&daemon, &agent, &id).await;
        let second = keep(&daemon, &agent, "the board has a review lane").await;
        daemon.mirror_once().await;
        let sent = methods(&fake);
        assert_eq!(&sent[2..], ["DELETE", "POST"], "{sent:?}");
        let now = held(&daemon, &project, &url, "sm-test-key-0123456789");
        assert!(!now.pending.contains(&id));
        assert!(now.pending.contains(&second));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_document_still_processing_there_is_kept_to_delete_again() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "done"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        let stuck = keep(&daemon, &agent, "the cache lives in target").await;
        let other = keep(&daemon, &agent, "tests run with nextest").await;
        daemon.mirror_once().await;
        fake.lock().unwrap().delete_errors = vec![stuck.clone()];
        forget(&daemon, &agent, &stuck).await;
        forget(&daemon, &agent, &other).await;
        daemon.mirror_once().await;
        let now = held(&daemon, &project, &url, "sm-test-key-0123456789");
        assert_eq!(
            now.ids,
            BTreeSet::from([stuck.clone()]),
            "only the one it deleted goes"
        );
        fake.lock().unwrap().delete_errors.clear();
        daemon.mirror_once().await;
        assert!(
            held(&daemon, &project, &url, "sm-test-key-0123456789")
                .ids
                .is_empty()
        );
        let sources = fake.lock().unwrap().sources.clone();
        assert!(
            !sources.is_empty() && sources.iter().all(|s| s == SOURCE),
            "every request names its source: {sources:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_document_that_failed_there_is_still_deleted_when_forgotten() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "queued"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        let id = keep(&daemon, &agent, "deploys go out on Mondays").await;
        daemon.mirror_once().await;
        fake.lock().unwrap().get = "failed";
        daemon.mirror_once().await;
        let now = held(&daemon, &project, &url, "sm-test-key-0123456789");
        assert!(
            now.pending.contains(&id),
            "failed there: still may be there"
        );
        forget(&daemon, &agent, &id).await;
        daemon.mirror_once().await;
        let fake = fake.lock().unwrap();
        let (line, _, body) = fake
            .seen
            .iter()
            .find(|(l, _, _)| l.starts_with("DELETE"))
            .unwrap();
        assert!(line.starts_with("DELETE"));
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap()["ids"],
            json!([id])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_answer_the_contract_does_not_have_clears_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "done"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        let id = keep(&daemon, &agent, "tests run with nextest").await;
        daemon.mirror_once().await;
        fake.lock().unwrap().delete_malformed = true;
        forget(&daemon, &agent, &id).await;
        daemon.mirror_once().await;
        assert!(
            held(&daemon, &project, &url, "sm-test-key-0123456789")
                .ids
                .contains(&id)
        );
        // And a POST answered with no status is not an addition.
        fake.lock().unwrap().post = (200, "");
        let other = keep(&daemon, &agent, "the board has a review lane").await;
        daemon.mirror_once().await;
        let now = held(&daemon, &project, &url, "sm-test-key-0123456789");
        assert!(now.pending.contains(&other) && !now.ids.contains(&other));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn every_document_processing_there_is_looked_at_in_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "queued"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        for n in 0..25 {
            keep(&daemon, &agent, &format!("fact number {n} about the build")).await;
        }
        daemon.mirror_once().await;
        let processing = held(&daemon, &project, &url, "sm-test-key-0123456789").processing;
        assert_eq!(processing.len(), 25);
        // The early ones stay queued; the last one has failed there.
        let last = processing.iter().next_back().unwrap().clone();
        {
            let mut fake = fake.lock().unwrap();
            fake.get = "queued";
            fake.get_one.insert(last.clone(), "failed");
        }
        daemon.mirror_once().await;
        daemon.mirror_once().await;
        let now = held(&daemon, &project, &url, "sm-test-key-0123456789");
        assert!(now.pending.contains(&last), "the late one was looked at");
        assert!(!now.processing.contains(&last));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_check_the_server_refuses_does_not_hold_the_others_back() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "queued"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        keep(&daemon, &agent, "fact a").await;
        keep(&daemon, &agent, "fact b").await;
        daemon.mirror_once().await;
        let processing: Vec<String> = held(&daemon, &project, &url, "sm-test-key-0123456789")
            .processing
            .into_iter()
            .collect();
        let (first, second) = (processing[0].clone(), processing[1].clone());
        {
            let mut fake = fake.lock().unwrap();
            fake.get_refused = Some(first.clone());
            fake.get_one.insert(second.clone(), "done");
        }
        daemon.mirror_once().await;
        daemon.mirror_once().await;
        let now = held(&daemon, &project, &url, "sm-test-key-0123456789");
        assert!(now.ids.contains(&second), "the second was reached");
        assert!(
            now.processing.contains(&first),
            "the first stays to try again"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn another_destination_is_a_copy_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, agent, project) = project(&tmp).await;
        let (url, fake) = server((200, "done"));
        configure(&tmp, &url, project.as_str(), true, "sm-test-key-0123456789");
        keep(&daemon, &agent, "tests run with nextest").await;
        daemon.mirror_once().await;
        // Another account's key: what the first holds is not this one's.
        configure(
            &tmp,
            &url,
            project.as_str(),
            true,
            "sm-other-account-key-987",
        );
        daemon.mirror_once().await;
        assert_eq!(methods(&fake), ["POST", "POST"]);
        let seen = fake.lock().unwrap().seen.clone();
        assert_eq!(seen[1].1, "Bearer sm-other-account-key-987");
    }

    /// The mirror verifies its server's certificate with the platform's
    /// own verifier and trust roots, not the bundled Mozilla set ureq
    /// would use unasked, with verification on, so a server of one's own
    /// behind an authority the system trusts is reached; it keeps its
    /// deadline, follows no redirect and reads every status itself. A
    /// server that hangs up mid-handshake stops the request with a word,
    /// not a crash: the verifier is built before the first message.
    #[test]
    fn the_mirror_trusts_the_platforms_roots() {
        use ureq::tls::RootCerts;
        let hangs_up = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = hangs_up.local_addr().unwrap().port();
        let mirror = SupermemoryConfig {
            enabled: true,
            projects: vec!["p".to_owned()],
            key_file: "/unused".into(),
            base_url: Some(format!("https://127.0.0.1:{port}")),
            dreaming: None,
            supporting_context: None,
        };
        let server = Server::new(&mirror, "agentdocker_p", "sm-test-key-0123456789");
        let config = server.agent.config();
        let tls = config.tls_config();
        assert!(
            matches!(tls.root_certs(), RootCerts::PlatformVerifier),
            "roots: {:?}",
            tls.root_certs()
        );
        assert!(!tls.disable_verification());
        assert_eq!(config.timeouts().global, Some(REQUEST_DEADLINE));
        assert_eq!(config.max_redirects(), 0);
        assert!(!config.https_only());
        assert!(!config.http_status_as_error());

        let peer = std::thread::spawn(move || drop(hangs_up.accept().unwrap()));
        let answer = server.check("m1");
        peer.join().unwrap();
        let error = answer.unwrap_err();
        assert!(error.starts_with("checking a memory failed ("), "{error}");
        assert!(!error.contains("127.0.0.1"), "{error}");
    }
}
