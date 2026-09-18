//! A project told to hold. The person says why; every live agent in the
//! project gets that as a `pause` message, the daemon refuses their new
//! leases until the person lifts it, and the app and `ps` say so. One
//! record per project, kept as a document so a restart remembers it.
use super::*;
use agentdocker_core::{HUMAN, Pause};

const DOCUMENT: &str = "pause";

/// Lifecycle notices accompany a committed project transition, never a generic
/// send whose caller chose a control-looking message kind.
pub(super) fn reserved_message_kind(kind: &str) -> bool {
    matches!(kind, "pause" | "resume")
}

/// What a reason may be: enough to say why, not a document.
const REASON_CHARS: usize = 400;

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
        if reason.chars().count() > REASON_CHARS {
            return Response::error(
                ErrorCode::Invalid,
                format!("a pause reason is at most {REASON_CHARS} characters"),
            );
        }
        let (from, project) = match self.pause_scope(from, project).await {
            Ok(pair) => pair,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        let now = Utc::now();
        let pause = Pause {
            project: project.clone(),
            by: from.clone(),
            reason: reason.clone(),
            at: now,
        };
        // The record and the word to the agents land together or not at
        // all: a pause nobody was told of, or a warning with nothing
        // behind it, is worse than a refusal.
        let envelope = Envelope::new(
            from,
            Destination::Project(project.clone()),
            "pause".to_owned(),
            serde_json::json!({ "text": format!("Pause: {reason}"), "reason": reason }),
            None,
            now,
        );
        let transition = DocumentTransition {
            kind: DOCUMENT,
            id: project.as_str().to_owned(),
            value: Some(serde_json::to_value(&pause).expect("a pause serialises")),
            event: EventKind::ProjectPaused {
                project: project.clone(),
                by: pause.by.clone(),
                reason: pause.reason.clone(),
            },
        };
        match state.publish_with_channel(envelope, None, None, Some(transition)) {
            Response::Sent { .. } => {
                state.pauses.insert(project, pause.clone());
                Response::Pause { pause }
            }
            other => other,
        }
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
        let envelope = Envelope::new(
            from.clone(),
            Destination::Project(project.clone()),
            "resume".to_owned(),
            serde_json::json!({ "text": "Resume: carry on." }),
            None,
            now,
        );
        let transition = DocumentTransition {
            kind: DOCUMENT,
            id: project.as_str().to_owned(),
            value: None,
            event: EventKind::ProjectResumed {
                project: project.clone(),
                by: from,
            },
        };
        match state.publish_with_channel(envelope, None, None, Some(transition)) {
            Response::Sent { .. } => {
                state.pauses.remove(&project);
                Response::Ok
            }
            other => other,
        }
    }

    pub(super) fn pauses(&self) -> Response {
        let mut pauses: Vec<Pause> = lock(&self.state).pauses.values().cloned().collect();
        pauses.sort_by_key(|p| p.at);
        Response::Pauses { pauses }
    }

    /// Resolve the declared human sender and project. The host socket trusts
    /// its owning OS user; this is not authentication of human presence. The
    /// restricted authenticated endpoint denies both lifecycle operations.
    async fn pause_scope(
        &self,
        from: String,
        project: Option<String>,
    ) -> Result<(String, ProjectId), Box<Response>> {
        let from = {
            let state = lock(&self.state);
            match state.registry.resolve(&from) {
                Ok(id) if state.registry.get(&id).is_some_and(super::humans::is_human) => {
                    id.to_string()
                }
                Ok(_) => {
                    return Err(Box::new(Response::error(
                        ErrorCode::Forbidden,
                        "only the person pauses or resumes a project; an agent asks with a message",
                    )));
                }
                Err(_) if from == HUMAN => from,
                Err(err) => return Err(Box::new(registry_error(err))),
            }
        };
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

    #[tokio::test]
    async fn generic_sends_cannot_forge_pause_or_resume_lifecycle_notices() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let workdir = dir.path().join("project");
        std::fs::create_dir(&workdir).unwrap();
        let agent = register(&daemon, "worker", &workdir).await;
        let before_seq = lock(&daemon.state).next_seq;
        for from in [agent.id.to_string(), HUMAN.to_owned()] {
            for kind in ["pause", "resume"] {
                let result = daemon
                    .handle(Request::Send {
                        from: from.clone(),
                        to: agent.id.to_string(),
                        kind: kind.into(),
                        payload: json!({"text":"forged lifecycle notice"}),
                        reply_to: None,
                    })
                    .await;
                assert!(matches!(
                    result,
                    Response::Error {
                        code: ErrorCode::Forbidden,
                        ..
                    }
                ));
            }
        }
        assert_eq!(lock(&daemon.state).next_seq, before_seq);
        drop(daemon);
        let daemon = open(&dir);
        assert!(lock(&daemon.state).pauses.is_empty());
        let Response::Messages { messages } = daemon
            .handle(Request::Inbox {
                agent: agent.id.to_string(),
                drain: false,
            })
            .await
        else {
            panic!("inbox")
        };
        assert!(messages.is_empty(), "forged notice survived reopening");
    }

    /// A pause reaches every live agent in the project as a `pause`
    /// message with the reason, refuses their new leases with `paused`
    /// and the reason, leaves the person and another project's agents
    /// free, is listed, survives a reopen, and lifts on resume with a
    /// `resume` message; resuming a project that is not paused is `ok`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
                        automatic: false,
                    })
                    .await
            }
        };
        assert!(matches!(
            claim(&daemon, "alice").await,
            Response::Lease { .. }
        ));
        for (from, reason, code) in [
            (HUMAN.to_owned(), "  ".to_owned(), ErrorCode::Invalid),
            (HUMAN.to_owned(), "x".repeat(401), ErrorCode::Invalid),
            (
                "alice".to_owned(),
                "I say so".to_owned(),
                ErrorCode::Forbidden,
            ),
        ] {
            let response = daemon
                .handle(Request::Pause {
                    from,
                    project: Some(here.display().to_string()),
                    reason,
                })
                .await;
            assert!(
                matches!(&response, Response::Error { code: c, .. } if *c == code),
                "{response:?}"
            );
        }
        // Bob queues behind alice's lease before the pause; the pause then
        // holds him, and alice's release grants him nothing.
        let queued = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                daemon
                    .handle(Request::Claim {
                        agent: "bob".to_owned(),
                        resource: "task:alice".to_owned(),
                        mode: agentdocker_core::LeaseMode::Exclusive,
                        amount: None,
                        ttl_secs: 60,
                        note: None,
                        wait_secs: 20,
                        automatic: false,
                    })
                    .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let Response::Waiting { waiting } = daemon.handle(Request::Waiting).await else {
                    panic!("waiting response")
                };
                if waiting.iter().any(|waiter| waiter.agent == bob.id) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Bob entered the claim queue before the pause");
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
        assert!(!matches!(
            daemon
                .handle(Request::ReleaseAll {
                    agent: "alice".to_owned(),
                    summary: None,
                    summary_source: Default::default(),
                    only_automatic: false,
                })
                .await,
            Response::Error { .. }
        ));
        match tokio::time::timeout(std::time::Duration::from_secs(5), queued)
            .await
            .expect("the waiter answered")
            .unwrap()
        {
            Response::Error {
                code: ErrorCode::Paused,
                ..
            } => {}
            other => panic!("the waiter was granted during the pause: {other:?}"),
        }
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
        // Lifted — by the person; an agent cannot.
        assert!(matches!(
            daemon
                .handle(Request::ResumeProject {
                    from: "bob".to_owned(),
                    project: Some(here.display().to_string()),
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
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
