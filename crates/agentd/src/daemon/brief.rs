//! `brief`: where the work stands in a project, from the daemon's own
//! records ([`agentdocker_core::brief`]). The daemon decides the reader and
//! its project and gathers that project's records; core chooses what the
//! reader is told. An agent reads its own project only; the person names
//! one, and `person_refusal` has already refused that word from inside an
//! agent's session.

use super::*;
use agentdocker_core::brief::{CardRecord, CheckoutRead, CheckoutRecord, Reader, Records, compose};
use agentdocker_core::gate::Gate;

/// How long a brief gives git, in all, to read checkouts nothing has read
/// yet: a session start waits for it.
const COLD_READ: std::time::Duration = std::time::Duration::from_millis(300);
use agentdocker_core::{Checkpoint, HUMAN, Task};

impl Daemon {
    pub(super) async fn work_brief(
        self: &Arc<Self>,
        from: &str,
        project: Option<&str>,
    ) -> Response {
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
        let (named, named_ref) = match project {
            Some(selector) => match self.resolve_project_ref(selector).await {
                Ok((id, found)) => (Some(id), found),
                Err(response) => return *response,
            },
            None => (None, None),
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
        // The project as its records and the name it was asked by give it:
        // the reader's own first, the one named, then live records before
        // ended ones.
        let refs: Vec<ProjectRef> = {
            let state = lock(&self.state);
            let own = match &reader {
                Reader::Agent { id, .. } => state.registry.get(id).and_then(|a| a.project.clone()),
                Reader::Person => None,
            };
            let mut records: Vec<&AgentRecord> = state
                .registry
                .all()
                .filter(|a| a.project.as_ref().is_some_and(|p| p.id() == project))
                .collect();
            records.sort_by_key(|a| !a.status.is_live());
            own.into_iter()
                .chain(named_ref.filter(|p| p.id() == project))
                .chain(records.into_iter().filter_map(|a| a.project.clone()))
                .collect()
        };
        // Where `agentdocker brief` asks GitHub: the reader's checkout, else
        // the one named, else the project's first known one.
        let repository = match &reader {
            Reader::Agent {
                checkout: Some(checkout),
                ..
            } => Some(checkout.clone()),
            _ => refs
                .iter()
                .find(|p| p.source == ProjectSource::Git)
                .map(|p| p.dir().to_path_buf()),
        };
        let deadline = std::time::Instant::now() + self.cold_read();
        let unlisted = self.list_project_checkouts(&project, &refs, deadline).await;
        // Checkouts nothing has read yet are read now, in what is left of
        // the time; those it does not reach are said to be unread.
        let unread: Vec<PathBuf> = {
            let state = lock(&self.state);
            state
                .project_checkout_dirs(&project)
                .into_iter()
                .filter(|dir| !state.checkout_reads.contains_key(dir))
                .collect()
        };
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        self.read_standings(&project, unread, left).await;
        let mut records = match self.brief_records(&project) {
            Ok(records) => records,
            Err(response) => return *response,
        };
        records.repository = repository;
        records.checkouts_unlisted = unlisted;
        // The gate's word at each pushed head, as `agentdocker gate` would
        // give it for that branch and commit.
        let pushed: Vec<(String, String)> = records
            .checkout_reads
            .iter()
            .filter_map(|c| match &c.read {
                CheckoutRead::Read { standing, .. } if standing.stands() => {
                    Some((standing.branch.clone()?, standing.pushed.clone()?))
                }
                _ => None,
            })
            .collect();
        for (branch, head) in pushed {
            if let Response::ReviewGate { gate, .. } = self
                .review_gate(
                    None,
                    Some(project.as_str().to_owned()),
                    &branch,
                    &head,
                    None,
                )
                .await
            {
                let word = match gate {
                    Gate::Approved { .. } => "approved",
                    Gate::Changes { .. } => "changes requested",
                    Gate::Stale { .. } => "stale (a pass of an earlier commit)",
                    Gate::Missing => "no verdict",
                };
                records.gates.push((branch, head, word.to_owned()));
            }
        }
        Response::Brief {
            brief: compose(&records, &reader),
        }
    }

    /// How long a brief gives git, in all, to list and read checkouts
    /// nothing has read yet: [`COLD_READ`], or what a test set.
    fn cold_read(&self) -> std::time::Duration {
        #[cfg(test)]
        if let Some(bound) = *lock(&self.cold_read) {
            return bound;
        }
        COLD_READ
    }

    /// Bound a brief's cold reads in a test: a long bound where the test
    /// is about what a read finds, none where it is about what a brief
    /// says of a checkout it did not reach.
    #[cfg(test)]
    pub(crate) fn set_cold_read_for_test(&self, bound: std::time::Duration) {
        *lock(&self.cold_read) = Some(bound);
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
        let checkout_reads = state
            .project_checkout_dirs(project)
            .into_iter()
            .map(|path| CheckoutRecord {
                read: state
                    .checkout_reads
                    .get(&path)
                    .filter(|(p, _)| p == project)
                    .map_or(CheckoutRead::Unread, |(_, read)| read.clone()),
                path,
            })
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
            checkout_reads,
            gates: Vec::new(),
            checkouts_unlisted: None,
            repository: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::brief::{Brief, Whose, render};
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
                unblock: None,
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

    /// A checkout nothing has read yet is read by the brief itself: the
    /// reader's own uncommitted file is told, from its own checkout, with
    /// when it was read. The bound a brief gives git is the machine's
    /// business, not this test's: it is set long here, and what a brief
    /// says of a checkout it did not reach in time is the next test's.
    #[tokio::test]
    async fn a_brief_reads_the_checkouts_nothing_has_read_yet() {
        let git = |dir: &std::path::Path, args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let dir = TempDir::new().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        if !git(&work, &["init", "-q"]) {
            return;
        }
        std::fs::write(work.join("kept.txt"), "kept").unwrap();
        assert!(git(&work, &["add", "-A"]) && git(&work, &["commit", "-q", "-m", "one"]));
        std::fs::write(work.join("wip.txt"), "not yet").unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().to_path_buf(), dir.path().join("sock")).unwrap());
        daemon.set_cold_read_for_test(std::time::Duration::from_secs(120));
        let me = register(&daemon, "alice", &work).await;
        let read = brief(
            daemon
                .handle(Request::Brief {
                    from: me.id.to_string(),
                    project: None,
                })
                .await,
        );
        assert_eq!(read.checkouts.len(), 1, "{:?}", read.checkouts);
        let line = &read.checkouts[0];
        assert!(line.yours, "{line:?}");
        let standing = line.standing.as_ref().unwrap();
        assert_eq!(standing.dirty, ["wip.txt"]);
        assert_eq!(standing.branch.as_deref(), Some("main"));
        assert_eq!(read.checkouts_unread, 0);
    }

    /// A brief whose bound runs out before git answers still answers, at
    /// once: the checkout it did not reach is counted as unread, never told
    /// as clean or as read, and the listing it could not finish is said.
    #[tokio::test]
    async fn a_brief_whose_bound_runs_out_counts_the_checkout_unread() {
        let dir = TempDir::new().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        if !git_ok(&work, &["init", "-q"]) {
            return;
        }
        std::fs::write(work.join("kept.txt"), "kept").unwrap();
        assert!(git_ok(&work, &["add", "-A"]) && git_ok(&work, &["commit", "-q", "-m", "one"]));
        std::fs::write(work.join("wip.txt"), "not yet").unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().to_path_buf(), dir.path().join("sock")).unwrap());
        daemon.set_cold_read_for_test(std::time::Duration::ZERO);
        let me = register(&daemon, "alice", &work).await;
        let read = brief(
            daemon
                .handle(Request::Brief {
                    from: me.id.to_string(),
                    project: None,
                })
                .await,
        );
        assert!(read.checkouts.is_empty(), "{:?}", read.checkouts);
        assert_eq!(read.checkouts_clean, 0);
        assert_eq!(read.checkouts_unread, 1, "{read:?}");
        assert!(
            read.not_covered
                .iter()
                .any(|line| line.contains("git could not list them")),
            "{:?}",
            read.not_covered
        );
    }

    /// `git` in `dir` with a fixed identity; whether it succeeded.
    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// The brief `from` asks for, asked again while a checkout is unread
    /// (each brief reads what the one before had no time for), so a slow
    /// machine is not taken for a wrong answer.
    async fn read_through(daemon: &Arc<Daemon>, from: &str, project: Option<&str>) -> Brief {
        let ask = || Request::Brief {
            from: from.to_owned(),
            project: project.map(str::to_owned),
        };
        let mut read = brief(daemon.handle(ask()).await);
        for _ in 0..20 {
            if read.checkouts_unread == 0 && read.not_covered.len() == 1 {
                break;
            }
            read = brief(daemon.handle(ask()).await);
        }
        read
    }

    /// A brief finds the project's checkouts itself, before any tick: a
    /// linked worktree nobody registered is listed and read for a live
    /// agent of the project, and, after a restart with nobody live, for
    /// the person naming the project by its id or by a path in another
    /// checkout. Where GitHub is asked from is the selected project's
    /// checkout, never another repository's.
    #[tokio::test]
    async fn a_brief_finds_the_checkouts_nobody_works_in() {
        let dir = TempDir::new().unwrap();
        let main = dir.path().join("main");
        let linked = dir.path().join("linked");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        if !git_ok(&main, &["init", "-q"]) {
            return;
        }
        assert!(git_ok(
            &main,
            &["commit", "-q", "--allow-empty", "-m", "root"]
        ));
        assert!(git_ok(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "topic",
                linked.to_str().unwrap()
            ]
        ));
        assert!(git_ok(
            &linked,
            &["commit", "-q", "--allow-empty", "-m", "topic"]
        ));
        std::fs::write(linked.join("wip.txt"), "not yet").unwrap();
        assert!(git_ok(&other, &["init", "-q"]));
        assert!(git_ok(
            &other,
            &["commit", "-q", "--allow-empty", "-m", "other root"]
        ));
        // Uncommitted files with long names make the other checkout's line
        // longer than the closing line a brief keeps room for, wherever the
        // temporary directory is. With only a short path to show (`/tmp` on
        // Linux) the line is the shorter, no budget under the whole holds
        // the title and the closing line, and the brief rightly says nothing
        // at all, which is no test of the closing line.
        for n in 0..3 {
            std::fs::write(
                other.join(format!("{n}-{}.txt", "a-long-file-name-".repeat(4))),
                "not yet",
            )
            .unwrap();
        }
        let daemon =
            Arc::new(Daemon::open(dir.path().to_path_buf(), dir.path().join("sock")).unwrap());
        let me = register(&daemon, "alice", &main).await;
        let stranger = register(&daemon, "carol", &other).await;
        let (main, linked, other) = (
            main.canonicalize().unwrap(),
            linked.canonicalize().unwrap(),
            other.canonicalize().unwrap(),
        );
        let project = me.project.as_ref().unwrap().id();
        let told = |read: &Brief| {
            let line = read
                .checkouts
                .iter()
                .find(|c| c.path == linked)
                .unwrap_or_else(|| panic!("{:?}", read.checkouts));
            let standing = line.standing.clone().unwrap();
            assert!(!line.yours);
            assert_eq!(standing.branch.as_deref(), Some("topic"));
            assert_eq!(standing.dirty, ["wip.txt"]);
            assert_eq!(
                (standing.base.as_deref(), standing.unmerged),
                (Some("main"), Some(1))
            );
        };

        let read = read_through(&daemon, me.id.as_str(), None).await;
        told(&read);
        assert_eq!(read.repository.as_deref(), Some(main.as_path()));
        // The main checkout, committed with no remote, is merged into its
        // own branch only: never pushed, so told, from git's read to the
        // text.
        let text = render(&read, Utc::now(), usize::MAX);
        let quoted = serde_json::to_string(&main.display().to_string()).unwrap();
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("- {quoted} (yours) on main at ")))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(line.contains(": never pushed; read "), "{line}");

        // A restart, with nobody of the project live: nothing is cached.
        for agent in [&me, &stranger] {
            let ended = daemon
                .handle(Request::Deregister {
                    agent: agent.id.to_string(),
                })
                .await;
            assert!(!matches!(ended, Response::Error { .. }), "{ended:?}");
        }
        {
            let mut state = lock(&daemon.state);
            state.project_checkouts.clear();
            state.checkout_reads.clear();
        }
        let read = read_through(&daemon, HUMAN, Some(project.as_str())).await;
        told(&read);
        assert_eq!(read.repository.as_deref(), Some(main.as_path()));
        let from_linked = linked.display().to_string();
        let read = read_through(&daemon, HUMAN, Some(from_linked.as_str())).await;
        told(&read);
        assert_eq!(read.repository.as_deref(), Some(linked.as_path()));
        let from_other = other.display().to_string();
        let read = read_through(&daemon, HUMAN, Some(from_other.as_str())).await;
        assert!(read.checkouts.iter().all(|c| c.path != linked), "{read:?}");
        assert_eq!(read.repository.as_deref(), Some(other.as_path()));
        // What it leaves out for room, the person is told to ask for by the
        // other project's id, which means it from any directory.
        let whole = render(&read, Utc::now(), usize::MAX);
        let short = render(&read, Utc::now(), whole.len() - 1);
        let other_id = stranger.project.as_ref().unwrap().id();
        assert!(
            short.contains(&format!(
                "\nLeft out for room: 1 checkout; `agentdocker brief --all --project {}` shows them.",
                other_id.as_str()
            )),
            "{short:?} of {whole:?}"
        );
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
