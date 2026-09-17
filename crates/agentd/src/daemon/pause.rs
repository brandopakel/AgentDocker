//! A project told to hold. The person says why; every live agent in the
//! project gets that as a `pause` message, the daemon refuses their new
//! leases until the person lifts it, and the app and `ps` say so. One
//! record per project, kept as a document so a restart remembers it.
use super::*;
use agentdocker_core::{HUMAN, Pause};

const DOCUMENT: &str = "pause";

impl Daemon {
    pub(super) async fn pause(
        self: &Arc<Self>,
        from: String,
        project: Option<String>,
        reason: String,
    ) -> Response {
        let reason = reason.trim().to_owned();
        if reason.is_empty() {
            return Response::error(
                ErrorCode::Invalid,
                "a pause needs a reason the agents can read",
            );
        }
        let (from, project) = match self.pause_scope(from, project).await {
            Ok(pair) => pair,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        let pause = Pause {
            project: project.clone(),
            by: from.clone(),
            reason: reason.clone(),
            at: Utc::now(),
        };
        let mut event = Event::new(
            EventKind::ProjectPaused {
                project: project.clone(),
                by: from.clone(),
                reason: reason.clone(),
            },
            pause.at,
        );
        event.seq = state.next_seq;
        if state.persist("project pause", |store| {
            store.put_document_with_event(DOCUMENT, project.as_str(), &pause, &event)
        }) != Persisted::Committed
        {
            return state.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the pause was not recorded")
            });
        }
        state.pauses.insert(project.clone(), pause.clone());
        state.next_seq += 1;
        let _ = state.events.send(event);
        // The words reach every live agent in the project, and the
        // archive keeps them under #everyone.
        state.send(
            from,
            Destination::Project(project),
            "pause".to_owned(),
            serde_json::json!({ "text": format!("Pause: {reason}"), "reason": reason }),
            None,
        );
        Response::Pause { pause }
    }

    pub(super) async fn resume_project(
        self: &Arc<Self>,
        from: String,
        project: Option<String>,
    ) -> Response {
        let (from, project) = match self.pause_scope(from, project).await {
            Ok(pair) => pair,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        if !state.pauses.contains_key(&project) {
            return Response::Ok;
        }
        let now = Utc::now();
        let mut event = Event::new(
            EventKind::ProjectResumed {
                project: project.clone(),
                by: from.clone(),
            },
            now,
        );
        event.seq = state.next_seq;
        if state.persist("project resume", |store| {
            store.delete_documents_with_event(
                DOCUMENT,
                std::slice::from_ref(&project.as_str().to_owned()),
                &event,
            )
        }) != Persisted::Committed
        {
            return state.write_failure().unwrap_or_else(|| {
                Response::error(ErrorCode::Internal, "the resume was not recorded")
            });
        }
        state.pauses.remove(&project);
        state.next_seq += 1;
        let _ = state.events.send(event);
        state.send(
            from,
            Destination::Project(project),
            "resume".to_owned(),
            serde_json::json!({ "text": "Resume: carry on." }),
            None,
        );
        Response::Ok
    }

    pub(super) fn pauses(&self) -> Response {
        let mut pauses: Vec<Pause> = lock(&self.state).pauses.values().cloned().collect();
        pauses.sort_by_key(|p| p.at);
        Response::Pauses { pauses }
    }

    /// Who is pausing, and which project: the caller's own when none is
    /// named — a person in the app names the one they are looking at.
    async fn pause_scope(
        &self,
        from: String,
        project: Option<String>,
    ) -> Result<(String, ProjectId), Box<Response>> {
        let project = match project {
            Some(selector) => self.resolve_project(&selector).await?,
            None => {
                let mut state = lock(&self.state);
                let id = state.resolve(&from)?;
                state
                    .registry
                    .get(&id)
                    .and_then(|a| a.project.as_ref().map(ProjectRef::id))
                    .ok_or_else(|| {
                        Box::new(Response::error(
                            ErrorCode::Invalid,
                            "the caller is in no project; name one",
                        ))
                    })?
            }
        };
        let from = {
            let state = lock(&self.state);
            match state.registry.resolve(&from) {
                Ok(id) => id.to_string(),
                Err(_) if from == HUMAN => from,
                Err(err) => return Err(Box::new(registry_error(err))),
            }
        };
        Ok((from, project))
    }
}

impl State {
    /// The pause that holds this agent back from a new lease, if any: the
    /// project's agents are held, the person is not.
    pub(super) fn pause_holding(&self, agent: &AgentId) -> Option<&Pause> {
        let record = self.registry.get(agent)?;
        let project = record.project.as_ref().map(ProjectRef::id);
        let human = super::humans::is_human(record);
        self.pauses
            .values()
            .find(|pause| pause.holds(project.as_ref(), human))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    async fn register(daemon: &Arc<Daemon>, name: &str, workdir: &std::path::Path) -> AgentRecord {
        match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.to_owned(),
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

    /// A pause reaches every live agent in the project as a `pause`
    /// message with the reason, refuses their new leases with `paused`
    /// and the reason, leaves the person and another project's agents
    /// free, is listed, survives a reopen, and lifts on resume with a
    /// `resume` message; resuming a project that is not paused is `ok`.
    #[tokio::test]
    async fn a_paused_project_holds_its_agents_until_the_person_lifts_it() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let here = dir.path().join("here");
        let there = dir.path().join("there");
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        let alice = register(&daemon, "alice", &here).await;
        let bob = register(&daemon, "bob", &here).await;
        let carol = register(&daemon, "carol", &there).await;
        let Response::Agent { agent: person } = daemon
            .handle(Request::Me {
                workdir: Some(here.clone()),
            })
            .await
        else {
            panic!("the person")
        };
        let project = alice.project.clone().unwrap().id();
        let claim = |daemon: &Arc<Daemon>, agent: &str| {
            let agent = agent.to_owned();
            let daemon = daemon.clone();
            async move {
                daemon
                    .handle(Request::Claim {
                        agent: agent.clone(),
                        resource: format!("task:{agent}"),
                        mode: agentdocker_core::LeaseMode::Exclusive,
                        amount: None,
                        ttl_secs: 60,
                        note: None,
                        wait_secs: 0,
                    })
                    .await
            }
        };
        assert!(matches!(
            claim(&daemon, "alice").await,
            Response::Lease { .. }
        ));
        assert!(matches!(
            daemon
                .handle(Request::Pause {
                    from: HUMAN.to_owned(),
                    project: Some(here.display().to_string()),
                    reason: "  ".to_owned(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let Response::Pause { pause } = daemon
            .handle(Request::Pause {
                from: HUMAN.to_owned(),
                project: Some(here.display().to_string()),
                reason: "sleeping the laptop".to_owned(),
            })
            .await
        else {
            panic!("paused")
        };
        assert_eq!(pause.project, project);
        assert_eq!(pause.by, person.id.to_string());
        assert_eq!(pause.reason, "sleeping the laptop");
        // Held: a new lease is refused with the reason; the one held
        // stays held.
        match claim(&daemon, "bob").await {
            Response::Error {
                code: ErrorCode::Paused,
                details,
                ..
            } => assert_eq!(details.unwrap()["reason"], "sleeping the laptop"),
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(claim(&daemon, "carol").await, Response::Lease { .. }),
            "another project is not paused"
        );
        assert!(
            matches!(
                claim(&daemon, person.id.as_str()).await,
                Response::Lease { .. }
            ),
            "the person is not held"
        );
        // The words reached both agents here and nobody there.
        for agent in [&alice, &bob] {
            let Response::Messages { messages } = daemon
                .handle(Request::Inbox {
                    agent: agent.id.to_string(),
                    drain: false,
                })
                .await
            else {
                panic!("inbox")
            };
            let pause_message = messages
                .iter()
                .find(|m| m.kind == "pause")
                .unwrap_or_else(|| panic!("{} was told", agent.spec.name));
            assert_eq!(pause_message.payload["reason"], "sleeping the laptop");
        }
        let Response::Messages { messages } = daemon
            .handle(Request::Inbox {
                agent: carol.id.to_string(),
                drain: false,
            })
            .await
        else {
            panic!("inbox")
        };
        assert!(!messages.iter().any(|m| m.kind == "pause"));
        // Listed, and still there after a reopen.
        let Response::Pauses { pauses } = daemon.handle(Request::Pauses).await else {
            panic!("pauses")
        };
        assert_eq!(pauses, vec![pause.clone()]);
        drop(daemon);
        let daemon = open(&dir);
        let Response::Pauses { pauses } = daemon.handle(Request::Pauses).await else {
            panic!("pauses")
        };
        assert_eq!(pauses, vec![pause]);
        assert!(matches!(
            claim(&daemon, "bob").await,
            Response::Error {
                code: ErrorCode::Paused,
                ..
            }
        ));
        // Lifted.
        assert!(matches!(
            daemon
                .handle(Request::ResumeProject {
                    from: HUMAN.to_owned(),
                    project: Some(here.display().to_string()),
                })
                .await,
            Response::Ok
        ));
        assert!(matches!(
            claim(&daemon, "bob").await,
            Response::Lease { .. }
        ));
        let Response::Pauses { pauses } = daemon.handle(Request::Pauses).await else {
            panic!("pauses")
        };
        assert!(pauses.is_empty());
        let Response::Messages { messages } = daemon
            .handle(Request::Inbox {
                agent: bob.id.to_string(),
                drain: false,
            })
            .await
        else {
            panic!("inbox")
        };
        assert!(messages.iter().any(|m| m.kind == "resume"));
        assert!(
            matches!(
                daemon
                    .handle(Request::ResumeProject {
                        from: HUMAN.to_owned(),
                        project: Some(here.display().to_string()),
                    })
                    .await,
                Response::Ok
            ),
            "resuming twice is nothing"
        );
        assert!(
            daemon.recent_events(20).iter().any(|e| matches!(&e.kind,
                EventKind::ProjectPaused { project: p, reason, .. } if *p == project && reason == "sleeping the laptop"))
                && daemon.recent_events(20).iter().any(|e| matches!(&e.kind,
                EventKind::ProjectResumed { project: p, .. } if *p == project)),
            "both announced"
        );
    }
}
