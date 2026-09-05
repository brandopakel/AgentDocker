//! Content observations are captured outside the state lock, then persisted
//! under it. Host timestamps describe the beginning of the observation.
use super::*;
use agentdocker_core::{ReadMark, StalePath};
use agentdocker_host::content;

impl Daemon {
    fn reader_checkout(
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
    pub(super) fn warn_readers(&mut self, change: &Change) {
        let Some(checkout) = &change.checkout else {
            return;
        };
        let absolute = checkout.join(&change.path);
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
            let paths = vec![absolute.clone()];
            self.emit(EventKind::AgentStale {
                agent: agent.clone(),
                paths: paths.clone(),
            });
            self.send("agentd".into(), Destination::Agent(agent.clone()), "stale".into(), json!({
                "text": format!("{} changed after your observation. Check current content and reread before editing. Attribution is best-effort.", absolute.display()),
                "paths": paths, "change": change,
            }), None);
        }
    }
}

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
}
