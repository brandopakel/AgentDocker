//! Session presentation keeps current work separate from retained history.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    Current,
    NeedsInput,
    History,
}

impl App {
    pub(super) fn reset_session_view(&mut self) {
        self.shell.session_filter = Filter::Current;
        self.shell.session_details = false;
        self.shell.needs_you_expanded = false;
        self.shell.more = false;
        self.shell.launch = false;
        self.confirm_stop = None;
    }

    pub(super) fn needs_input(&self, id: &str) -> bool {
        self.questions
            .iter()
            .any(|q| q.from == id && !q.expired(Utc::now()))
    }

    pub(super) fn delivery_paused(&self, agent: &AgentRecord) -> bool {
        agent
            .input_delivery
            .as_ref()
            .is_some_and(|d| d.paused_for(agent.process_started_at))
    }

    fn needs_attention(&self, agent: &AgentRecord) -> bool {
        self.needs_input(agent.id.as_str()) || self.delivery_paused(agent)
    }

    pub(super) fn session_records(&self, filter: Filter) -> Vec<&AgentRecord> {
        let needle = self.shell.search.to_lowercase();
        let mut records: Vec<_> = self
            .agents
            .iter()
            .filter(|a| {
                a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                    && self.has_project(a.project.as_ref())
                    && match filter {
                        Filter::Current => a.status.is_live(),
                        Filter::NeedsInput => self.needs_attention(a),
                        Filter::History => !a.status.is_live(),
                    }
                    && format!(
                        "{} {} {} {}",
                        a.spec.name,
                        a.spec.runtime,
                        a.id,
                        a.vcs.as_ref().map(|v| v.describe()).unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&needle)
            })
            .collect();
        records.sort_by(|a, b| {
            (if self.all_projects() {
                a.project
                    .as_ref()
                    .map(|p| &p.root)
                    .cmp(&b.project.as_ref().map(|p| &p.root))
            } else {
                std::cmp::Ordering::Equal
            })
            .then_with(|| (!self.needs_attention(a)).cmp(&!self.needs_attention(b)))
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id))
        });
        records
    }

    /// Separate snapshots can overlap briefly after adoption. Only a known
    /// PID *and* birth time prove a discovery row already has a registration.
    pub(super) fn available_processes(&self) -> Vec<&DiscoveredProcess> {
        let needle = self.shell.search.to_lowercase();
        let mut seen = BTreeSet::new();
        self.discovered
            .iter()
            .filter(|p| {
                self.has_project(p.project.as_ref())
                    && !self.agents.iter().any(|a| {
                        a.status.is_live()
                            && a.pid == Some(p.pid)
                            && p.started_at.is_some()
                            && a.process_started_at == p.started_at
                    })
                    && format!("{} {}", p.runtime, p.command)
                        .to_lowercase()
                        .contains(&needle)
                    && seen.insert((p.pid, p.started_at))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, AgentStatus};

    fn app() -> App {
        let (tx, _) = queue::channel();
        let (_, rx) = sync_channel(MESSAGE_CAPACITY);
        App::bare(tx, rx)
    }
    fn record(name: &str) -> AgentRecord {
        let mut record = AgentRecord::new(
            AgentSpec {
                name: name.into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            Utc::now(),
        );
        record.status = AgentStatus::Running;
        record
    }

    #[test]
    fn paused_delivery_remains_visible_after_exit_but_does_not_open_the_question_inbox() {
        let mut app = app();
        let mut paused = record("paused");
        let now = Utc::now();
        paused.status = AgentStatus::Exited { code: Some(1) };
        paused.process_started_at = Some(now);
        paused.input_delivery = Some(agentdocker_core::InputDelivery {
            process_started_at: now,
            paused: true,
            pause_reason: Some("Retained input requires review".into()),
            reported_at: now,
            received: None,
            received_at: None,
        });
        app.agents.push(paused.clone());
        assert!(app.session_records(Filter::Current).is_empty());
        assert_eq!(app.session_records(Filter::NeedsInput)[0].id, paused.id);
        assert!(
            !app.needs_input(paused.id.as_str()),
            "delivery recovery is not a human question"
        );
        app.agents[0].process_started_at = Some(now + chrono::Duration::seconds(1));
        assert!(
            app.session_records(Filter::NeedsInput).is_empty(),
            "an old pause cannot describe a successor process"
        );
    }

    #[test]
    fn current_sessions_exclude_history_and_humans_without_losing_records() {
        let mut app = app();
        let live = record("codex");
        let mut finished = record("codex");
        finished.status = AgentStatus::Exited { code: Some(0) };
        let mut human = record("user");
        human.spec.runtime = agentdocker_core::HUMAN_RUNTIME.into();
        app.agents = vec![finished.clone(), live.clone(), human];
        assert_eq!(app.session_records(Filter::Current)[0].id, live.id);
        assert_eq!(app.session_records(Filter::Current).len(), 1);
        assert_eq!(app.session_records(Filter::History)[0].id, finished.id);
        assert_eq!(app.agents.len(), 3);
        app.shell.search = finished.id.to_string();
        assert!(app.session_records(Filter::Current).is_empty());
        assert_eq!(app.session_records(Filter::History).len(), 1);
    }

    #[test]
    fn overlapping_adoption_snapshots_require_process_birth_evidence() {
        let mut app = app();
        let mut agent = record("registered");
        agent.pid = Some(42);
        agent.process_started_at = Some(Utc::now());
        let process = DiscoveredProcess {
            pid: 42,
            ppid: 1,
            runtime: "codex".into(),
            command: "codex".into(),
            cwd: None,
            project: None,
            started_at: agent.process_started_at,
            session: None,
        };
        app.agents.push(agent);
        app.discovered = vec![process.clone(), process];
        assert!(app.available_processes().is_empty());
        app.agents[0].process_started_at = Some(Utc::now() + chrono::Duration::seconds(1));
        assert_eq!(
            app.available_processes().len(),
            1,
            "PID reuse remains visible"
        );
        app.agents[0].process_started_at = None;
        assert_eq!(
            app.available_processes().len(),
            1,
            "unknown birth is not a match"
        );
        app.agents[0].status = AgentStatus::Exited { code: Some(0) };
        assert_eq!(app.available_processes().len(), 1);
    }

    #[test]
    fn attention_is_scoped_to_the_project_and_keeps_unanswered_finished_sessions() {
        let mut app = app();
        // Other sessions: the projectless view. With nothing selected and
        // this flag off, the home view would show every project at once.
        app.shell.catalog.unassigned = true;
        let first = record("needs-input");
        let quiet = record("quiet");
        let mut finished = record("finished-but-unanswered");
        finished.status = AgentStatus::Exited { code: Some(0) };
        let mut elsewhere = record("other-project");
        elsewhere.project = Some(ProjectRef::directory("/other"));
        let now = Utc::now();
        for agent in [&first, &finished, &elsewhere] {
            app.questions.push(Question {
                presentation: None,
                id: agent.id.to_string().into(),
                from: agent.id.to_string(),
                to: agentdocker_core::Destination::Agent("user".into()),
                text: "Continue?".into(),
                asked_at: now,
                expires_at: now + chrono::Duration::minutes(1),
            });
        }
        app.agents = vec![quiet, first.clone(), finished, elsewhere];
        assert_eq!(app.session_records(Filter::Current)[0].id, first.id);
        assert_eq!(app.session_records(Filter::NeedsInput).len(), 2);
        app.questions[0].expires_at = now - chrono::Duration::seconds(1);
        assert_eq!(app.session_records(Filter::NeedsInput).len(), 1);
    }
}
