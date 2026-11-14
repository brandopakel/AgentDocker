//! `brief`: where the work stands in a project, from the daemon's own
//! records ([`agentdocker_core::brief`]). The daemon decides the reader and
//! its project and gathers that project's records; core chooses what the
//! reader is told. An agent reads its own project only; the person names
//! one, and `person_refusal` has already refused that word from inside an
//! agent's session.

use super::*;
use agentdocker_core::brief::{CardRecord, Reader, Records, compose};
use agentdocker_core::{Checkpoint, HUMAN, Task};

impl Daemon {
    pub(super) async fn work_brief(&self, from: &str, project: Option<&str>) -> Response {
        let person = from == HUMAN || {
            let state = lock(&self.state);
            state
                .registry
                .resolve(from)
                .ok()
                .and_then(|id| state.registry.get(&id))
                .is_some_and(humans::is_human)
        };
        let (reader, own) = if person {
            (Reader::Person, None)
        } else {
            let mut state = lock(&self.state);
            let id = match state.resolve(from) {
                Ok(id) => id,
                Err(response) => return *response,
            };
            let record = state.registry.get(&id).expect("resolved");
            let Some(own) = record.project.as_ref().map(ProjectRef::id) else {
                return Response::error(
                    ErrorCode::Invalid,
                    "this agent works in no project, so it has no brief",
                );
            };
            let reader = Reader::Agent {
                id: id.clone(),
                name: record.spec.name.clone(),
                roles: record.roles().into_iter().map(str::to_owned).collect(),
                checkout: None,
            };
            (reader, Some(own))
        };
        // An agent's brief is of its own project: one naming another is
        // refused, never answered with the other's records.
        let named = match project {
            Some(selector) => match self.resolve_project(selector).await {
                Ok(id) => Some(id),
                Err(response) => return *response,
            },
            None => None,
        };
        let project = match (own, named) {
            (Some(own), Some(named)) if named != own => {
                return Response::error(
                    ErrorCode::Forbidden,
                    "an agent's brief is of its own project",
                );
            }
            (Some(own), _) => own,
            (None, Some(named)) => named,
            (None, None) => {
                return Response::error(
                    ErrorCode::Invalid,
                    "name the project the brief is of (--project <path or id>)",
                );
            }
        };
        // Where a checkpoint can be resumed from: the checkout `resume`
        // compares it with.
        let reader = match reader {
            Reader::Agent {
                id, name, roles, ..
            } => Reader::Agent {
                checkout: self
                    .reader_checkout(id.as_str())
                    .ok()
                    .map(|(_, checkout, _)| checkout),
                id,
                name,
                roles,
            },
            Reader::Person => Reader::Person,
        };
        match self.brief_records(&project) {
            Ok(records) => Response::Brief {
                brief: compose(&records, &reader),
            },
            Err(response) => *response,
        }
    }

    /// The records of `project` a brief is chosen from: its agents, live
    /// and ended, and any other record holding one of its open cards; its
    /// open cards with their lanes and newest comments; the checkpoints,
    /// questions and unexpired leases core sorts by project.
    fn brief_records(&self, project: &ProjectId) -> Result<Records, Box<Response>> {
        let now = Utc::now();
        let mut state = lock(&self.state);
        let ours = |a: &AgentRecord| a.project.as_ref().is_some_and(|p| &p.id() == project);
        let mut agents: Vec<AgentRecord> =
            state.registry.all().filter(|a| ours(a)).cloned().collect();
        let project_name = agents
            .iter()
            .find_map(|a| a.project.as_ref().map(ProjectRef::name))
            .unwrap_or_else(|| project.as_str().to_owned());
        let open: Vec<Task> = state
            .project_cards(project)?
            .into_iter()
            .filter(Task::open)
            .collect();
        for holder in open.iter().filter_map(|t| t.assignee.as_ref()) {
            if !agents.iter().any(|a| &a.id == holder)
                && let Some(record) = state.registry.get(holder)
            {
                agents.push(record.clone());
            }
        }
        let mut cards = Vec::with_capacity(open.len());
        for task in open {
            let lane = state
                .board_of(&task)
                .map(|board| board.lane_of(&task).name.clone())
                .unwrap_or_else(|_| task.column.label().to_owned());
            let newest = state
                .card_comments(&task.id)
                .into_iter()
                .rev()
                .find(|c| c.deleted_at.is_none());
            cards.push(CardRecord { task, lane, newest });
        }
        let checkpoints = state.read_store("checkpoint", |store| {
            store.documents::<Checkpoint>("checkpoint", None)
        })?;
        // Whom each handoff of a checkpoint is addressed to, by its id.
        let handoffs = state
            .read_store("handoff", |store| {
                store.documents::<agentdocker_core::handoff::HandoffBundle>("handoff", None)
            })?
            .into_iter()
            .filter_map(|bundle| bundle.to.map(|to| (bundle.id, to)))
            .collect();
        let mut checkouts = state
            .project_checkouts
            .get(project)
            .cloned()
            .unwrap_or_default();
        for agent in agents.iter().filter(|a| ours(a)) {
            if let Some(dir) = agent.project.as_ref().map(|p| p.dir().to_path_buf())
                && !checkouts.contains(&dir)
            {
                checkouts.push(dir);
            }
        }
        let questions = state.questions.values().cloned().collect();
        let leases = state
            .leases
            .all()
            .into_iter()
            .filter(|l| !l.is_expired(now))
            .cloned()
            .collect();
        Ok(Records {
            project: project.clone(),
            project_name,
            agents,
            cards,
            checkpoints,
            checkouts,
            questions,
            leases,
            handoffs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::brief::{Brief, Whose};
    use tempfile::TempDir;

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

    /// A card `holder` files in `project` and pulls; its key.
    async fn held_card(
        daemon: &Arc<Daemon>,
        holder: &AgentRecord,
        project: &str,
        title: &str,
    ) -> String {
        let task = match daemon
            .handle(Request::TaskCreate {
                from: holder.id.to_string(),
                project: Some(project.to_owned()),
                title: title.to_owned(),
                acceptance: String::new(),
                column: None,
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
        {
            Response::Task { task } => task,
            other => panic!("{other:?}"),
        };
        let pulled = daemon
            .handle(Request::TaskPull {
                agent: holder.id.to_string(),
                task: Some(task.id.to_string()),
                take_over_from: None,
                project: None,
                board: None,
            })
            .await;
        assert!(!matches!(pulled, Response::Error { .. }), "{pulled:?}");
        task.key
    }

    fn brief(response: Response) -> Brief {
        match response {
            Response::Brief { brief } => brief,
            other => panic!("{other:?}"),
        }
    }

    fn keys(brief: &Brief) -> Vec<(&Whose, Vec<&str>)> {
        brief
            .groups
            .iter()
            .map(|g| (&g.whose, g.cards.iter().map(|c| c.key.as_str()).collect()))
            .collect()
    }

    /// An agent reads its own project's brief: its own open card first,
    /// though it is live, then a card a session that ended still holds,
    /// under that session; never another project's, which naming refuses.
    /// The person reads the project it names, and must name one.
    #[tokio::test]
    async fn a_brief_is_of_the_readers_own_project() {
        let dir = TempDir::new().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().to_path_buf(), dir.path().join("sock")).unwrap());
        let work = dir.path().join("work");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let (work_dir, other_dir) = (work.display().to_string(), other.display().to_string());
        let me = register(&daemon, "alice", &work).await;
        let gone = register(&daemon, "bob", &work).await;
        let stranger = register(&daemon, "carol", &other).await;
        let mine = held_card(&daemon, &me, &work_dir, "mine").await;
        let left = held_card(&daemon, &gone, &work_dir, "left behind").await;
        let theirs = held_card(&daemon, &stranger, &other_dir, "theirs").await;
        assert!(matches!(
            daemon
                .handle(Request::Deregister {
                    agent: gone.id.to_string(),
                })
                .await,
            Response::Ok | Response::Agent { .. }
        ));

        let ask = |from: &str, project: Option<&str>| Request::Brief {
            from: from.to_owned(),
            project: project.map(str::to_owned),
        };
        let read = brief(daemon.handle(ask(me.id.as_str(), None)).await);
        let groups = keys(&read);
        assert!(
            matches!(groups[0], (Whose::Yours, ref k) if k == &[mine.as_str()]),
            "{groups:?}"
        );
        assert!(
            matches!(groups[1], (Whose::Left { name, .. }, ref k) if name == "bob" && k == &[left.as_str()]),
            "{groups:?}"
        );
        assert_eq!(groups.len(), 2, "never another project's card: {groups:?}");
        assert!(
            read.groups
                .iter()
                .flat_map(|g| &g.cards)
                .all(|c| c.key != theirs)
        );
        assert_eq!(
            brief(daemon.handle(ask(me.id.as_str(), Some(&work_dir))).await),
            read
        );
        assert!(matches!(
            daemon.handle(ask(me.id.as_str(), Some(&other_dir))).await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));

        let person = brief(daemon.handle(ask(HUMAN, Some(&other_dir))).await);
        assert!(matches!(person.reader, Reader::Person));
        // Carol is live: her card is counted, not listed.
        assert!(person.groups.is_empty(), "{:?}", keys(&person));
        assert_eq!(person.held_live.len(), 1);
        assert_eq!(
            (person.held_live[0].name.as_str(), person.held_live[0].cards),
            ("carol", 1)
        );
        assert!(matches!(
            daemon.handle(ask(HUMAN, None)).await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }
}
