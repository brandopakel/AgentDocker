//! Asking a holder to let go. An agent refused an edit or a commit over a
//! path somebody else holds can wait the lease out, or ask: the daemon
//! sends the holder a `release_request` message naming the resource and
//! keeps the request as a document until the holder answers. An accept
//! releases the named leases on the holder's behalf, journaled with the
//! summary they give; a decline carries their reason. Either answer goes
//! back to the requester as a `release_response` in reply to the request.
use super::*;
use agentdocker_core::lease::{RELEASE_REQUEST_TEXT_CHARS, RELEASE_REQUEST_TTL_SECS};
use agentdocker_core::{
    RELEASE_REQUEST_KIND, RELEASE_RESPONSE_KIND, ReleaseAnswer, ReleaseRequest,
};

const DOCUMENT: &str = "release_request";

/// How many requests may wait for an answer at once, over everybody: a
/// bound on what a loop of requests can leave on the books.
pub(super) const MAX_RELEASE_REQUESTS: usize = 256;

impl Daemon {
    pub(super) async fn request_release(
        self: &Arc<Self>,
        from: String,
        resource: String,
        reason: Option<String>,
    ) -> Response {
        let requester = match self.resolve(&from) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let reason = match bounded_text(reason, "a reason") {
            Ok(reason) => reason,
            Err(response) => return *response,
        };
        let resource = match self
            .localise(ResourceKey::new(resource), Some(&requester), false)
            .await
        {
            Ok(resource) => resource,
            Err(response) => return *response,
        };
        let mut state = lock(&self.state);
        let now = Utc::now();
        state.expire_leases_at(now);
        state.expire_release_requests(now);
        if let Some(error) = state.write_failure() {
            return error;
        }
        let requester_name = state.agent_name(&requester);
        // Who holds what overlaps it, each with their leases, in a stable
        // order so two holders get their requests the same way each time.
        let mut by_holder: BTreeMap<AgentId, Vec<Lease>> = BTreeMap::new();
        for lease in state.leases.all() {
            if lease.holder != requester && lease.resource.overlaps(&resource) {
                by_holder
                    .entry(lease.holder.clone())
                    .or_default()
                    .push(lease.clone());
            }
        }
        if by_holder.is_empty() {
            return Response::error(
                ErrorCode::NotFound,
                format!("nobody else holds a lease on {resource}"),
            );
        }
        // Everything is checked before anything is sent, so a holder the
        // requester may not reach refuses the whole request rather than
        // leaving it half asked.
        let mut requests = Vec::new();
        let mut to_send = Vec::new();
        for (holder, leases) in by_holder {
            // A request already waiting on this holder for this resource
            // is the answer: asking twice would only queue the same word.
            if let Some(pending) = state.release_requests.values().find(|request| {
                *state.registry.canonical_id(&request.requester) == requester
                    && *state.registry.canonical_id(&request.holder) == holder
                    && request.resource == resource
            }) {
                requests.push(pending.clone());
                continue;
            }
            // The holder must be somebody the requester may speak to: its
            // own project's agent or the person, as any send is held to.
            if let Err(refusal) =
                state.within_project(Some(&requester), Destination::Agent(holder.clone()))
            {
                return *refusal;
            }
            let action = format!("send:{holder}");
            let ruling = state.permits(&requester, &action);
            if !ruling.is_allowed() {
                return state.refuse(&requester, &action, ruling);
            }
            to_send.push((holder, leases));
        }
        if state.release_requests.len() + to_send.len() > MAX_RELEASE_REQUESTS {
            return Response::error(
                ErrorCode::Unavailable,
                "too many release requests are already waiting for an answer",
            );
        }
        let mut prepared = Vec::new();
        for (holder, leases) in to_send {
            let lease_ids: Vec<LeaseId> = leases.iter().map(|lease| lease.id.clone()).collect();
            let text = {
                let mut text = format!(
                    "{requester_name} asks you to release {resource} ({})",
                    describe_leases(&leases)
                );
                if let Some(reason) = &reason {
                    text.push_str(": ");
                    text.push_str(reason);
                }
                text.push_str(
                    ". Answer with respond_release (accept with a summary of what you changed, \
                     or decline with a reason); the daemon releases the lease for you on an accept.",
                );
                text
            };
            let envelope = Envelope::new(
                requester.to_string(),
                Destination::Agent(holder.clone()),
                RELEASE_REQUEST_KIND.to_owned(),
                Value::Null,
                None,
                now,
            );
            let request = ReleaseRequest {
                message: envelope.id.clone(),
                requester: requester.clone(),
                holder: holder.clone(),
                resource: resource.clone(),
                leases: lease_ids.clone(),
                reason: reason.clone(),
                at: now,
                expires_at: now + Duration::seconds(RELEASE_REQUEST_TTL_SECS),
                answer: None,
            };
            let envelope = Envelope {
                payload: json!({
                    "text": text,
                    "request": request.message,
                    "resource": resource,
                    "leases": lease_ids,
                    "requester": requester,
                    "reason": reason,
                }),
                ..envelope
            };
            prepared.push((envelope, request));
        }
        // Every holder's queue has room, or nobody is asked: a second
        // holder's full queue must not leave the first one asked alone.
        if let Some(full) = prepared
            .iter()
            .find(|(envelope, request)| !state.inbox_takes(&request.holder, envelope))
            .map(|(_, request)| request.holder.clone())
        {
            return state.inbox_full(&full);
        }
        for (envelope, request) in prepared {
            let transition = DocumentTransition {
                kind: DOCUMENT,
                id: request.message.to_string(),
                value: Some(serde_json::to_value(&request).expect("a release request serialises")),
                event: EventKind::ReleaseRequested {
                    request: request.clone(),
                },
                released: Vec::new(),
                journal: None,
                closes: None,
            };
            match state.publish_with_channel(envelope, None, None, Some(transition)) {
                Response::Sent { .. } => {
                    state
                        .release_requests
                        .insert(request.message.clone(), request.clone());
                    requests.push(request);
                }
                other => return other,
            }
        }
        Response::ReleaseRequests { requests }
    }

    pub(super) async fn respond_release(
        self: &Arc<Self>,
        from: String,
        message: MessageId,
        accept: bool,
        text: Option<String>,
    ) -> Response {
        let responder = match self.resolve(&from) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let text = match bounded_text(text, if accept { "a summary" } else { "a reason" }) {
            Ok(text) => text,
            Err(response) => return *response,
        };
        // The ledger's word on the held paths before the release that
        // quotes it, as a holder's own release has.
        if accept {
            self.flush_release_watcher(&from, None).await;
        }
        let mut state = lock(&self.state);
        let now = Utc::now();
        // A lease past its TTL is expired, not released: it must leave the
        // table as such before an accept counts what is still held.
        state.expire_leases_at(now);
        state.expire_release_requests(now);
        if let Some(error) = state.write_failure() {
            return error;
        }
        let Some(request) = state.release_requests.get(&message).cloned() else {
            return Response::error(
                ErrorCode::NotFound,
                format!("no release request {message} is waiting for an answer"),
            );
        };
        // Either side may have been folded into another record since the
        // request (a session that came back, `resume_input` retiring the
        // fresh record into its predecessor): the request is that record's
        // now, and its answer goes where the requester lives on.
        let holder = state.registry.canonical_id(&request.holder).clone();
        let requester = state.registry.canonical_id(&request.requester).clone();
        if holder != responder {
            return Response::error(
                ErrorCode::Forbidden,
                format!(
                    "this release request was put to {}, not to you",
                    state.agent_name(&holder)
                ),
            );
        }
        let requester_name = state.agent_name(&requester);
        let holder_name = state.agent_name(&holder);
        // Only the request's leases, and only those still held: a lease the
        // holder took since is not what was asked for. They are let go in
        // the same transaction as the answer, below: a requester whose
        // queue is full, or a write that fails, leaves them held and the
        // request answerable.
        let (still_held, journal) = if accept {
            let still_held: Vec<Lease> = request
                .leases
                .iter()
                .filter_map(|id| state.leases.get(id))
                .filter(|lease| lease.holder == responder)
                .cloned()
                .collect();
            // The journal line already says what was released; the summary
            // says what changed, and that it was let go on request. Nothing
            // still held is nothing released, and no entry that says so.
            let journal = if still_held.is_empty() {
                None
            } else {
                let summary = match &text {
                    Some(text) => format!("{text} (at {requester_name}'s request)"),
                    None => format!("at {requester_name}'s request"),
                };
                state.release_entry(
                    &responder,
                    &still_held,
                    Some(summary),
                    SummarySource::Explicit,
                )
            };
            if let Some(error) = state.write_failure() {
                return error;
            }
            (still_held, journal)
        } else {
            (Vec::new(), None)
        };
        let released: Vec<LeaseId> = still_held.iter().map(|lease| lease.id.clone()).collect();
        // The answer names the records as they are now.
        let answered = ReleaseRequest {
            holder: holder.clone(),
            requester: requester.clone(),
            answer: Some(ReleaseAnswer {
                accepted: accept,
                text: text.clone(),
                at: now,
                released: released.clone(),
            }),
            ..request.clone()
        };
        let mut reply = if accept {
            format!(
                "{holder_name} released {} ({})",
                request.resource,
                match released.len() {
                    0 => "nothing was still held".to_owned(),
                    1 => "1 lease".to_owned(),
                    n => format!("{n} leases"),
                }
            )
        } else {
            format!("{holder_name} declined to release {}", request.resource)
        };
        if let Some(text) = &text {
            reply.push_str(": ");
            reply.push_str(text);
        }
        let mut payload = json!({
            "text": reply,
            "request": request.message,
            "resource": request.resource,
            "accepted": accept,
            "released": released,
        });
        payload[if accept { "summary" } else { "reason" }] = json!(text);
        let envelope = Envelope::new(
            responder.to_string(),
            Destination::Agent(requester),
            RELEASE_RESPONSE_KIND.to_owned(),
            payload,
            Some(request.message.clone()),
            now,
        );
        let transition = DocumentTransition {
            kind: DOCUMENT,
            id: request.message.to_string(),
            value: None,
            event: EventKind::ReleaseAnswered {
                request: answered.clone(),
            },
            released: still_held,
            journal,
            closes: None,
        };
        match state.publish_with_channel(envelope, None, None, Some(transition)) {
            Response::Sent { .. } => {
                state.release_requests.remove(&request.message);
                Response::ReleaseRequest { request: answered }
            }
            other => other,
        }
    }
}

impl State {
    /// Whether an agent's queue takes one more message, as the message path
    /// decides it: under the count and under the bytes.
    fn inbox_takes(&self, agent: &AgentId, envelope: &Envelope) -> bool {
        let count = self.inboxes.get(agent).map_or(0, VecDeque::len);
        let bytes = self.inbox_bytes.get(agent).copied().unwrap_or_default();
        count < INBOX_CAPACITY && bytes.saturating_add(message_bytes(envelope)) <= INBOX_BYTES
    }

    /// The refusal the message path gives a full queue.
    fn inbox_full(&self, agent: &AgentId) -> Response {
        Response::error(
            ErrorCode::Backpressure,
            format!(
                "Inbox for {} is full (limit {INBOX_CAPACITY} messages / {} MiB). Acknowledge existing messages before retrying; nothing was sent.",
                agent.short(),
                INBOX_BYTES / (1024 * 1024)
            ),
        )
    }

    /// Drop the requests nobody answered within their day, each with its
    /// own event; nothing is dropped while storage is failing.
    pub(super) fn expire_release_requests(&mut self, now: DateTime<Utc>) {
        let mut expired: Vec<ReleaseRequest> = self
            .release_requests
            .values()
            .filter(|request| request.expired(now))
            .cloned()
            .collect();
        if expired.is_empty() || self.storage_error.is_some() {
            return;
        }
        expired.sort_by(|a, b| a.message.as_str().cmp(b.message.as_str()));
        for request in expired {
            let id = request.message.clone();
            let _ = self.transition(
                "release request expiry",
                Event::new(
                    EventKind::ReleaseRequestExpired {
                        request: request.clone(),
                    },
                    now,
                ),
                |store, event| {
                    store.delete_documents_with_event(DOCUMENT, &[id.to_string()], event)
                },
                |state| {
                    state.release_requests.remove(&id);
                },
            );
        }
    }

    /// An agent as the message names it: by name, or by its short id when
    /// it has none.
    fn agent_name(&self, id: &AgentId) -> String {
        self.registry
            .get(id)
            .map(|agent| agent.spec.name.as_str())
            .filter(|name| !name.is_empty())
            .map_or_else(|| id.short().to_owned(), str::to_owned)
    }
}

/// The text an agent gives, trimmed and bounded; empty is none.
fn bounded_text(text: Option<String>, what: &str) -> Result<Option<String>, Box<Response>> {
    let Some(text) = text.map(|text| text.trim().to_owned()) else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > RELEASE_REQUEST_TEXT_CHARS {
        return Err(Box::new(Response::error(
            ErrorCode::Invalid,
            format!(
                "{what} on a release request is at most {RELEASE_REQUEST_TEXT_CHARS} characters"
            ),
        )));
    }
    Ok(Some(text))
}

fn describe_leases(leases: &[Lease]) -> String {
    leases
        .iter()
        .map(|lease| format!("{} {} on {}", lease.id, lease.mode, lease.resource))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap())
    }

    async fn register(daemon: &Arc<Daemon>, name: &str, workdir: &Path) -> AgentRecord {
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

    async fn claim(daemon: &Arc<Daemon>, agent: &str, resource: &str) -> Lease {
        match daemon
            .handle(Request::Claim {
                agent: agent.to_owned(),
                resource: resource.to_owned(),
                mode: LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 600,
                note: Some("refactoring".into()),
                wait_secs: 0,
                priority: 0,
                automatic: false,
            })
            .await
        {
            Response::Lease { lease } => lease,
            other => panic!("{other:?}"),
        }
    }

    async fn inbox(daemon: &Arc<Daemon>, agent: &str) -> Vec<Envelope> {
        match daemon
            .handle(Request::Inbox {
                agent: agent.to_owned(),
                drain: false,
            })
            .await
        {
            Response::Messages { messages } => messages,
            other => panic!("{other:?}"),
        }
    }

    async fn leases(daemon: &Arc<Daemon>) -> Vec<Lease> {
        match daemon
            .handle(Request::Leases {
                agent: None,
                resource: None,
            })
            .await
        {
            Response::Leases { leases } => leases,
            other => panic!("{other:?}"),
        }
    }

    /// A request reaches the holder as a `release_request` naming the
    /// resource and leases; a second ask is the same request; the holder's
    /// accept releases the lease on their behalf, journals the summary and
    /// answers the requester with a `release_response` in reply; the
    /// request survives a reopen until it is answered.
    #[tokio::test]
    async fn an_accepted_request_releases_the_lease_for_the_holder() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let project = dir.path().join("repo");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src/lib.rs"), "").unwrap();
        let holder = register(&daemon, "holder", &project).await;
        let asker = register(&daemon, "asker", &project).await;
        let file = project.join("src/lib.rs").canonicalize().unwrap();
        let lease = claim(&daemon, "holder", &format!("path:{}", file.display())).await;
        let mut events = daemon.subscribe_events();

        let Response::ReleaseRequests { requests } = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: format!("path:{}", project.join("src").display()),
                reason: Some("  my commit touches it  ".into()),
            })
            .await
        else {
            panic!("request")
        };
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.holder, holder.id);
        assert_eq!(request.requester, asker.id);
        assert_eq!(request.leases, vec![lease.id.clone()]);
        assert_eq!(request.reason.as_deref(), Some("my commit touches it"));
        assert!(request.answer.is_none());
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(event.kind);
        }
        assert!(
            seen.iter()
                .any(|kind| matches!(kind, EventKind::ReleaseRequested { request: r } if r.message == request.message))
        );
        assert!(seen.iter().any(|kind| matches!(
            kind,
            EventKind::MessageSent { kind, .. } if kind == RELEASE_REQUEST_KIND
        )));

        let queued = inbox(&daemon, "holder").await;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, RELEASE_REQUEST_KIND);
        assert_eq!(queued[0].id, request.message);
        assert_eq!(queued[0].payload["request"], json!(request.message));
        assert_eq!(queued[0].payload["leases"], json!([lease.id]));
        let text = queued[0].payload["text"].as_str().unwrap();
        assert!(
            text.starts_with("asker asks you to release path:"),
            "{text}"
        );
        assert!(text.contains("my commit touches it"));

        // Asking again is the same request, not a second message.
        let Response::ReleaseRequests { requests: again } = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: format!("path:{}", project.join("src").display()),
                reason: None,
            })
            .await
        else {
            panic!("request again")
        };
        assert_eq!(again[0].message, request.message);
        assert_eq!(inbox(&daemon, "holder").await.len(), 1);

        // Only the holder may answer.
        assert!(matches!(
            daemon
                .handle(Request::RespondRelease {
                    from: "asker".into(),
                    request: request.message.clone(),
                    accept: true,
                    text: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));

        // The request outlives a restart.
        let message = request.message.clone();
        drop(daemon);
        let daemon = open(&dir);
        assert!(lock(&daemon.state).release_requests.contains_key(&message));
        let mut events = daemon.subscribe_events();

        let Response::ReleaseRequest { request: answered } = daemon
            .handle(Request::RespondRelease {
                from: "holder".into(),
                request: message.clone(),
                accept: true,
                text: Some("renamed the module".into()),
            })
            .await
        else {
            panic!("respond")
        };
        let answer = answered.answer.expect("answered");
        assert!(answer.accepted);
        assert_eq!(answer.released, vec![lease.id.clone()]);
        assert_eq!(answer.text.as_deref(), Some("renamed the module"));
        assert!(leases(&daemon).await.is_empty(), "the lease was released");
        assert!(lock(&daemon.state).release_requests.is_empty());

        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(event.kind);
        }
        assert!(
            seen.iter().any(
                |kind| matches!(kind, EventKind::LeaseReleased { lease: l } if l.id == lease.id)
            )
        );
        assert!(seen.iter().any(|kind| matches!(
            kind,
            EventKind::ReleaseAnswered { request } if request.answer.as_ref().is_some_and(|a| a.accepted)
        )));
        let journaled = seen.iter().find_map(|kind| match kind {
            EventKind::JournalAppended { entry } => Some(entry.clone()),
            _ => None,
        });
        let journaled = journaled.expect("the release was journaled");
        assert_eq!(journaled.agent.as_ref(), Some(&holder.id));
        assert!(
            journaled
                .summary
                .contains("renamed the module (at asker's request)"),
            "{}",
            journaled.summary
        );

        let replies = inbox(&daemon, "asker").await;
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, RELEASE_RESPONSE_KIND);
        assert_eq!(replies[0].reply_to, Some(message.clone()));
        assert_eq!(replies[0].payload["accepted"], json!(true));
        assert_eq!(replies[0].payload["released"], json!([lease.id]));
        assert_eq!(replies[0].payload["summary"], json!("renamed the module"));
        assert!(
            replies[0].payload["text"]
                .as_str()
                .unwrap()
                .starts_with("holder released path:")
        );

        // Answered: gone.
        assert!(matches!(
            daemon
                .handle(Request::RespondRelease {
                    from: "holder".into(),
                    request: message,
                    accept: true,
                    text: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
    }

    /// A decline keeps the lease and tells the requester why; a resource
    /// nobody else holds, or only the requester holds, is `not_found`; a
    /// reason beyond the bound is refused before anything is sent.
    #[tokio::test]
    async fn a_decline_keeps_the_lease_and_carries_the_reason() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let project = dir.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        register(&daemon, "holder", &project).await;
        register(&daemon, "asker", &project).await;
        let lease = claim(&daemon, "holder", "branch:main").await;
        claim(&daemon, "asker", "task:AD-1").await;

        for (resource, reason, code) in [
            ("task:AD-1", None, ErrorCode::NotFound),
            ("task:nobody", None, ErrorCode::NotFound),
            ("branch:main", Some("x".repeat(401)), ErrorCode::Invalid),
        ] {
            let response = daemon
                .handle(Request::RequestRelease {
                    from: "asker".into(),
                    resource: resource.into(),
                    reason,
                })
                .await;
            assert!(
                matches!(&response, Response::Error { code: c, .. } if *c == code),
                "{resource}: {response:?}"
            );
        }
        assert!(inbox(&daemon, "holder").await.is_empty());

        let Response::ReleaseRequests { requests } = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: "branch:main".into(),
                reason: None,
            })
            .await
        else {
            panic!("request")
        };
        let message = requests[0].message.clone();
        let Response::ReleaseRequest { request } = daemon
            .handle(Request::RespondRelease {
                from: "holder".into(),
                request: message.clone(),
                accept: false,
                text: Some("mid-rebase, ten minutes".into()),
            })
            .await
        else {
            panic!("decline")
        };
        let answer = request.answer.unwrap();
        assert!(!answer.accepted);
        assert!(answer.released.is_empty());
        assert_eq!(leases(&daemon).await.len(), 2, "nothing was released");
        assert!(leases(&daemon).await.iter().any(|l| l.id == lease.id));
        let replies = inbox(&daemon, "asker").await;
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, RELEASE_RESPONSE_KIND);
        assert_eq!(replies[0].reply_to, Some(message));
        assert_eq!(replies[0].payload["accepted"], json!(false));
        assert_eq!(
            replies[0].payload["reason"],
            json!("mid-rebase, ten minutes")
        );
        assert_eq!(
            replies[0].payload["text"],
            json!("holder declined to release branch:main: mid-rebase, ten minutes")
        );
    }

    /// An accept is one transaction with its answer. A requester whose
    /// queue is full refuses the answer (`backpressure`), and nothing is
    /// released or journaled: the lease stays held and the request can be
    /// answered once there is room. A write that fails at the commit leaves
    /// the same: after a restart the lease is still held, the request still
    /// waits, and answering it then releases the lease, journaled once.
    #[tokio::test]
    async fn an_accept_releases_nothing_unless_its_answer_lands() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let project = dir.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        register(&daemon, "holder", &project).await;
        let asker = register(&daemon, "asker", &project).await;
        let lease = claim(&daemon, "holder", "branch:main").await;
        let Response::ReleaseRequests { requests } = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: "branch:main".into(),
                reason: None,
            })
            .await
        else {
            panic!("request")
        };
        let message = requests[0].message.clone();
        let accept = |daemon: Arc<Daemon>, message: MessageId| async move {
            daemon
                .handle(Request::RespondRelease {
                    from: "holder".into(),
                    request: message,
                    accept: true,
                    text: Some("done with it".into()),
                })
                .await
        };

        // The requester's queue is full.
        for n in 0..INBOX_CAPACITY {
            assert!(matches!(
                daemon
                    .handle(Request::Send {
                        from: agentdocker_core::HUMAN.into(),
                        to: "asker".into(),
                        kind: "chat".into(),
                        payload: json!({ "text": format!("filler {n}") }),
                        reply_to: None,
                        links: Vec::new(),
                    })
                    .await,
                Response::Sent { .. }
            ));
        }
        let mut events = daemon.subscribe_events();
        let refused = accept(daemon.clone(), message.clone()).await;
        assert!(
            matches!(
                refused,
                Response::Error {
                    code: ErrorCode::Backpressure,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert!(events.try_recv().is_err(), "nothing happened");
        assert!(leases(&daemon).await.iter().any(|l| l.id == lease.id));
        assert!(lock(&daemon.state).release_requests.contains_key(&message));

        // Room again: the same answer now lands.
        assert!(matches!(
            daemon
                .handle(Request::Inbox {
                    agent: asker.id.to_string(),
                    drain: true,
                })
                .await,
            Response::Messages { .. }
        ));

        // A write that fails at the commit: nothing moves, in memory or on
        // disk.
        lock(&daemon.state)
            .store
            .reject_event_for_test("release_answered");
        let failed = accept(daemon.clone(), message.clone()).await;
        assert!(matches!(failed, Response::Error { .. }), "{failed:?}");
        {
            let state = lock(&daemon.state);
            assert!(state.leases.get(&lease.id).is_some(), "still held");
            assert!(state.release_requests.contains_key(&message));
        }
        drop(daemon);
        let daemon = open(&dir);
        assert!(leases(&daemon).await.iter().any(|l| l.id == lease.id));
        assert!(lock(&daemon.state).release_requests.contains_key(&message));
        assert!(inbox(&daemon, "asker").await.is_empty());

        let mut events = daemon.subscribe_events();
        let Response::ReleaseRequest { request } = accept(daemon.clone(), message.clone()).await
        else {
            panic!("accept after the restart")
        };
        assert_eq!(request.answer.unwrap().released, vec![lease.id.clone()]);
        assert!(leases(&daemon).await.is_empty());
        let mut journaled = 0;
        let mut kinds = Vec::new();
        while let Ok(event) = events.try_recv() {
            if matches!(event.kind, EventKind::JournalAppended { .. }) {
                journaled += 1;
            }
            kinds.push(event.kind);
        }
        assert_eq!(journaled, 1, "one release, journaled once");
        let position = |want: fn(&EventKind) -> bool| kinds.iter().position(want).unwrap();
        assert!(
            position(|k| matches!(k, EventKind::LeaseReleased { .. }))
                < position(|k| matches!(k, EventKind::ReleaseAnswered { .. })),
            "the release, then its answer, in one commit"
        );
        let replies = inbox(&daemon, "asker").await;
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, RELEASE_RESPONSE_KIND);
        assert_eq!(replies[0].payload["released"], json!([lease.id]));
    }

    /// A request goes to every holder or to nobody: when one holder's
    /// queue is full the whole request is `backpressure`, nothing is
    /// recorded or announced, and the other holder is not asked alone.
    #[tokio::test]
    async fn a_request_goes_to_every_holder_or_to_none() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let project = dir.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        register(&daemon, "first", &project).await;
        let second = register(&daemon, "second", &project).await;
        register(&daemon, "asker", &project).await;
        for holder in ["first", "second"] {
            assert!(matches!(
                daemon
                    .handle(Request::Claim {
                        agent: holder.into(),
                        resource: "db:shared".into(),
                        mode: LeaseMode::Shared,
                        amount: None,
                        ttl_secs: 600,
                        note: None,
                        wait_secs: 0,
                        priority: 0,
                        automatic: false,
                    })
                    .await,
                Response::Lease { .. }
            ));
        }
        for n in 0..INBOX_CAPACITY {
            assert!(matches!(
                daemon
                    .handle(Request::Send {
                        from: agentdocker_core::HUMAN.into(),
                        to: second.id.to_string(),
                        kind: "chat".into(),
                        payload: json!({ "text": format!("filler {n}") }),
                        reply_to: None,
                        links: Vec::new(),
                    })
                    .await,
                Response::Sent { .. }
            ));
        }
        let mut events = daemon.subscribe_events();
        let refused = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: "db:shared".into(),
                reason: None,
            })
            .await;
        assert!(
            matches!(
                refused,
                Response::Error {
                    code: ErrorCode::Backpressure,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert!(events.try_recv().is_err(), "nothing was announced");
        assert!(lock(&daemon.state).release_requests.is_empty());
        assert!(inbox(&daemon, "first").await.is_empty(), "not asked alone");

        // Room again: both are asked.
        assert!(matches!(
            daemon
                .handle(Request::Inbox {
                    agent: second.id.to_string(),
                    drain: true,
                })
                .await,
            Response::Messages { .. }
        ));
        let Response::ReleaseRequests { requests } = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: "db:shared".into(),
                reason: None,
            })
            .await
        else {
            panic!("request")
        };
        assert_eq!(requests.len(), 2);
        assert_eq!(inbox(&daemon, "first").await.len(), 1);
        assert_eq!(inbox(&daemon, "second").await.len(), 1);
    }

    /// A lease past its TTL has expired, whatever the reaper has got to:
    /// an accept in that window releases nothing, the lease leaves as
    /// `lease_expired`, nothing is journaled, and the answer says nothing
    /// was still held.
    #[tokio::test]
    async fn an_accept_after_the_ttl_finds_the_lease_expired_not_held() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let project = dir.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        let holder = register(&daemon, "holder", &project).await;
        register(&daemon, "asker", &project).await;
        let lease = claim(&daemon, "holder", "branch:main").await;
        let Response::ReleaseRequests { requests } = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: "branch:main".into(),
                reason: None,
            })
            .await
        else {
            panic!("request")
        };
        // The TTL passes before the reaper's next tick.
        {
            let mut state = lock(&daemon.state);
            let held = state.leases.release(&lease.id, &holder.id).unwrap();
            state.leases.restore(Lease {
                expires_at: Utc::now() - Duration::seconds(1),
                ..held
            });
        }
        let mut events = daemon.subscribe_events();
        let Response::ReleaseRequest { request } = daemon
            .handle(Request::RespondRelease {
                from: "holder".into(),
                request: requests[0].message.clone(),
                accept: true,
                text: Some("all yours".into()),
            })
            .await
        else {
            panic!("accept")
        };
        assert!(request.answer.unwrap().released.is_empty());
        let mut kinds = Vec::new();
        while let Ok(event) = events.try_recv() {
            kinds.push(event.kind);
        }
        assert!(
            kinds
                .iter()
                .any(|k| matches!(k, EventKind::LeaseExpired { lease: l } if l.id == lease.id)),
            "{kinds:?}"
        );
        assert!(
            !kinds
                .iter()
                .any(|k| matches!(k, EventKind::LeaseReleased { .. }))
        );
        assert!(
            !kinds
                .iter()
                .any(|k| matches!(k, EventKind::JournalAppended { .. }))
        );
        assert!(leases(&daemon).await.is_empty());
        let replies = inbox(&daemon, "asker").await;
        assert_eq!(replies[0].payload["released"], json!([]));
        assert!(
            replies[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("nothing was still held")
        );
    }

    /// An accept after the lease has gone on its own releases nothing and
    /// says so; a request that waits its day out is dropped with an event
    /// and cannot be answered; another project's holder is out of reach.
    #[tokio::test]
    async fn a_lapsed_lease_an_expired_request_and_another_project() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let here = dir.path().join("here");
        let there = dir.path().join("there");
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        register(&daemon, "holder", &here).await;
        register(&daemon, "asker", &here).await;
        register(&daemon, "stranger", &there).await;
        let lease = claim(&daemon, "holder", "branch:main").await;
        claim(&daemon, "stranger", "db:shared").await;

        assert!(matches!(
            daemon
                .handle(Request::RequestRelease {
                    from: "asker".into(),
                    resource: "db:shared".into(),
                    reason: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::Forbidden,
                ..
            }
        ));

        let Response::ReleaseRequests { requests } = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: "branch:main".into(),
                reason: None,
            })
            .await
        else {
            panic!("request")
        };
        let message = requests[0].message.clone();
        // The holder lets go itself first.
        assert!(matches!(
            daemon
                .handle(Request::Release {
                    agent: "holder".into(),
                    lease: lease.id.clone(),
                    summary: Some("done".into()),
                    summary_source: SummarySource::Explicit,
                })
                .await,
            Response::Lease { .. }
        ));
        let mut events = daemon.subscribe_events();
        let Response::ReleaseRequest { request } = daemon
            .handle(Request::RespondRelease {
                from: "holder".into(),
                request: message,
                accept: true,
                text: Some("nothing left".into()),
            })
            .await
        else {
            panic!("accept")
        };
        assert!(request.answer.unwrap().released.is_empty());
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event.kind, EventKind::JournalAppended { .. }),
                "releasing nothing journals nothing"
            );
        }
        let replies = inbox(&daemon, "asker").await;
        assert!(
            replies[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("nothing was still held")
        );

        // A request left unanswered for a day is dropped.
        claim(&daemon, "holder", "branch:main").await;
        let Response::ReleaseRequests { requests } = daemon
            .handle(Request::RequestRelease {
                from: "asker".into(),
                resource: "branch:main".into(),
                reason: None,
            })
            .await
        else {
            panic!("request")
        };
        let message = requests[0].message.clone();
        {
            let mut state = lock(&daemon.state);
            let request = state.release_requests.get_mut(&message).unwrap();
            request.expires_at = Utc::now() - Duration::seconds(1);
        }
        let mut events = daemon.subscribe_events();
        assert!(matches!(
            daemon
                .handle(Request::RespondRelease {
                    from: "holder".into(),
                    request: message.clone(),
                    accept: true,
                    text: None,
                })
                .await,
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            }
        ));
        assert!(lock(&daemon.state).release_requests.is_empty());
        let mut expired = false;
        while let Ok(event) = events.try_recv() {
            expired |= matches!(event.kind, EventKind::ReleaseRequestExpired { request } if request.message == message);
        }
        assert!(expired, "the expiry was announced");
        drop(daemon);
        let daemon = open(&dir);
        assert!(
            lock(&daemon.state).release_requests.is_empty(),
            "and is durable"
        );
    }
}
