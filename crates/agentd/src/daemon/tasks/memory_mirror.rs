//! A copy of chosen projects' memories in Supermemory, its hosted API or a
//! server of the person's own, turned on in `[memory.supermemory]` and off
//! otherwise. Nothing depends on it: the memory here is complete without
//! it, and a mirror that cannot be reached only falls behind.
//!
//! Once a minute the copy is brought in step with the project's current
//! memories: what it lacks is added under the memory's own id, and what is
//! no longer said — forgotten, or superseded — is deleted. What it holds is
//! kept here as one document per project, written only after the server
//! said yes, so a failure is retried from where it stopped and nothing is
//! sent twice on purpose. Only memories go: kept on purpose, refused when
//! they read like a secret.
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use super::*;
use agentdocker_core::config::SupermemoryConfig;
use agentdocker_core::memory::{self, Memory};
use serde::{Deserialize, Serialize};

pub(in crate::daemon) const MIRROR: &str = "memory_mirror";
/// Memories sent in one run, so a long backlog drains over several.
const ADDS_PER_RUN: usize = 50;
/// Ids taken out per request.
const REMOVALS_PER_REQUEST: usize = 100;
/// The whole of one request.
const REQUEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// What a project's copy holds, as far as this machine knows.
#[derive(Debug, Serialize, Deserialize)]
pub(in crate::daemon) struct Mirrored {
    pub project: ProjectId,
    pub ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

/// What one run managed before it stopped, and why it stopped, if it did.
#[derive(Debug, Default)]
struct Pushed {
    added: Vec<String>,
    removed: Vec<String>,
    stopped: Option<String>,
}

impl Mirrored {
    fn empty(project: &ProjectId) -> Self {
        Self {
            project: project.clone(),
            ids: BTreeSet::new(),
            updated_at: None,
        }
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

    async fn mirror_project(
        self: &Arc<Self>,
        mirror: &SupermemoryConfig,
        key: &str,
        project: ProjectId,
    ) {
        let (memories, mirrored) = {
            let mut state = lock(&self.state);
            let memories = match state.project_memories(&project) {
                Ok(memories) => memories,
                Err(_) => return,
            };
            let mirrored = state
                .store_read("memory mirror", |store| {
                    store.document::<Mirrored>(MIRROR, project.as_str())
                })
                .flatten()
                .unwrap_or_else(|| Mirrored::empty(&project));
            (memories, mirrored)
        };
        let (add, remove) = memory::mirror_plan(&memories, &mirrored.ids);
        if add.is_empty() && remove.is_empty() {
            return;
        }
        let add: Vec<Memory> = add.into_iter().take(ADDS_PER_RUN).cloned().collect();
        let base = mirror.base_url().to_owned();
        let namespace = SupermemoryConfig::namespace(&project);
        let key = key.to_owned();
        let pushed =
            tokio::task::spawn_blocking(move || push(&base, &namespace, &key, &add, &remove))
                .await
                .unwrap_or_else(|_| Pushed {
                    stopped: Some("the mirror run did not complete".into()),
                    ..Pushed::default()
                });
        if let Some(reason) = &pushed.stopped {
            warn!(project = %project.short(), %reason, "memory mirror fell behind; it tries again in a minute");
        }
        if pushed.added.is_empty() && pushed.removed.is_empty() {
            return;
        }
        let mut state = lock(&self.state);
        // What the copy holds now, read again under the lock: a forget
        // between the reads changes only the next run's plan.
        let mut now = state
            .store_read("memory mirror", |store| {
                store.document::<Mirrored>(MIRROR, project.as_str())
            })
            .flatten()
            .unwrap_or_else(|| Mirrored::empty(&project));
        now.ids.extend(pushed.added.iter().cloned());
        for id in &pushed.removed {
            now.ids.remove(id);
        }
        now.updated_at = Some(Utc::now());
        let kinds = vec![EventKind::MemoryMirrored {
            project: project.clone(),
            added: pushed.added.len(),
            removed: pushed.removed.len(),
        }];
        let _ = state.commit_documents(
            "memory mirror",
            vec![doc(MIRROR, project.as_str().to_owned(), &now)],
            kinds,
        );
    }
}

/// Send what the plan says, in order, stopping at the first refusal. The
/// key goes in a header and never in what is logged or returned.
fn push(base: &str, namespace: &str, key: &str, add: &[Memory], remove: &[String]) -> Pushed {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_DEADLINE))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let auth = format!("Bearer {key}");
    let url = format!("{base}/ns/{namespace}/document");
    let mut pushed = Pushed::default();
    for memory in add {
        let body = json!({
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
        let sent = agent
            .post(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/json")
            .send(body.to_string().as_bytes());
        match sent.map(|response| response.status().as_u16()) {
            // Already there under its id is as good as added.
            Ok(200..=299 | 409) => pushed.added.push(memory.id.clone()),
            Ok(status) => {
                pushed.stopped = Some(format!("adding a memory was refused (HTTP {status})"));
                return pushed;
            }
            Err(error) => {
                pushed.stopped = Some(format!("adding a memory failed ({})", failure(&error)));
                return pushed;
            }
        }
    }
    for ids in remove.chunks(REMOVALS_PER_REQUEST) {
        let body = json!({ "ids": ids });
        let sent = agent
            .delete(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/json")
            .force_send_body()
            .send(body.to_string().as_bytes());
        match sent.map(|response| response.status().as_u16()) {
            // Gone already is as good as removed.
            Ok(200..=299 | 404) => pushed.removed.extend(ids.iter().cloned()),
            Ok(status) => {
                pushed.stopped = Some(format!("removing memories was refused (HTTP {status})"));
                return pushed;
            }
            Err(error) => {
                pushed.stopped = Some(format!("removing memories failed ({})", failure(&error)));
                return pushed;
            }
        }
    }
    pushed
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;

    /// What a fake server was sent: request line, authorization, body.
    type Seen = Arc<StdMutex<Vec<(String, String, String)>>>;

    /// A server that answers every request with `status` and keeps what
    /// it was sent.
    fn server(status: u16) -> (String, Seen) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let kept = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let (mut length, mut auth) = (0usize, String::new());
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
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                kept.lock().unwrap().push((
                    line.trim_end().to_owned(),
                    auth,
                    String::from_utf8(body).unwrap(),
                ));
                let reply = "{\"id\":\"x\",\"status\":\"queued\"}";
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        (address, seen)
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

    fn configure(dir: &TempDir, url: &str, project: &str, enabled: bool) {
        let key = dir.path().join("supermemory.key");
        std::fs::write(&key, "sm-test-key-0123456789\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(
            dir.path().join(agentdocker_core::config::FILE_NAME),
            format!(
                "[memory.supermemory]\nenabled = {enabled}\nkey_file = {:?}\nprojects = [{project:?}]\nbase_url = {url:?}\n",
                key.display().to_string()
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
            })
            .await
        {
            Response::Memory { memory } => memory.id,
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_copy_gets_what_is_kept_and_loses_what_is_forgotten() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let agent = register(&daemon, &repo).await;
        let project = agent.project.as_ref().unwrap().id();
        let (url, seen) = server(200);
        configure(&tmp, &url, project.as_str(), true);

        let first = keep(&daemon, &agent, "tests run with nextest").await;
        let second = keep(&daemon, &agent, "the reviewer gives verdicts").await;
        daemon.mirror_once().await;
        let path = format!("/ns/agentdocker_{}/document", project.as_str());
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2, "{seen:?}");
            for (line, auth, body) in seen.iter() {
                assert_eq!(line, &format!("POST {path} HTTP/1.1"));
                assert_eq!(auth, "Bearer sm-test-key-0123456789");
                let body: Value = serde_json::from_str(body).unwrap();
                assert_eq!(body["taskType"], "memory");
                assert!([&first, &second].contains(&&body["id"].as_str().unwrap().to_owned()));
            }
        }
        // In step: nothing more is sent.
        daemon.mirror_once().await;
        assert_eq!(seen.lock().unwrap().len(), 2);

        let forgotten = daemon
            .handle(Request::MemoryForget {
                from: agent.id.to_string(),
                memory: first.clone(),
            })
            .await;
        assert!(matches!(forgotten, Response::Ok), "{forgotten:?}");
        daemon.mirror_once().await;
        let seen = seen.lock().unwrap();
        let (line, _, body) = seen.last().unwrap();
        assert_eq!(line, &format!("DELETE {path} HTTP/1.1"));
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap()["ids"],
            json!([first])
        );
        let held = lock(&daemon.state)
            .store_read("test", |store| {
                store.document::<Mirrored>(MIRROR, project.as_str())
            })
            .flatten()
            .unwrap();
        assert_eq!(held.ids, BTreeSet::from([second]));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nothing_leaves_unless_turned_on_and_a_refusal_is_retried() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = open(&tmp);
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let agent = register(&daemon, &repo).await;
        let project = agent.project.as_ref().unwrap().id();
        let (url, seen) = server(500);
        keep(&daemon, &agent, "deploys go out on Mondays").await;

        // No file, and a file that does not turn it on: nothing is sent.
        daemon.mirror_once().await;
        configure(&tmp, &url, project.as_str(), false);
        daemon.mirror_once().await;
        assert!(seen.lock().unwrap().is_empty());

        // On, and refused: nothing is recorded as held, so it is tried again.
        configure(&tmp, &url, project.as_str(), true);
        daemon.mirror_once().await;
        daemon.mirror_once().await;
        assert_eq!(seen.lock().unwrap().len(), 2, "tried again");
        let held = lock(&daemon.state)
            .store_read("test", |store| {
                store.document::<Mirrored>(MIRROR, project.as_str())
            })
            .flatten();
        assert!(held.is_none_or(|held| held.ids.is_empty()));
    }
}
