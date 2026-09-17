//! Persist availability before suppressing provider input. Inspecting an inbox
//! and acknowledging a proven receipt remain possible while delivery waits.
use super::*;
use agentdocker_core::{ProviderAvailability, ProviderReport, provider_block};

impl State {
    pub(super) fn delivery_queue(&mut self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(error) => return *error,
        };
        let target = self.registry.get(&id).expect("resolved");
        if let Some((source, availability)) = provider_block(target, self.registry.all()) {
            return Response::InputWaiting {
                agent: id.clone(),
                blocked_by: source.id.clone(),
                availability: availability.clone(),
                queued: self.inboxes.get(&id).map_or(0, VecDeque::len),
            };
        }
        self.inbox(reference, false)
    }

    pub(super) fn report_provider(
        &mut self,
        reference: &str,
        generation: DateTime<Utc>,
        observed_at: DateTime<Utc>,
        report: ProviderReport,
        now: DateTime<Utc>,
    ) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(error) => return *error,
        };
        let mut record = self.registry.get(&id).expect("resolved").clone();
        if !record.status.is_live()
            || record.process_started_at != Some(generation)
            || record.spec.runtime == agentdocker_core::HUMAN_RUNTIME
        {
            return Response::error(
                ErrorCode::Invalid,
                "provider report has no matching live process generation",
            );
        }
        if observed_at < generation
            || observed_at > now
            || now - observed_at >= Duration::minutes(5)
            || record
                .provider_availability
                .as_ref()
                .is_some_and(|old| observed_at < old.observed_at)
        {
            return Response::error(
                ErrorCode::Invalid,
                "provider report is stale or in the future",
            );
        }
        let previous = record.provider_availability.as_ref();
        let state = match report {
            ProviderReport::Blocked { issue } => {
                if !issue.valid_for(&record) {
                    return Response::error(
                        ErrorCode::Invalid,
                        "provider quota scope must match this agent's explicit provider, membership and model",
                    );
                }
                if previous.is_some_and(|p| {
                    p.issue.as_ref() == Some(&issue) && p.process_started_at == generation
                }) {
                    return Response::Ok; // Repeated limit signals do not create a notification/event storm.
                }
                ProviderAvailability {
                    process_started_at: generation,
                    observed_at,
                    issue: Some(issue),
                    cleared_observation: None,
                }
            }
            ProviderReport::Recovered { blocked_at } => {
                if previous
                    .is_some_and(|p| p.issue.is_none() && p.cleared_observation == Some(blocked_at))
                {
                    return Response::Ok;
                }
                if !previous.is_some_and(|p| p.issue.is_some() && p.observed_at == blocked_at) {
                    return Response::error(
                        ErrorCode::Conflict,
                        "provider limit changed; inspect the current state before resuming",
                    );
                }
                ProviderAvailability {
                    process_started_at: generation,
                    observed_at,
                    issue: None,
                    cleared_observation: Some(blocked_at),
                }
            }
        };
        if previous.is_some_and(|p| observed_at == p.observed_at) {
            return Response::error(
                ErrorCode::Conflict,
                "provider reports conflict at the same observation time",
            );
        }
        record.provider_availability = Some(state.clone());
        self.persist_provider(record, state, false, now)
    }

    pub(super) fn resume_provider(
        &mut self,
        reference: &str,
        blocked_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(error) => return *error,
        };
        let mut record = self.registry.get(&id).expect("resolved").clone();
        let Some(previous) = record.provider_availability.as_ref() else {
            return Response::error(
                ErrorCode::Conflict,
                "this agent has no recorded provider block",
            );
        };
        if previous.issue.is_none() && previous.cleared_observation == Some(blocked_at) {
            return Response::Ok;
        }
        if previous.issue.is_none()
            || previous.observed_at != blocked_at
            || now <= previous.observed_at
        {
            return Response::error(
                ErrorCode::Conflict,
                "provider limit changed; review the new limit before resuming",
            );
        }
        let state = ProviderAvailability {
            process_started_at: previous.process_started_at,
            observed_at: now,
            issue: None,
            cleared_observation: Some(blocked_at),
        };
        record.provider_availability = Some(state.clone());
        self.persist_provider(record, state, true, now)
    }

    fn persist_provider(
        &mut self,
        record: AgentRecord,
        availability: ProviderAvailability,
        user_resumed: bool,
        now: DateTime<Utc>,
    ) -> Response {
        let id = record.id.clone();
        let mut event = Event::new(
            EventKind::ProviderAvailabilityReported {
                agent: id.clone(),
                availability,
                user_resumed,
            },
            now,
        );
        event.seq = self.next_seq;
        let committed = self.persist("provider availability", |store| {
            store.agent_transition(&record, &event)
        });
        if committed == Persisted::Committed {
            *self.registry.get_mut(&id).expect("resolved") = record;
            self.next_seq += 1;
            let _ = self.events.send(event);
        }
        self.write_failure().unwrap_or(Response::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{ProviderIssue, ProviderIssueKind as Kind};

    fn fixture(runtime: &str) -> (tempfile::TempDir, Arc<Daemon>, AgentRecord, DateTime<Utc>) {
        let dir = tempfile::tempdir().unwrap();
        let daemon =
            Arc::new(Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap());
        let now = Utc::now();
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: "provider-fixture".into(),
                runtime: runtime.into(),
                provider: Some("provider-fixture".into()),
                model: Some("model-a".into()),
                ..Default::default()
            },
            false,
            now - Duration::seconds(10),
        );
        agent.status = AgentStatus::Running;
        agent.process_started_at = Some(now - Duration::seconds(10));
        let mut state = lock(&daemon.state);
        state.registry.insert(agent.clone()).unwrap();
        state.store.upsert_agent(&agent).unwrap();
        drop(state);
        (dir, daemon, agent, now)
    }

    fn block(
        state: &mut State,
        agent: &AgentRecord,
        issue: ProviderIssue,
        at: DateTime<Utc>,
    ) -> Response {
        state.report_provider(
            agent.id.as_str(),
            agent.process_started_at.unwrap(),
            at,
            ProviderReport::Blocked { issue },
            at,
        )
    }

    #[test]
    fn every_runtime_uses_the_same_limit_queue_and_recovery_contract() {
        for runtime in agentdocker_core::runtime::RUNTIMES
            .iter()
            .map(|r| r.name)
            .chain(["custom-local-provider"])
        {
            for kind in [
                Kind::Usage,
                Kind::Rate,
                Kind::Budget,
                Kind::Billing,
                Kind::Concurrency,
                Kind::Context,
                Kind::Authentication,
                Kind::Transport,
                Kind::Unknown,
            ] {
                let (_dir, daemon, agent, now) = fixture(runtime);
                let mut state = lock(&daemon.state);
                for i in 0..8 {
                    assert!(matches!(
                        state.send(
                            if i % 2 == 0 { "user" } else { "peer" }.into(),
                            Destination::Agent(agent.id.clone()),
                            "chat".into(),
                            json!({"text":i}),
                            None
                        ),
                        Response::Sent { .. }
                    ));
                }
                let before = state.inboxes[&agent.id].clone();
                let mut issue = ProviderIssue::local(kind);
                issue.reset_at = Some(now - Duration::seconds(1));
                assert!(matches!(
                    block(&mut state, &agent, issue.clone(), now),
                    Response::Ok
                ));
                let seq = state.next_seq;
                for _ in 0..30 {
                    assert!(matches!(
                        block(
                            &mut state,
                            &agent,
                            issue.clone(),
                            now + Duration::seconds(1)
                        ),
                        Response::Ok
                    ));
                    assert!(matches!(
                        state.delivery_queue(agent.id.as_str()),
                        Response::InputWaiting { queued: 8, .. }
                    ));
                }
                assert_eq!(
                    state.next_seq, seq,
                    "duplicate provider reports must not flood events"
                );
                assert_eq!(state.inboxes[&agent.id], before);
                assert!(matches!(
                    state.inbox(agent.id.as_str(), true),
                    Response::Error {
                        code: ErrorCode::Conflict,
                        ..
                    }
                ));
                assert!(
                    matches!(state.inbox(agent.id.as_str(), false), Response::Messages { messages } if messages.len() == 8)
                );
                assert!(matches!(
                    state.resume_provider(agent.id.as_str(), now, now + Duration::seconds(2)),
                    Response::Ok
                ));
                let Response::Messages { messages } = state.delivery_queue(agent.id.as_str())
                else {
                    panic!("resumed queue")
                };
                assert_eq!(messages, before.into_iter().collect::<Vec<_>>());
                assert!(
                    state
                        .registry
                        .get(&agent.id)
                        .unwrap()
                        .input_delivery
                        .is_none(),
                    "a limit or resume is not an input receipt"
                );
            }
        }
    }

    #[tokio::test]
    async fn blocked_legacy_queue_replies_remain_decodable_without_draining_input() {
        #[derive(serde::Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum LegacyReply {
            Messages { messages: Vec<serde_json::Value> },
            Error { code: ErrorCode },
        }
        let (_dir, daemon, mut agent, now) = fixture("codex");
        let ids = {
            let mut state = lock(&daemon.state);
            for sender in ["user", "peer"] {
                state.send(
                    sender.into(),
                    Destination::Agent(agent.id.clone()),
                    "chat".into(),
                    json!({"text":"keep this input"}),
                    None,
                );
            }
            assert!(matches!(
                block(&mut state, &agent, ProviderIssue::local(Kind::Usage), now),
                Response::Ok
            ));
            let response = state.inbox(agent.id.as_str(), true);
            assert!(matches!(
                serde_json::from_value::<LegacyReply>(serde_json::to_value(response).unwrap())
                    .unwrap(),
                LegacyReply::Error {
                    code: ErrorCode::Conflict
                }
            ));
            let ids = state.inboxes[&agent.id]
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>();
            agent = state.registry.get(&agent.id).unwrap().clone();
            agent.managed = true;
            agent.spec.env.insert(
                agentdocker_host::provider_input::CODEX_INPUT_ENV.into(),
                "1".into(),
            );
            *state.registry.get_mut(&agent.id).unwrap() = agent.clone();
            state.store.upsert_agent(&agent).unwrap();
            ids
        };
        for acknowledge in [vec![], vec![ids[0].clone()]] {
            let response = daemon
                .handle(Request::ProviderInbox {
                    agent: agent.id.to_string(),
                    acknowledge,
                    token: None,
                })
                .await;
            assert!(
                matches!(serde_json::from_value::<LegacyReply>(serde_json::to_value(response).unwrap()).unwrap(),
                LegacyReply::Messages { messages } if messages.is_empty())
            );
        }
        let mut state = lock(&daemon.state);
        assert_eq!(
            state.inboxes[&agent.id]
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            ids[1..]
        );
        assert!(
            state
                .registry
                .get(&agent.id)
                .unwrap()
                .provider_availability
                .as_ref()
                .unwrap()
                .issue
                .is_some()
        );
        assert!(matches!(
            state.resume_provider(agent.id.as_str(), now, now + Duration::seconds(1)),
            Response::Ok
        ));
        assert!(
            matches!(state.delivery_queue(agent.id.as_str()), Response::Messages { messages }
            if messages.len()==1 && messages[0].id==ids[1])
        );
    }

    #[test]
    fn readiness_receipts_and_daemon_restart_cannot_clear_a_provider_limit() {
        let (dir, daemon, agent, now) = fixture("claude-code");
        {
            let mut state = lock(&daemon.state);
            assert!(matches!(
                block(&mut state, &agent, ProviderIssue::local(Kind::Rate), now),
                Response::Ok
            ));
            assert!(matches!(
                state.report_input(
                    agent.id.as_str(),
                    agent.process_started_at.unwrap(),
                    now + Duration::seconds(1),
                    agentdocker_core::InputReport::Ready,
                    None,
                    now + Duration::seconds(1)
                ),
                Response::Ok
            ));
            state.send(
                "user".into(),
                Destination::Agent(agent.id.clone()),
                "chat".into(),
                json!({"text":"received before the limit"}),
                None,
            );
            let message = state.inboxes[&agent.id][0].id.clone();
            let input = agentdocker_core::ReceivedInput {
                messages: vec![message.clone()],
                receipt: agentdocker_core::InputReceipt::ClaudeChannel,
            };
            assert!(matches!(
                state.report_input(
                    agent.id.as_str(),
                    agent.process_started_at.unwrap(),
                    now + Duration::seconds(2),
                    agentdocker_core::InputReport::Received { input },
                    None,
                    now + Duration::seconds(2)
                ),
                Response::Ok
            ));
            assert!(matches!(
                state.ack_inbox(agent.id.as_str(), &[message]),
                Response::Ok
            ));
            assert!(matches!(
                state.delivery_queue(agent.id.as_str()),
                Response::InputWaiting { queued: 0, .. }
            ));
        }
        drop(daemon);
        let reopened = Daemon::open(dir.path().join("state"), dir.path().join("sock")).unwrap();
        let mut state = lock(&reopened.state);
        assert!(matches!(
            state.delivery_queue(agent.id.as_str()),
            Response::InputWaiting { .. }
        ));
        let saved = state.registry.get(&agent.id).unwrap();
        assert!(saved.input_delivery.as_ref().unwrap().received.is_some());
        assert_eq!(
            saved
                .provider_availability
                .as_ref()
                .unwrap()
                .issue
                .as_ref()
                .unwrap()
                .kind,
            Kind::Rate
        );
    }

    #[test]
    fn newer_limits_and_changed_generations_refuse_stale_recovery() {
        let (_dir, daemon, agent, now) = fixture("gemini-cli");
        let mut state = lock(&daemon.state);
        assert!(matches!(
            block(&mut state, &agent, ProviderIssue::local(Kind::Usage), now),
            Response::Ok
        ));
        assert!(matches!(
            block(
                &mut state,
                &agent,
                ProviderIssue::local(Kind::Rate),
                now + Duration::seconds(1)
            ),
            Response::Ok
        ));
        let snapshot = state.registry.get(&agent.id).unwrap().clone();
        assert!(matches!(
            state.resume_provider(agent.id.as_str(), now, now + Duration::seconds(2)),
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        assert!(matches!(
            state.report_provider(
                agent.id.as_str(),
                now,
                now + Duration::seconds(2),
                ProviderReport::Recovered {
                    blocked_at: now + Duration::seconds(1)
                },
                now + Duration::seconds(2)
            ),
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        assert_eq!(state.registry.get(&agent.id).unwrap(), &snapshot);
        assert!(matches!(
            state.report_provider(
                agent.id.as_str(),
                agent.process_started_at.unwrap(),
                now + Duration::seconds(2),
                ProviderReport::Recovered {
                    blocked_at: now + Duration::seconds(1)
                },
                now + Duration::seconds(2)
            ),
            Response::Ok
        ));
    }

    #[test]
    fn shared_reports_without_a_known_provider_leave_queues_and_state_unchanged() {
        let (_dir, daemon, mut origin, now) = fixture("custom-provider");
        let mut state = lock(&daemon.state);
        origin.spec.provider = None;
        origin
            .spec
            .labels
            .insert("provider-quota".into(), "account".into());
        *state.registry.get_mut(&origin.id).unwrap() = origin.clone();
        state.store.upsert_agent(&origin).unwrap();
        let mut peer = origin.clone();
        peer.id = AgentId::generate();
        peer.spec.name = "peer".into();
        state.registry.insert(peer.clone()).unwrap();
        for sender in ["user", origin.id.as_str()] {
            state.send(
                sender.into(),
                Destination::Agent(peer.id.clone()),
                "chat".into(),
                json!({"text": "retained input"}),
                None,
            );
        }
        let before = state.inboxes[&peer.id].clone();
        let seq = state.next_seq;
        let mut issue = ProviderIssue::local(Kind::Usage);
        issue.quota_group = Some("account".into());
        assert!(matches!(
            block(&mut state, &origin, issue, now),
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        assert_eq!(state.next_seq, seq);
        assert!(
            state
                .registry
                .get(&origin.id)
                .unwrap()
                .provider_availability
                .is_none()
        );
        assert!(
            matches!(state.delivery_queue(peer.id.as_str()), Response::Messages { messages }
            if messages == before.into_iter().collect::<Vec<_>>())
        );
    }

    #[test]
    fn shared_limits_survive_origin_exit_and_refuse_unrelated_or_stale_recovery() {
        let (_dir, daemon, agent, now) = fixture("opencode");
        let mut state = lock(&daemon.state);
        let mut origin = agent.clone();
        origin
            .spec
            .labels
            .insert("provider-quota".into(), "account-a".into());
        *state.registry.get_mut(&origin.id).unwrap() = origin.clone();
        let mut peer = origin.clone();
        peer.id = AgentId::generate();
        peer.spec.name = "peer".into();
        state.registry.insert(peer.clone()).unwrap();
        let mut unrelated = peer.clone();
        unrelated.id = AgentId::generate();
        unrelated.spec.name = "unrelated".into();
        unrelated.spec.labels.clear();
        state.registry.insert(unrelated.clone()).unwrap();
        let mut issue = ProviderIssue::local(Kind::Usage);
        issue.quota_group = Some("account-a".into());
        assert!(matches!(
            block(&mut state, &origin, issue, now),
            Response::Ok
        ));
        assert!(matches!(
            state.delivery_queue(peer.id.as_str()),
            Response::InputWaiting { .. }
        ));
        assert!(matches!(
            state.delivery_queue(unrelated.id.as_str()),
            Response::Messages { .. }
        ));
        state.registry.get_mut(&origin.id).unwrap().status = AgentStatus::Exited { code: Some(1) };
        assert!(matches!(
            state.remove(origin.id.as_str()),
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        assert!(matches!(
            state.resume_provider(peer.id.as_str(), now, now + Duration::seconds(1)),
            Response::Error { .. }
        ));
        assert!(matches!(
            state.resume_provider(origin.id.as_str(), now, now - Duration::seconds(1)),
            Response::Error { .. }
        ));
        assert!(matches!(
            state.delivery_queue(peer.id.as_str()),
            Response::InputWaiting { .. }
        ));
        assert!(matches!(
            state.resume_provider(origin.id.as_str(), now, now + Duration::seconds(1)),
            Response::Ok
        ));
        assert!(matches!(
            state.delivery_queue(peer.id.as_str()),
            Response::Messages { .. }
        ));
        assert!(matches!(state.remove(origin.id.as_str()), Response::Ok));
    }

    #[test]
    fn a_failed_store_never_publishes_provider_recovery_or_changes_the_queue() {
        let (_dir, daemon, agent, now) = fixture("custom-provider");
        let mut state = lock(&daemon.state);
        assert!(matches!(
            block(&mut state, &agent, ProviderIssue::local(Kind::Usage), now),
            Response::Ok
        ));
        let snapshot = state.registry.get(&agent.id).unwrap().clone();
        let mut events = state.events.subscribe();
        state.store.reject_writes_for_test();
        assert!(matches!(
            state.resume_provider(agent.id.as_str(), now, now + Duration::seconds(1)),
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        assert_eq!(state.registry.get(&agent.id).unwrap(), &snapshot);
        assert_eq!(state.store.load_agents().unwrap(), vec![snapshot]);
        assert!(events.try_recv().is_err());
    }
}
