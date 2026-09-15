//! One external controller as the sole consumer of an agent's queued input.
//!
//! A managed Codex bridge session already has one consumer: the bridge the
//! daemon started. An agent somebody registered from the outside, a hook or
//! MCP session of any runtime, has none: whoever reads its inbox delivers.
//! An input binding gives such an agent one consumer too, an external
//! controller process, bound to one exact provider generation (process,
//! session, profile) and authenticated by a token the controller made and
//! keeps. While the binding stands, legacy readers are told the input is
//! owned, the controller's reads take the queue, and a message a legacy
//! reader had already been offered before the binding travels flagged, so
//! the controller reconciles it against the provider rather than enqueue
//! it twice or drop it.
use std::collections::HashSet;

use agentdocker_core::{
    AgentId, ErrorCode, EventKind, InputBinding, MessageId, ProcessIdentity, ProviderGeneration,
    Response,
};
use chrono::{DateTime, Utc};

use super::*;

/// The digest a binding keeps of a token; the token itself is never stored.
pub fn token_digest(token: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn token_valid(token: &str) -> bool {
    (32..=128).contains(&token.len()) && token.chars().all(|c| c.is_ascii_graphic())
}

/// Whether the process is the one named: alive, and born when the identity
/// says. A recycled pid with another birth is somebody else.
fn is_running(process: &ProcessIdentity) -> bool {
    agentdocker_host::procinfo::alive(process.pid)
        && agentdocker_host::procinfo::start_time(process.pid) == Some(process.started_at)
}

impl State {
    /// Who consumes this agent's input, when it is not the caller: the
    /// bound controller, or the daemon's own bridge.
    pub(super) fn input_owner(&self, id: &AgentId) -> Option<Response> {
        let record = self.registry.get(id)?;
        if let Some(binding) = &record.input_binding {
            return Some(Response::InputOwned {
                agent: id.clone(),
                owner: "controller".to_owned(),
                controller: Some(binding.controller.clone()),
                since: Some(binding.bound_at),
            });
        }
        if agentdocker_host::provider_input::is_codex_input(record) {
            return Some(Response::InputOwned {
                agent: id.clone(),
                owner: "bridge".to_owned(),
                controller: None,
                since: record.started_at,
            });
        }
        None
    }

    /// Whether `token` opens this agent's binding. `Ok(None)` when the
    /// agent has no binding at all.
    pub(super) fn binding_for(
        &self,
        id: &AgentId,
        token: Option<&str>,
    ) -> Result<Option<&InputBinding>, Box<Response>> {
        let Some(binding) = self.registry.get(id).and_then(|r| r.input_binding.as_ref()) else {
            return Ok(None);
        };
        match token {
            Some(token) if binding.accepts_digest(&token_digest(token)) => Ok(Some(binding)),
            Some(_) => Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "this agent's input is bound to a controller; the token does not match",
            ))),
            None => Err(Box::new(Response::error(
                ErrorCode::Forbidden,
                "this agent's input is bound to a controller; a token is required",
            ))),
        }
    }

    /// A legacy reader was just offered these queued messages: remember
    /// the first time each was, so a controller that binds later knows
    /// which ones may already have been injected. While a binding stands
    /// the same offer makes those messages uncertain for the controller
    /// at once: a non-draining read may be a person looking, or the
    /// session's own MCP read putting the text in front of the model, and
    /// the daemon cannot tell which. Bookkeeping, like liveness:
    /// persisted without an event.
    pub(super) fn note_legacy_offers(
        &mut self,
        id: &AgentId,
        messages: &[MessageId],
        now: DateTime<Utc>,
    ) {
        let Some(mut record) = self.registry.get(id).cloned() else {
            return;
        };
        let queued: HashSet<&MessageId> = self
            .inboxes
            .get(id)
            .into_iter()
            .flatten()
            .map(|m| &m.id)
            .collect();
        record.legacy_offers.retain(|m, _| queued.contains(m));
        let mut changed = false;
        for message in messages {
            if queued.contains(message) && !record.legacy_offers.contains_key(message) {
                record.legacy_offers.insert(message.clone(), now);
                changed = true;
            }
            if let Some(binding) = record.input_binding.as_mut()
                && queued.contains(message)
                && !binding.uncertain.contains(message)
            {
                binding.uncertain.push(message.clone());
                changed = true;
            }
        }
        if !changed
            && self.registry.get(id).map(|r| &r.legacy_offers) == Some(&record.legacy_offers)
        {
            return;
        }
        self.persist("legacy offers", |store| store.upsert_agent(&record));
        if self.storage_error.is_none() {
            *self.registry.get_mut(id).expect("resolved agent") = record;
        }
    }

    /// Queued messages left the queue: nothing about them is uncertain or
    /// offered any more.
    pub(super) fn forget_delivered(&mut self, id: &AgentId, messages: &[MessageId]) {
        let Some(record) = self.registry.get_mut(id) else {
            return;
        };
        let mut changed = false;
        for message in messages {
            changed |= record.legacy_offers.remove(message).is_some();
            if let Some(binding) = record.input_binding.as_mut() {
                let before = binding.uncertain.len();
                binding.uncertain.retain(|m| m != message);
                changed |= binding.uncertain.len() != before;
            }
        }
        if changed {
            let record = record.clone();
            self.persist("input bookkeeping", |store| store.upsert_agent(&record));
        }
    }

    pub(super) fn bind_input(
        &mut self,
        reference: &str,
        provider: ProviderGeneration,
        controller: ProcessIdentity,
        token: &str,
        now: DateTime<Utc>,
    ) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let record = self.registry.get(&id).expect("resolved agent");
        if !token_valid(token) {
            return Response::error(
                ErrorCode::Invalid,
                "a binding token is 32 to 128 printable ASCII characters, made and kept by the controller",
            );
        }
        if !provider.valid() || !std::path::Path::new(&provider.profile).is_absolute() {
            return Response::error(
                ErrorCode::Invalid,
                "the provider generation needs a pid, a session id and an absolute profile path",
            );
        }
        if agentdocker_host::provider_input::is_codex_input(record) {
            return Response::error(
                ErrorCode::Invalid,
                "this session's input is consumed by the daemon's own Codex bridge",
            );
        }
        if !record.status.is_live() {
            return Response::error(ErrorCode::Invalid, "the agent is not live");
        }
        // The provider is the registered process, exactly, and the session
        // the registration named: a controller cannot bind somebody else's
        // conversation to this agent's queue.
        if record.pid != Some(provider.process.pid)
            || record.process_started_at != Some(provider.process.started_at)
        {
            return Response::error(
                ErrorCode::Invalid,
                "the provider process must be the agent's registered pid and birth",
            );
        }
        if record.spec.labels.get("session_id") != Some(&provider.session) {
            return Response::error(
                ErrorCode::Invalid,
                "the provider session must be the agent's registered session_id",
            );
        }
        if !is_running(&provider.process) {
            return Response::error(ErrorCode::Invalid, "the provider process is not running");
        }
        if !is_running(&controller) {
            return Response::error(ErrorCode::Invalid, "the controller process is not running");
        }
        let digest = token_digest(token);
        let mut resumed = false;
        let mut binding = match &record.input_binding {
            None => InputBinding {
                provider: provider.clone(),
                controller: controller.clone(),
                token_sha256: digest,
                bound_at: now,
                controller_generations: 1,
                // Whatever a legacy reader was already offered may be inside
                // the provider already; the controller finds out, not us.
                uncertain: record.legacy_offers.keys().cloned().collect(),
            },
            Some(existing) => {
                if !existing.accepts_digest(&digest) {
                    return Response::error(
                        ErrorCode::Forbidden,
                        "this agent's input is already bound; the token does not match",
                    );
                }
                if existing.provider != provider {
                    // Even a dead provider keeps its binding and queue:
                    // reconciling them is explicit, through unbind.
                    return Response::Error {
                        code: ErrorCode::Conflict,
                        message: "this agent's input is bound to another provider generation; unbind it first"
                            .to_owned(),
                        details: Some(serde_json::json!({ "bound": existing.provider })),
                    };
                }
                if existing.controller == controller {
                    // The same bind again: a lost reply, answered the same way.
                    return Response::InputBound {
                        agent: id,
                        binding: existing.clone(),
                        resumed: false,
                    };
                }
                if is_running(&existing.controller) {
                    return Response::Error {
                        code: ErrorCode::Conflict,
                        message: "another controller process still holds this binding".to_owned(),
                        details: Some(serde_json::json!({ "controller": existing.controller })),
                    };
                }
                resumed = true;
                let mut resumed_binding = existing.clone();
                resumed_binding.controller = controller.clone();
                resumed_binding.controller_generations =
                    existing.controller_generations.saturating_add(1);
                resumed_binding
            }
        };
        // Uncertainty is only ever about messages still queued.
        let queued: HashSet<&MessageId> = self
            .inboxes
            .get(&id)
            .into_iter()
            .flatten()
            .map(|m| &m.id)
            .collect();
        binding.uncertain.retain(|m| queued.contains(m));
        let mut record = record.clone();
        record.input_binding = Some(binding.clone());
        let mut event = Event::new(
            EventKind::InputBound {
                agent: id.clone(),
                controller,
                resumed,
            },
            now,
        );
        event.seq = self.next_seq;
        self.persist("input binding", |store| {
            store.agent_transition(&record, &event)
        });
        if let Some(error) = self.storage_failure() {
            return error;
        }
        *self.registry.get_mut(&id).expect("resolved agent") = record;
        self.next_seq += 1;
        let _ = self.events.send(event);
        Response::InputBound {
            agent: id,
            binding,
            resumed,
        }
    }

    pub(super) fn unbind_input(
        &mut self,
        reference: &str,
        token: Option<&str>,
        force: bool,
        now: DateTime<Utc>,
    ) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let record = self.registry.get(&id).expect("resolved agent");
        let Some(binding) = &record.input_binding else {
            return Response::Ok;
        };
        let reason = match token {
            Some(token) if binding.accepts_digest(&token_digest(token)) => {
                "released by its controller"
            }
            Some(_) => {
                return Response::error(
                    ErrorCode::Forbidden,
                    "the token does not match this agent's binding",
                );
            }
            None if force && !is_running(&binding.controller) => {
                "forced after the controller ended"
            }
            None if force => {
                return Response::error(
                    ErrorCode::Conflict,
                    "the bound controller process is still running; unbind with its token or stop it first",
                );
            }
            None => {
                return Response::error(
                    ErrorCode::Forbidden,
                    "unbinding needs the controller's token, or --force once the controller has ended",
                );
            }
        };
        let mut record = record.clone();
        record.input_binding = None;
        let mut event = Event::new(
            EventKind::InputUnbound {
                agent: id.clone(),
                reason: reason.to_owned(),
            },
            now,
        );
        event.seq = self.next_seq;
        self.persist("input unbinding", |store| {
            store.agent_transition(&record, &event)
        });
        if let Some(error) = self.storage_failure() {
            return error;
        }
        *self.registry.get_mut(&id).expect("resolved agent") = record;
        self.next_seq += 1;
        let _ = self.events.send(event);
        Response::Ok
    }

    /// The bound controller's read: acknowledge what it has receipts for,
    /// then take the whole queue, with the messages a legacy reader had
    /// already been offered named so it reconciles them first.
    pub(super) fn bound_read(
        &mut self,
        reference: &str,
        acknowledge: &[MessageId],
        token: &str,
    ) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if let Err(refusal) = self.binding_for(&id, Some(token)) {
            return *refusal;
        }
        if !acknowledge.is_empty() {
            match self.ack_inbox(reference, acknowledge) {
                Response::Ok => self.forget_delivered(&id, acknowledge),
                other => return other,
            }
        }
        // A blocked provider offers nothing and keeps every row, as it
        // does for the bridge.
        match self.delivery_queue(reference) {
            Response::Messages { messages } => {
                let uncertain = self
                    .registry
                    .get(&id)
                    .and_then(|r| r.input_binding.as_ref())
                    .map(|b| b.uncertain.clone())
                    .unwrap_or_default();
                Response::InputBatch {
                    agent: id,
                    messages,
                    uncertain,
                }
            }
            Response::InputWaiting { .. } => Response::InputBatch {
                agent: id,
                messages: Vec::new(),
                uncertain: Vec::new(),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentRecord, AgentSpec, InputReport, Request};
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    /// This test process, exactly: the one identity that is certainly
    /// alive for the whole test.
    fn me() -> ProcessIdentity {
        let pid = std::process::id();
        ProcessIdentity {
            pid,
            started_at: agentdocker_host::procinfo::start_time(pid).expect("own birth"),
        }
    }

    /// A short-lived process to be a controller that is somebody else.
    struct Other(std::process::Child);

    impl Other {
        fn spawn() -> Self {
            Self(
                std::process::Command::new("sleep")
                    .arg("30")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        }

        fn identity(&self) -> ProcessIdentity {
            let pid = self.0.id();
            ProcessIdentity {
                pid,
                started_at: agentdocker_host::procinfo::start_time(pid).expect("child birth"),
            }
        }

        fn end(mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    impl Drop for Other {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Register an agent standing for this process, with the session id
    /// a hook would have reported.
    async fn provider(daemon: &Arc<Daemon>, name: &str, session: &str) -> AgentRecord {
        let mut spec = AgentSpec {
            name: name.into(),
            runtime: "custom".into(),
            ..Default::default()
        };
        spec.labels.insert("session_id".into(), session.into());
        match daemon
            .handle(Request::Register {
                spec,
                pid: Some(std::process::id()),
                session: None,
            })
            .await
        {
            Response::Agent { agent } => agent,
            other => panic!("{other:?}"),
        }
    }

    async fn peer(daemon: &Arc<Daemon>, name: &str) -> AgentRecord {
        match daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: name.into(),
                    ..Default::default()
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

    async fn send(
        daemon: &Arc<Daemon>,
        from: &AgentRecord,
        to: &AgentRecord,
        text: &str,
    ) -> MessageId {
        match daemon
            .handle(Request::Send {
                from: from.id.to_string(),
                to: to.id.to_string(),
                kind: "chat".into(),
                payload: serde_json::json!({ "text": text }),
                reply_to: None,
            })
            .await
        {
            Response::Sent { message, .. } => message,
            other => panic!("{other:?}"),
        }
    }

    fn generation(record: &AgentRecord, session: &str) -> ProviderGeneration {
        ProviderGeneration {
            process: ProcessIdentity {
                pid: record.pid.unwrap(),
                started_at: record.process_started_at.unwrap(),
            },
            session: session.into(),
            profile: "/etc/hosts".into(),
        }
    }

    fn bind(
        record: &AgentRecord,
        session: &str,
        controller: ProcessIdentity,
        token: &str,
    ) -> Request {
        Request::BindInput {
            agent: record.id.to_string(),
            provider: generation(record, session),
            controller,
            token: token.into(),
        }
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const OTHER_TOKEN: &str = "fedcba9876543210fedcba9876543210";

    /// A controller binds once, binds again as a no-op, reads the queue
    /// with the messages a hook had already been offered flagged, and from
    /// then on legacy readers are told the input is owned, except for the
    /// acknowledgement of exactly those flagged messages.
    #[tokio::test]
    async fn a_binding_makes_the_controller_the_consumer_and_flags_what_a_hook_already_saw() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "sess-1").await;
        let sender = peer(&daemon, "sender").await;
        let first = send(&daemon, &sender, &receiver, "one").await;
        // A hook read before any binding: an offer the daemon remembers.
        let offered = daemon
            .handle(Request::DeliveryQueue {
                agent: receiver.id.to_string(),
            })
            .await;
        assert!(matches!(&offered, Response::Messages { messages } if messages.len() == 1));
        let second = send(&daemon, &sender, &receiver, "two").await;

        let bound = daemon.handle(bind(&receiver, "sess-1", me(), TOKEN)).await;
        let Response::InputBound {
            binding, resumed, ..
        } = bound
        else {
            panic!("{bound:?}");
        };
        assert!(!resumed);
        assert_eq!(binding.controller_generations, 1);
        assert_eq!(
            binding.uncertain,
            vec![first.clone()],
            "only what the hook saw"
        );
        assert!(binding.accepts_digest(&token_digest(TOKEN)));
        assert!(!binding.accepts_digest(&token_digest(OTHER_TOKEN)));

        // The same bind again is the same answer, not a new binding.
        let again = daemon.handle(bind(&receiver, "sess-1", me(), TOKEN)).await;
        assert!(
            matches!(&again, Response::InputBound { binding: b, resumed: false, .. } if *b == binding),
            "{again:?}"
        );

        // Legacy readers: told, not refused. A plain look stays possible.
        for request in [
            Request::DeliveryQueue {
                agent: receiver.id.to_string(),
            },
            Request::Inbox {
                agent: receiver.id.to_string(),
                drain: true,
            },
        ] {
            let answer = daemon.handle(request).await;
            assert!(
                matches!(&answer, Response::InputOwned { owner, controller: Some(c), .. } if owner == "controller" && *c == me()),
                "{answer:?}"
            );
        }
        // A look without draining after the binding may be a person or
        // the session's own MCP read, so what it showed becomes uncertain
        // too: here the second message.
        assert!(matches!(
            daemon
                .handle(Request::Inbox {
                    agent: receiver.id.to_string(),
                    drain: false,
                })
                .await,
            Response::Messages { .. }
        ));
        assert_eq!(
            lock(&daemon.state)
                .registry
                .get(&receiver.id)
                .unwrap()
                .input_binding
                .as_ref()
                .unwrap()
                .uncertain,
            vec![first.clone(), second.clone()]
        );
        // Reading as the provider without the token is refused; with it,
        // the whole queue comes with the uncertain one named.
        assert!(matches!(
            daemon
                .handle(Request::ProviderInbox {
                    agent: receiver.id.to_string(),
                    acknowledge: Vec::new(),
                    token: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::ProviderInbox {
                    agent: receiver.id.to_string(),
                    acknowledge: Vec::new(),
                    token: Some(OTHER_TOKEN.into()),
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        let batch = daemon
            .handle(Request::ProviderInbox {
                agent: receiver.id.to_string(),
                acknowledge: Vec::new(),
                token: Some(TOKEN.into()),
            })
            .await;
        let Response::InputBatch {
            messages,
            uncertain,
            ..
        } = batch
        else {
            panic!("{batch:?}");
        };
        assert_eq!(
            messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            vec![first.clone(), second.clone()]
        );
        assert_eq!(uncertain, vec![first.clone(), second.clone()]);

        // A legacy reader finishing its in-flight delivery may still
        // acknowledge what it was offered, and nothing else: a third
        // message nobody legacy has seen is the controller's alone.
        let third = send(&daemon, &sender, &receiver, "three").await;
        assert!(matches!(
            daemon
                .handle(Request::AckInbox {
                    agent: receiver.id.to_string(),
                    messages: vec![third.clone()],
                })
                .await,
            Response::InputOwned { .. }
        ));
        assert!(matches!(
            daemon
                .handle(Request::AckInbox {
                    agent: receiver.id.to_string(),
                    messages: vec![first.clone()],
                })
                .await,
            Response::Ok
        ));
        // The controller acknowledges the rest; nothing uncertain remains.
        let batch = daemon
            .handle(Request::ProviderInbox {
                agent: receiver.id.to_string(),
                acknowledge: vec![second.clone(), third.clone()],
                token: Some(TOKEN.into()),
            })
            .await;
        assert!(
            matches!(&batch, Response::InputBatch { messages, uncertain, .. } if messages.is_empty() && uncertain.is_empty()),
            "{batch:?}"
        );
        let record = lock(&daemon.state)
            .registry
            .get(&receiver.id)
            .unwrap()
            .clone();
        assert!(record.legacy_offers.is_empty());
        assert!(record.input_binding.unwrap().uncertain.is_empty());
    }

    /// A controller that restarted resumes with its token; one that is
    /// still running is not replaced; the wrong token opens nothing; and
    /// the binding, with the queue, survives the daemon reopening.
    #[tokio::test]
    async fn a_restarted_controller_resumes_and_a_live_one_is_not_replaced() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "sess-1").await;
        let sender = peer(&daemon, "sender").await;
        let queued = send(&daemon, &sender, &receiver, "kept").await;
        let first_controller = Other::spawn();
        assert!(matches!(
            daemon
                .handle(bind(
                    &receiver,
                    "sess-1",
                    first_controller.identity(),
                    TOKEN
                ))
                .await,
            Response::InputBound { resumed: false, .. }
        ));
        // Somebody else, right token, while the first still runs.
        let conflict = daemon.handle(bind(&receiver, "sess-1", me(), TOKEN)).await;
        assert!(
            matches!(
                &conflict,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ),
            "{conflict:?}"
        );
        // The first controller ends; a wrong token still opens nothing.
        let ended = first_controller.identity();
        first_controller.end();
        assert!(matches!(
            daemon
                .handle(bind(&receiver, "sess-1", me(), OTHER_TOKEN))
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        let resumed = daemon.handle(bind(&receiver, "sess-1", me(), TOKEN)).await;
        let Response::InputBound {
            binding, resumed, ..
        } = resumed
        else {
            panic!("{resumed:?}");
        };
        assert!(resumed);
        assert_eq!(binding.controller, me());
        assert_ne!(binding.controller, ended);
        assert_eq!(binding.controller_generations, 2);

        // Durable: the binding and the queue are there after a reopen.
        drop(daemon);
        let daemon = open(&dir);
        let record = lock(&daemon.state)
            .registry
            .get(&receiver.id)
            .unwrap()
            .clone();
        assert_eq!(record.input_binding, Some(binding));
        let batch = daemon
            .handle(Request::ProviderInbox {
                agent: receiver.id.to_string(),
                acknowledge: Vec::new(),
                token: Some(TOKEN.into()),
            })
            .await;
        assert!(
            matches!(&batch, Response::InputBatch { messages, .. } if messages.iter().map(|m| &m.id).eq([&queued])),
            "{batch:?}"
        );
    }

    /// A binding names the registered provider exactly: not another pid,
    /// not another session, not a relative profile, not the daemon's own
    /// bridge session, and not a dead controller.
    #[tokio::test]
    async fn a_binding_must_name_the_registered_provider_exactly() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "sess-1").await;
        let refused = |response: Response| {
            assert!(
                matches!(
                    &response,
                    Response::Error {
                        code: ErrorCode::Invalid,
                        ..
                    }
                ),
                "{response:?}"
            );
        };
        let mut wrong_pid = generation(&receiver, "sess-1");
        wrong_pid.process.pid += 1;
        refused(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: wrong_pid,
                    controller: me(),
                    token: TOKEN.into(),
                })
                .await,
        );
        refused(daemon.handle(bind(&receiver, "sess-2", me(), TOKEN)).await);
        let mut relative = generation(&receiver, "sess-1");
        relative.profile = "profile.toml".into();
        refused(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: relative,
                    controller: me(),
                    token: TOKEN.into(),
                })
                .await,
        );
        refused(
            daemon
                .handle(bind(&receiver, "sess-1", me(), "short"))
                .await,
        );
        let gone = Other::spawn();
        let dead = gone.identity();
        gone.end();
        refused(daemon.handle(bind(&receiver, "sess-1", dead, TOKEN)).await);
        assert!(
            lock(&daemon.state)
                .registry
                .get(&receiver.id)
                .unwrap()
                .input_binding
                .is_none(),
            "nothing bound"
        );
    }

    /// Another provider generation is refused while a binding stands, even
    /// after the provider ended; releasing is explicit, by token, or by
    /// force only once the controller is gone. Reports need the token too.
    #[tokio::test]
    async fn another_generation_waits_for_an_explicit_unbind_and_reports_need_the_token() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "sess-1").await;
        assert!(matches!(
            daemon.handle(bind(&receiver, "sess-1", me(), TOKEN)).await,
            Response::InputBound { .. }
        ));
        // The same process and session under another profile: another
        // generation, refused while this binding stands.
        let mut other_profile = generation(&receiver, "sess-1");
        other_profile.profile = "/etc/passwd".into();
        let other = daemon
            .handle(Request::BindInput {
                agent: receiver.id.to_string(),
                provider: other_profile.clone(),
                controller: me(),
                token: TOKEN.into(),
            })
            .await;
        assert!(
            matches!(
                &other,
                Response::Error {
                    code: ErrorCode::Conflict,
                    ..
                }
            ),
            "{other:?}"
        );
        // Reports: the token, or nothing.
        let report = |token: Option<&str>| Request::ReportInput {
            agent: receiver.id.to_string(),
            process_started_at: receiver.process_started_at.unwrap(),
            observed_at: Utc::now(),
            report: InputReport::Ready,
            token: token.map(str::to_owned),
        };
        assert!(matches!(
            daemon.handle(report(None)).await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        assert!(matches!(
            daemon.handle(report(Some(TOKEN))).await,
            Response::Ok
        ));
        // Unbinding: not without the token, not by force while the
        // controller (this process) runs, then by token.
        let unbind = |token: Option<&str>, force: bool| Request::UnbindInput {
            agent: receiver.id.to_string(),
            token: token.map(str::to_owned),
            force,
        };
        assert!(matches!(
            daemon.handle(unbind(None, false)).await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        assert!(matches!(
            daemon.handle(unbind(None, true)).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        assert!(matches!(
            daemon.handle(unbind(Some(OTHER_TOKEN), false)).await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));
        let mut events = daemon.subscribe_events();
        assert!(matches!(
            daemon.handle(unbind(Some(TOKEN), false)).await,
            Response::Ok
        ));
        let unbound = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(event) = events.recv().await
                    && let EventKind::InputUnbound { reason, .. } = event.kind
                {
                    return reason;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(unbound, "released by its controller");
        // Free again: the other generation binds, and legacy reads work
        // in between.
        assert!(matches!(
            daemon
                .handle(Request::DeliveryQueue {
                    agent: receiver.id.to_string(),
                })
                .await,
            Response::Messages { .. }
        ));
        assert!(
            matches!(
                daemon.handle(bind(&receiver, "sess-2", me(), TOKEN)).await,
                Response::Error {
                    code: ErrorCode::Invalid,
                    ..
                }
            ),
            "sess-2 is not the registered session"
        );
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: other_profile,
                    controller: me(),
                    token: OTHER_TOKEN.into(),
                })
                .await,
            Response::InputBound { resumed: false, .. }
        ));
        // A forced unbind is refused while this process, the controller, lives.
        assert!(matches!(
            daemon.handle(unbind(None, true)).await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
    }
}
