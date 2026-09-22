//! Session presentation keeps current work separate from retained history.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    Current,
    NeedsInput,
    /// Sessions that have ended: not a tab, the collapsed *Earlier* group
    /// under the current ones.
    Earlier,
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
        agentdocker_core::provider_block(agent, &self.agents).is_some()
            || agent
                .input_delivery
                .as_ref()
                .is_some_and(|d| d.paused_for(agent.process_started_at))
    }

    /// Messages this session holds that no model has taken. The binding's
    /// uncertain set is offered input it cannot prove was handed over; those
    /// may also still be queued, so the larger of the two is the count, never
    /// their sum. `None` while the queue count is unknown.
    pub(super) fn undelivered(&self, agent: &AgentRecord) -> Option<usize> {
        let uncertain = agent
            .input_binding
            .as_ref()
            .map_or(0, |binding| binding.uncertain.len());
        self.queued_inputs
            .get(agent.id.as_str())
            .map(|queued| (*queued).max(uncertain))
    }

    /// An ended session whose paused delivery still holds messages, or
    /// might: the one delivery case where an ended session asks something of
    /// the person. Ended with nothing waiting is simply ended. Unknown counts
    /// as holding once snapshots are arriving (a wrong "nothing waits" would
    /// hide kept messages; a wrong "holding" costs one dismissal), but not
    /// before the first one, or every ended session would flash here on
    /// launch.
    pub(super) fn ended_with_undelivered(&self, agent: &AgentRecord) -> bool {
        !agent.status.is_live()
            && agent
                .input_delivery
                .as_ref()
                .is_some_and(|d| d.paused_for(agent.process_started_at))
            && match self.undelivered(agent) {
                Some(count) => count > 0,
                None => self.activity_seen,
            }
    }

    /// Whether the person dismissed this ended session's notice.
    pub(super) fn notice_dismissed(&self, agent: &AgentRecord) -> bool {
        self.shell
            .catalog
            .is_dismissed(agent.id.as_str(), agent.process_started_at, agent.pid)
    }

    /// Paused delivery that asks something of the person: a live session
    /// held by a provider block or not receiving messages, or an ended
    /// session still held by a block or still holding messages. An ended
    /// session can report no recovery, so each ended case can be dismissed;
    /// otherwise it would sit here for good.
    pub(super) fn delivery_needs_you(&self, agent: &AgentRecord) -> bool {
        let blocked = agentdocker_core::provider_block(agent, &self.agents).is_some();
        if agent.status.is_live() {
            return blocked || self.delivery_paused(agent);
        }
        (blocked || self.ended_with_undelivered(agent)) && !self.notice_dismissed(agent)
    }

    fn needs_attention(&self, agent: &AgentRecord) -> bool {
        self.needs_input(agent.id.as_str()) || self.delivery_needs_you(agent)
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
                        Filter::Earlier => !a.status.is_live(),
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
    fn provider_limits_override_ready_receivers_and_keep_attention_after_exit() {
        use agentdocker_core::{ProviderAvailability, ProviderIssue, ProviderIssueKind};
        let mut app = app();
        let mut limited = record("limited");
        let now = Utc::now();
        limited.process_started_at = Some(now);
        limited.input_delivery = Some(agentdocker_core::InputDelivery {
            process_started_at: now,
            paused: false,
            pause_reason: None,
            reported_at: now,
            received: None,
            received_at: None,
        });
        limited.provider_availability = Some(ProviderAvailability {
            process_started_at: now,
            observed_at: now,
            issue: Some(ProviderIssue::local(ProviderIssueKind::Usage)),
            cleared_observation: None,
        });
        app.agents.push(limited.clone());
        app.shell.unfocused = true;
        app.activity
            .insert(limited.id.to_string(), Activity::Working { since: now });
        app.note_completions(&BTreeMap::from([(
            limited.id.to_string(),
            Activity::Idle { since: now },
        )]));
        assert!(
            !app.shell.unviewed_done.contains(limited.id.as_str()),
            "a failed or limited turn is not Done"
        );
        assert!(
            app.delivery_paused(&limited),
            "a ready receiver cannot erase a provider limit"
        );
        app.agents[0].status = AgentStatus::Exited { code: Some(1) };
        app.agents[0].process_started_at = Some(now + chrono::Duration::seconds(2));
        assert_eq!(app.session_records(Filter::NeedsInput)[0].id, limited.id);
        assert!(
            !app.needs_input(limited.id.as_str()),
            "availability is not a question or an approval"
        );
    }

    #[test]
    fn paused_delivery_remains_visible_after_exit_but_does_not_open_the_question_inbox() {
        let mut app = app();
        // Snapshots are arriving; this session's count is simply unknown.
        app.activity_seen = true;
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

    /// An ended session with paused delivery asks for the person only while
    /// messages are left: with none it is just ended, with some it stays
    /// in Needs input until they are dismissed, and dismissing keeps them.
    #[test]
    fn ended_sessions_need_the_person_only_while_messages_wait_and_until_dismissed() {
        let mut app = app();
        let mut ended = record("ended");
        let now = Utc::now();
        ended.status = AgentStatus::Exited { code: None };
        ended.process_started_at = Some(now);
        ended.input_delivery = Some(agentdocker_core::InputDelivery {
            process_started_at: now,
            paused: true,
            pause_reason: Some(agentdocker_core::input::PAUSE_CONTROLLER_ENDED.into()),
            reported_at: now,
            received: None,
            received_at: None,
        });
        app.agents.push(ended.clone());
        let id = ended.id.to_string();

        // Before the first activity snapshot nothing is known, and nothing
        // is shown; once snapshots arrive, unknown errs on the side of
        // holding.
        assert!(!app.delivery_needs_you(&app.agents[0]));
        app.activity_seen = true;
        assert!(app.delivery_needs_you(&app.agents[0]));

        // Nothing queued: ended, not waiting on anyone.
        app.queued_inputs.insert(id.clone(), 0);
        assert!(!app.ended_with_undelivered(&app.agents[0]));
        assert!(!app.delivery_needs_you(&app.agents[0]));
        assert!(app.session_records(Filter::NeedsInput).is_empty());
        assert_eq!(app.session_records(Filter::Earlier)[0].id, ended.id);

        // Two queued: the one thing it still asks of the person.
        app.queued_inputs.insert(id.clone(), 2);
        assert_eq!(app.undelivered(&app.agents[0]), Some(2));
        // Uncertain input may still be queued too: the count is the larger,
        // never the sum.
        app.agents[0].input_binding = Some(
            serde_json::from_value(serde_json::json!({
                "provider": {"process": {"pid": 1, "started_at": now}, "session": "s", "profile": "/p"},
                "controller": {"pid": 2, "started_at": now},
                "token_sha256": "0".repeat(64), "bound_at": now,
                "controller_generations": 1, "uncertain": ["m1"]
            }))
            .unwrap(),
        );
        assert_eq!(app.undelivered(&app.agents[0]), Some(2));
        app.queued_inputs.insert(id.clone(), 0);
        assert_eq!(app.undelivered(&app.agents[0]), Some(1));
        app.queued_inputs.insert(id.clone(), 2);
        app.agents[0].input_binding = None;
        assert!(app.delivery_needs_you(&app.agents[0]));
        assert_eq!(app.session_records(Filter::NeedsInput)[0].id, ended.id);

        // Dismissed: off Needs input, the count and the queue untouched.
        let _ = app.update(Message::DismissDelivery(id.clone()));
        assert!(app.notice_dismissed(&app.agents[0]));
        assert!(!app.delivery_needs_you(&app.agents[0]));
        assert!(app.session_records(Filter::NeedsInput).is_empty());
        assert_eq!(app.undelivered(&app.agents[0]), Some(2));
        assert_eq!(app.shell.catalog.dismissed.len(), 1);
        // Dismissing twice records nothing new.
        let _ = app.update(Message::DismissDelivery(id));
        assert_eq!(app.shell.catalog.dismissed.len(), 1);

        // A resumed process is a different notice: the old dismissal does
        // not hide it.
        app.agents[0].process_started_at = Some(now + chrono::Duration::seconds(5));
        app.agents[0]
            .input_delivery
            .as_mut()
            .unwrap()
            .process_started_at = now + chrono::Duration::seconds(5);
        assert!(!app.notice_dismissed(&app.agents[0]));
        assert!(app.delivery_needs_you(&app.agents[0]));
    }

    /// An ended session held by a provider block can report no recovery,
    /// so its notice can be put away like any other ended one.
    #[test]
    fn an_ended_provider_block_can_be_dismissed() {
        use agentdocker_core::{ProviderAvailability, ProviderIssue, ProviderIssueKind};
        let mut app = app();
        let mut limited = record("limited");
        let now = Utc::now();
        limited.status = AgentStatus::Exited { code: Some(1) };
        limited.process_started_at = Some(now);
        limited.provider_availability = Some(ProviderAvailability {
            process_started_at: now,
            observed_at: now,
            issue: Some(ProviderIssue::local(ProviderIssueKind::Usage)),
            cleared_observation: None,
        });
        app.agents.push(limited.clone());
        assert!(app.delivery_needs_you(&app.agents[0]));
        assert_eq!(app.session_records(Filter::NeedsInput).len(), 1);
        let _ = app.update(Message::DismissDelivery(limited.id.to_string()));
        assert!(!app.delivery_needs_you(&app.agents[0]));
        assert!(app.session_records(Filter::NeedsInput).is_empty());
    }

    /// A live session whose delivery stopped always needs the person;
    /// dismissal is only for ended ones.
    #[test]
    fn live_paused_delivery_is_never_dismissed_away() {
        let mut app = app();
        let mut live = record("live");
        let now = Utc::now();
        live.process_started_at = Some(now);
        live.input_delivery = Some(agentdocker_core::InputDelivery {
            process_started_at: now,
            paused: true,
            pause_reason: None,
            reported_at: now,
            received: None,
            received_at: None,
        });
        app.agents.push(live.clone());
        app.queued_inputs.insert(live.id.to_string(), 0);
        assert!(app.delivery_needs_you(&app.agents[0]));
        app.shell
            .catalog
            .dismiss(live.id.as_str(), live.process_started_at, live.pid);
        assert!(app.delivery_needs_you(&app.agents[0]));
    }

    #[test]
    fn current_sessions_exclude_earlier_ones_and_humans_without_losing_records() {
        let mut app = app();
        let live = record("codex");
        let mut finished = record("codex");
        finished.status = AgentStatus::Exited { code: Some(0) };
        let mut human = record("user");
        human.spec.runtime = agentdocker_core::HUMAN_RUNTIME.into();
        app.agents = vec![finished.clone(), live.clone(), human];
        assert_eq!(app.session_records(Filter::Current)[0].id, live.id);
        assert_eq!(app.session_records(Filter::Current).len(), 1);
        assert_eq!(app.session_records(Filter::Earlier)[0].id, finished.id);
        assert_eq!(app.agents.len(), 3);
        app.shell.search = finished.id.to_string();
        assert!(app.session_records(Filter::Current).is_empty());
        assert_eq!(app.session_records(Filter::Earlier).len(), 1);
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
