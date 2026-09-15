//! Content observations are captured outside the state lock, then persisted
//! under it. Host timestamps describe the beginning of the observation.
use super::*;
use agentdocker_core::{ReadMark, StalePath};
use agentdocker_host::content;

impl Daemon {
    /// Resolve a running writer and its registered working directory under one
    /// guard. Unlike a read, a commit must not fall back to another directory
    /// when the agent registered no workdir. Policy keys use the checkout root
    /// separately, so registering from a subdirectory does not change them.
    pub(super) fn writer_checkout(
        &self,
        reference: &str,
    ) -> Result<(AgentId, PathBuf, Option<String>), Box<Response>> {
        let mut state = lock(&self.state);
        let id = state.resolve(reference)?;
        let record = state.registry.get(&id).unwrap();
        if record.status != AgentStatus::Running {
            return Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "commits require a running agent",
            )));
        }
        let workdir = record.spec.workdir.clone().ok_or_else(|| {
            Box::new(Response::error(
                ErrorCode::Invalid,
                "this agent registered no working directory, so there is no checkout of its own to commit; commit with git, or re-register from the checkout you are in",
            ))
        })?;
        Ok((
            id,
            project::canonical(&workdir),
            record.vcs.as_ref().and_then(|v| v.head.clone()),
        ))
    }

    pub(super) fn reader_checkout(
        &self,
        reference: &str,
    ) -> Result<(AgentId, PathBuf, Option<String>), Box<Response>> {
        let mut state = lock(&self.state);
        let id = state.resolve(reference)?;
        let record = state.registry.get(&id).unwrap();
        if record.status != AgentStatus::Running {
            return Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "observations require a running agent",
            )));
        }
        let root = record
            .project
            .as_ref()
            .map(|p| p.dir().to_path_buf())
            .or_else(|| record.spec.workdir.clone())
            .ok_or_else(|| {
                Box::new(Response::error(ErrorCode::Invalid, "agent has no checkout"))
            })?;
        Ok((
            id,
            project::canonical(&root),
            record.vcs.as_ref().and_then(|v| v.head.clone()),
        ))
    }

    pub(super) fn reads(&self, reference: &str) -> Response {
        let mut state = lock(&self.state);
        let id = match state.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        match state.store.document::<Vec<ReadMark>>("reads", id.as_str()) {
            Ok(reads) => Response::Reads {
                reads: reads.unwrap_or_default(),
            },
            Err(e) => Response::error(ErrorCode::Internal, e.to_string()),
        }
    }

    pub(super) async fn observe(&self, reference: &str, paths: Vec<String>) -> Response {
        if paths.is_empty() || paths.len() > 1000 {
            return Response::error(ErrorCode::Invalid, "observe requires 1–1000 paths");
        }
        let (id, root, head) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let observed_root = root.clone();
        let captured = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ReadMark>> {
            let mut marks = Vec::new();
            for raw in paths {
                let path = checkout_path(&root, &raw)?;
                let at = Utc::now();
                let version = content::fingerprint(&path)?;
                marks.push(ReadMark {
                    path,
                    at,
                    version,
                    head: head.clone(),
                });
            }
            Ok(marks)
        })
        .await;
        let marks = match captured {
            Ok(Ok(marks)) => marks,
            Ok(Err(e)) => return Response::error(ErrorCode::Invalid, e.to_string()),
            Err(e) => return Response::error(ErrorCode::Internal, e.to_string()),
        };
        let mut state = lock(&self.state);
        if !state.registry.get(&id).is_some_and(|r| {
            r.status == AgentStatus::Running
                && r.project
                    .as_ref()
                    .is_some_and(|p| project::canonical(p.dir()) == observed_root)
        }) {
            return Response::error(
                ErrorCode::Forbidden,
                "agent stopped or changed checkout during observation",
            );
        }
        let existing = match state.store.document::<Vec<ReadMark>>("reads", id.as_str()) {
            Ok(existing) => existing.unwrap_or_default(),
            Err(e) => return Response::error(ErrorCode::Internal, e.to_string()),
        };
        let mut by_path: BTreeMap<PathBuf, ReadMark> =
            existing.into_iter().map(|m| (m.path.clone(), m)).collect();
        let paths = marks.iter().map(|m| m.path.clone()).collect();
        for mark in marks {
            if by_path.get(&mark.path).is_none_or(|old| old.at <= mark.at) {
                by_path.insert(mark.path.clone(), mark);
            }
        }
        if by_path.len() > 1000 {
            return Response::error(
                ErrorCode::Invalid,
                "read-set capacity exceeded; checkpoint and start a replacement session",
            );
        }
        let reads: Vec<_> = by_path.into_values().collect();
        if let Err(e) = state.store.put_document("reads", id.as_str(), &reads) {
            return Response::error(ErrorCode::Internal, e.to_string());
        }
        state.emit(EventKind::ReadsObserved { agent: id, paths });
        Response::Reads { reads }
    }

    pub(super) async fn stale(&self, reference: &str, paths: Vec<String>) -> Response {
        let (_, root, _) = match self.reader_checkout(reference) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let reads = match self.reads(reference) {
            Response::Reads { reads } => reads,
            e => return e,
        };
        let checked = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<StalePath>> {
            let selected: Vec<ReadMark> = if paths.is_empty() {
                reads
            } else {
                let mut selected = BTreeMap::new();
                for raw in paths {
                    let path = checkout_path(&root, &raw)?;
                    // The newest read covering a target wins. Re-reading a file
                    // can clear that target without refreshing an entire directory.
                    if let Some(mark) = reads
                        .iter()
                        .filter(|m| path.starts_with(&m.path))
                        .max_by_key(|m| m.at)
                    {
                        selected.insert(mark.path.clone(), mark.clone());
                    }
                    for mark in reads.iter().filter(|m| m.path.starts_with(&path)) {
                        selected.insert(mark.path.clone(), mark.clone());
                    }
                }
                selected.into_values().collect()
            };
            Ok(check_reads(&selected))
        })
        .await;
        match checked {
            Ok(Ok(stale)) => Response::Stale { stale },
            Ok(Err(e)) => Response::error(ErrorCode::Invalid, e.to_string()),
            Err(e) => Response::error(ErrorCode::Internal, e.to_string()),
        }
    }
}

pub(super) fn checkout_path(root: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    let raw = Path::new(raw);
    let path = project::canonical(&if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    });
    anyhow::ensure!(
        path.starts_with(root),
        "observation is outside the agent's physical checkout"
    );
    Ok(path)
}

pub(super) fn check_reads(reads: &[ReadMark]) -> Vec<StalePath> {
    reads
        .iter()
        .filter_map(|mark| match content::fingerprint(&mark.path) {
            Ok(current) if current == mark.version => None,
            Ok(current) => Some(StalePath {
                path: mark.path.clone(),
                observed: mark.version.clone(),
                current: Some(current),
                reason: "content changed since observation; reread before editing".into(),
            }),
            Err(e) => Some(StalePath {
                path: mark.path.clone(),
                observed: mark.version.clone(),
                current: None,
                reason: format!("cannot verify current content: {e}"),
            }),
        })
        .collect()
}

impl State {
    pub(super) fn warn_readers(&mut self, change: &Change, physical: Option<&Path>) {
        let Some(checkout) = &change.checkout else {
            return;
        };
        let absolute = physical
            .map(Path::to_path_buf)
            .unwrap_or_else(|| checkout.join(&change.path));
        let agents: Vec<_> = self
            .registry
            .list(false)
            .iter()
            .filter(|r| {
                r.project.as_ref().is_some_and(|p| p.dir() == checkout)
                    && change.by.agent() != Some(&r.id)
            })
            .map(|r| r.id.clone())
            .collect();
        for agent in agents {
            let reads = match self
                .store
                .document::<Vec<ReadMark>>("reads", agent.as_str())
            {
                Ok(Some(reads)) => reads,
                _ => continue,
            };
            if !reads
                .iter()
                .any(|m| absolute.starts_with(&m.path) && m.at < change.at)
            {
                continue;
            }
            // The live event says so at once; the inbox notice waits for
            // the tick, so one switch of a branch is one message.
            self.emit(EventKind::AgentStale {
                agent: agent.clone(),
                paths: vec![absolute.clone()],
            });
            let pending = self.pending_stale.entry(agent).or_default();
            if pending.len() < PENDING_STALE_PATHS || pending.contains_key(&absolute) {
                pending.insert(absolute.clone(), change.clone());
            }
        }
    }

    /// One `stale` message per reader for everything that changed since the
    /// last tick, unless the reader still has the last one queued: then
    /// the paths wait, and the notice sent once that one has been
    /// acknowledged names them all (a queued envelope is never changed).
    /// The notice is kept under [`NOTICE_BYTES`], naming fewer paths and
    /// changes when they do not fit, down to the count alone. A message
    /// that still cannot be queued (a full inbox, a storage failure) is
    /// dropped with its paths; `check_stale` reads content and hooks
    /// refuse a stale edit, whether or not a notice arrived.
    pub(super) fn flush_notices(&mut self) {
        if self.fenced() {
            // Nothing can be queued while fenced; what waits keeps waiting
            // for the successor's first tick.
            return;
        }
        self.stale_outstanding.retain(|agent, message| {
            self.inboxes
                .get(agent)
                .is_some_and(|queue| queue.iter().any(|m| m.id == *message))
        });
        let ready: Vec<AgentId> = self
            .pending_stale
            .keys()
            .filter(|agent| !self.stale_outstanding.contains_key(*agent))
            .cloned()
            .collect();
        for agent in ready {
            let Some(pending) = self.pending_stale.remove(&agent) else {
                continue;
            };
            if self
                .registry
                .get(&agent)
                .is_none_or(|r| !r.status.is_live())
            {
                continue;
            }
            let count = pending.len();
            let mut changes: Vec<&Change> = pending.values().collect();
            changes.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.seq.cmp(&a.seq)));
            let mut list_paths = LISTED_STALE_PATHS;
            let mut list_changes = LISTED_STALE_CHANGES;
            let payload = loop {
                let paths: Vec<&PathBuf> = pending.keys().take(list_paths).collect();
                let listed = paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let text = if count == 1 && !paths.is_empty() {
                    format!(
                        "{listed} changed after your observation. Check current content and reread before editing. Attribution is best-effort."
                    )
                } else if paths.is_empty() {
                    format!(
                        "{count} paths changed after your observation. Check current content and reread before editing (`stale` names them). Attribution is best-effort."
                    )
                } else {
                    format!(
                        "{count} paths changed after your observation: {listed}{}. Check current content and reread before editing. Attribution is best-effort.",
                        if count > paths.len() {
                            format!(" (+{} more)", count - paths.len())
                        } else {
                            String::new()
                        }
                    )
                };
                let payload = json!({
                    "text": text, "paths": paths, "count": count,
                    "changes": &changes[..list_changes.min(changes.len())],
                });
                let fits = serde_json::to_vec(&payload).is_ok_and(|b| b.len() <= NOTICE_BYTES);
                if fits || (list_paths == 0 && list_changes == 0) {
                    break payload;
                }
                list_changes /= 2;
                list_paths /= 2;
            };
            let response = self.send(
                "agentd".into(),
                Destination::Agent(agent.clone()),
                "stale".into(),
                payload,
                None,
            );
            if let Response::Sent { message, .. } = response {
                self.stale_outstanding.insert(agent, message);
            }
        }
        self.flush_contested();
    }
}

/// Distinct paths kept per reader between ticks; beyond this nothing more
/// is tracked, the notice's `count` is what was, and a reader that never
/// drains its inbox finds the rest by `check_stale`.
const PENDING_STALE_PATHS: usize = 10_000;
/// Paths named in one notice, and changes carried with it, at most; a
/// notice is kept under this many bytes by naming fewer.
const LISTED_STALE_PATHS: usize = 200;
const LISTED_STALE_CHANGES: usize = 50;
const NOTICE_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    async fn setup(home: &Path, checkout: &Path, name: &str) -> Arc<Daemon> {
        let daemon = Arc::new(Daemon::open(home.into(), home.join("sock")).unwrap());
        daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.into(),
                    workdir: Some(checkout.into()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await;
        daemon
    }
    #[tokio::test]
    async fn content_check_catches_unwatched_changes_retries_and_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("state");
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let daemon = setup(&home, &root, "reader").await;
        assert!(matches!(
            daemon.observe("reader", vec!["file".into()]).await,
            Response::Reads { .. }
        ));
        std::fs::write(root.join("file"), "two").unwrap();
        // No watcher event is needed, and repeated checks cannot bypass it.
        for _ in 0..2 {
            assert!(
                matches!(daemon.stale("reader", vec!["file".into()]).await, Response::Stale { stale } if stale.len() == 1)
            );
        }
        drop(daemon);
        let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
        assert!(
            matches!(daemon.stale("reader", vec![]).await, Response::Stale { stale } if stale.len() == 1)
        );
        daemon.observe("reader", vec!["file".into()]).await;
        assert!(
            matches!(daemon.stale("reader", vec![]).await, Response::Stale { stale } if stale.is_empty())
        );
        assert!(matches!(
            daemon.observe("reader", vec!["../outside".into()]).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }
    #[tokio::test]
    async fn rereading_specific_file_shadows_old_directory_for_that_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let daemon = setup(&tmp.path().join("state"), &root, "reader").await;
        daemon.observe("reader", vec![".".into()]).await;
        std::fs::write(root.join("file"), "two").unwrap();
        daemon.observe("reader", vec!["file".into()]).await;
        assert!(
            matches!(daemon.stale("reader", vec!["file".into()]).await, Response::Stale { stale } if stale.is_empty())
        );
        assert!(
            matches!(daemon.stale("reader", vec![".".into()]).await, Response::Stale { stale } if !stale.is_empty())
        );
    }

    #[tokio::test]
    async fn separate_checkouts_do_not_stale_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("file"), "one").unwrap();
        std::fs::write(second.join("file"), "one").unwrap();
        let daemon = setup(&tmp.path().join("state"), &first, "reader").await;
        daemon.observe("reader", vec!["file".into()]).await;
        std::fs::write(second.join("file"), "two").unwrap();
        assert!(
            matches!(daemon.stale("reader", vec![]).await, Response::Stale { stale } if stale.is_empty())
        );
    }
}

#[cfg(test)]
mod warning_tests {
    use super::*;
    #[tokio::test]
    async fn watcher_warns_only_readers_of_the_changed_physical_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "one").unwrap();
        let root = project::canonical(&root);
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        let reader = match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "reader".into(),
                    workdir: Some(root.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        };
        daemon.observe("reader", vec!["file".into()]).await;
        daemon
            .record_fs_changes(
                vec![Observed {
                    checkout: Checkout {
                        dir: root,
                        project: reader.project.unwrap().id(),
                        worktree: None,
                    },
                    path: "file".into(),
                    kind: agentdocker_core::ChangeKind::Modified,
                }],
                vec![],
            )
            .await;
        daemon.flush_notices();
        let Response::Messages { messages } = daemon
            .handle(Request::Inbox {
                agent: "reader".into(),
                drain: false,
            })
            .await
        else {
            panic!()
        };
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind, "stale");
    }

    /// A branch switch is hundreds of changes in a moment. The reader gets
    /// one notice naming the paths, and nothing more until it has consumed
    /// that one; what changes in the meantime waits and is named by the
    /// next notice, so an unread notice is superseded, never followed.
    #[tokio::test]
    async fn stale_notices_are_one_per_tick_and_wait_for_the_last_to_be_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        for name in ["a", "b", "c", "d"] {
            std::fs::write(root.join(name), "one").unwrap();
        }
        let root = project::canonical(&root);
        let daemon =
            Arc::new(Daemon::open(tmp.path().join("state"), tmp.path().join("sock")).unwrap());
        let Response::Agent { agent: reader } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "reader".into(),
                    workdir: Some(root.clone()),
                    ..AgentSpec::default()
                },
                pid: None,
                session: None,
            })
            .await
        else {
            panic!("register failed");
        };
        let project = reader.project.unwrap().id();
        let checkout = Checkout {
            dir: root.clone(),
            project,
            worktree: None,
        };
        daemon.observe("reader", vec![".".into()]).await;
        let change = |path: &str, kind| Observed {
            checkout: checkout.clone(),
            path: path.into(),
            kind,
        };
        use agentdocker_core::ChangeKind::{Created, Modified, Removed};
        // Created, removed and modified for one path within the switch, and
        // two more paths: one notice.
        daemon
            .record_fs_changes(
                vec![
                    change("a", Created),
                    change("a", Removed),
                    change("a", Modified),
                    change("b", Modified),
                    change("c", Modified),
                ],
                vec![],
            )
            .await;
        let inbox = |drain: bool| {
            let daemon = daemon.clone();
            async move {
                match daemon
                    .handle(Request::Inbox {
                        agent: "reader".into(),
                        drain,
                    })
                    .await
                {
                    Response::Messages { messages } => messages,
                    other => panic!("{other:?}"),
                }
            }
        };
        assert!(inbox(false).await.is_empty(), "nothing before the tick");
        daemon.flush_notices();
        let first = inbox(false).await;
        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(first[0].kind, "stale");
        let canonical = |name: &str| agentdocker_host::project::canonical(&root.join(name));
        assert_eq!(
            first[0].payload["paths"],
            json!([canonical("a"), canonical("b"), canonical("c")])
        );
        assert_eq!(first[0].payload["count"], json!(3));
        assert_eq!(
            first[0].payload["changes"].as_array().unwrap().len(),
            3,
            "the last change per path travels with the notice"
        );
        assert!(
            first[0].payload["text"]
                .as_str()
                .unwrap()
                .starts_with("3 paths changed after your observation: ")
        );
        // Unread: a further change waits rather than queueing another.
        daemon
            .record_fs_changes(vec![change("d", Modified)], vec![])
            .await;
        daemon.flush_notices();
        daemon.flush_notices();
        assert_eq!(inbox(false).await.len(), 1, "still the one notice");
        // Consumed: the next tick names what waited.
        let drained = inbox(true).await;
        assert_eq!(drained.len(), 1);
        daemon.flush_notices();
        let second = inbox(true).await;
        assert_eq!(second.len(), 1, "{second:?}");
        assert_eq!(second[0].payload["paths"], json!([canonical("d")]));
        assert_eq!(
            second[0].payload["text"],
            json!(format!(
                "{} changed after your observation. Check current content and reread before editing. Attribution is best-effort.",
                canonical("d").display()
            ))
        );
        daemon.flush_notices();
        assert!(
            inbox(false).await.is_empty(),
            "nothing pending, nothing sent"
        );
        // Fenced: the tick sends nothing and forgets nothing; the notice
        // arrives once authority is back.
        daemon
            .record_fs_changes(vec![change("b", Modified)], vec![])
            .await;
        daemon.offer_transfer(1).unwrap();
        daemon.flush_notices();
        {
            let state = lock(&daemon.state);
            assert!(
                state.pending_stale.values().any(|p| !p.is_empty()),
                "still pending while fenced"
            );
            assert!(
                state.stale_outstanding.is_empty(),
                "nothing sent while fenced"
            );
        }
        assert!(daemon.abort_transfer("cleanup"));
        daemon.flush_notices();
        let after = inbox(true).await;
        assert_eq!(after.len(), 1, "{after:?}");
        assert_eq!(after[0].payload["paths"], json!([canonical("b")]));
        // A change to very many paths with long names still fits one
        // notice: fewer are named, the count stays exact.
        // Names near the filesystem's limit, in nested directories.
        let long: Vec<String> = (0..400)
            .map(|n| format!("{0}/{0}/{0}-{n}", "d".repeat(200)))
            .collect();
        for name in &long {
            std::fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
            std::fs::write(root.join(name), "one").unwrap();
        }
        daemon.observe("reader", vec![".".into()]).await;
        daemon
            .record_fs_changes(
                long.iter().map(|name| change(name, Modified)).collect(),
                vec![],
            )
            .await;
        daemon.flush_notices();
        let big = inbox(true).await;
        assert_eq!(big.len(), 1, "{}", big.len());
        assert_eq!(big[0].payload["count"], json!(400));
        let named = big[0].payload["paths"].as_array().unwrap().len();
        assert!(named > 0 && named < 200, "named {named}");
        assert!(serde_json::to_vec(&big[0].payload).unwrap().len() <= NOTICE_BYTES);
    }
}
