//! Daemon state and request handling.
//!
//! Locking discipline: every method locks at most one mutex at a time, so
//! there is no lock ordering to get wrong. Locks are `std::sync::Mutex`
//! because no lock is ever held across an `.await`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use agentdocker_core::{
    AgentId, AgentRecord, AgentSpec, AgentStatus, Claimed, Destination, Envelope, ErrorCode, Event,
    EventKind, Lease, LeaseError, LeaseId, LeaseMode, LeaseTable, MessageId, Registry,
    RegistryError, Request, ResourceKey, Response, topic_matches,
};
use chrono::{Duration, Utc};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::supervisor;

/// Messages queued per agent while it has no live subscription.
const INBOX_CAPACITY: usize = 1000;
/// Leases longer than this are clamped; a TTL is a liveness bound, not a
/// reservation.
const MAX_LEASE_TTL_SECS: u64 = 24 * 60 * 60;

pub struct Daemon {
    pub home: PathBuf,
    pub socket: PathBuf,
    started: Instant,
    registry: Mutex<Registry>,
    leases: Mutex<LeaseTable>,
    inboxes: Mutex<HashMap<AgentId, VecDeque<Envelope>>>,
    /// Number of live subscriptions per agent. Agents with one or more skip
    /// the inbox and get messages pushed directly.
    live_subscribers: Mutex<HashMap<AgentId, usize>>,
    bus: broadcast::Sender<Envelope>,
    events: broadcast::Sender<Event>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn registry_error(err: RegistryError) -> Response {
    let code = match err {
        RegistryError::NameTaken(_) => ErrorCode::NameTaken,
        RegistryError::NotFound(_) => ErrorCode::NotFound,
        RegistryError::Ambiguous(_) => ErrorCode::Ambiguous,
    };
    Response::error(code, err.to_string())
}

fn lease_error(err: LeaseError) -> Response {
    let code = match err {
        LeaseError::Conflict { .. } => ErrorCode::Conflict,
        LeaseError::NotFound(_) => ErrorCode::NotFound,
        LeaseError::NotHolder { .. } => ErrorCode::Forbidden,
    };
    Response::error(code, err.to_string())
}

fn ttl(secs: u64) -> Duration {
    let secs = i64::try_from(secs.min(MAX_LEASE_TTL_SECS)).unwrap_or(i64::MAX);
    Duration::seconds(secs)
}

fn default_name(id: &AgentId) -> String {
    format!("agent-{}", &id.as_str()[..6])
}

impl Daemon {
    pub fn new(home: PathBuf, socket: PathBuf) -> Self {
        let (bus, _) = broadcast::channel(1024);
        let (events, _) = broadcast::channel(1024);
        Self {
            home,
            socket,
            started: Instant::now(),
            registry: Mutex::new(Registry::new()),
            leases: Mutex::new(LeaseTable::new()),
            inboxes: Mutex::new(HashMap::new()),
            live_subscribers: Mutex::new(HashMap::new()),
            bus,
            events,
        }
    }

    pub fn log_path(&self, id: &AgentId) -> PathBuf {
        self.home.join("logs").join(format!("{id}.log"))
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn emit(&self, kind: EventKind) {
        let _ = self.events.send(Event::new(kind, Utc::now()));
    }

    pub fn resolve(&self, reference: &str) -> Result<AgentId, Box<Response>> {
        lock(&self.registry)
            .resolve(reference)
            .map_err(|err| Box::new(registry_error(err)))
    }

    pub fn is_live(&self, id: &AgentId) -> bool {
        lock(&self.registry)
            .get(id)
            .is_some_and(|a| a.status.is_live())
    }

    /// Handle every non-streaming request.
    pub async fn handle(self: &Arc<Self>, request: Request) -> Response {
        match request {
            Request::Ping => Response::Pong {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                uptime_secs: self.started.elapsed().as_secs(),
            },
            Request::Run { spec } => self.run(spec).await,
            Request::Register { spec, pid } => self.register(spec, pid),
            Request::Deregister { agent } => self.deregister(&agent),
            Request::Stop { agent, force } => self.stop(&agent, force),
            Request::Remove { agent } => self.remove(&agent),
            Request::List { all } => Response::Agents {
                agents: lock(&self.registry).list(all),
            },
            Request::Inspect { agent } => self.inspect(&agent),
            Request::Heartbeat { agent } => match self.resolve(&agent) {
                Ok(id) => {
                    self.touch(&id);
                    Response::Ok
                }
                Err(response) => *response,
            },
            Request::Send {
                from,
                to,
                kind,
                payload,
                reply_to,
            } => self.send(from, &to, kind, payload, reply_to),
            Request::Inbox { agent, drain } => self.inbox(&agent, drain),
            Request::Claim {
                agent,
                resource,
                mode,
                ttl_secs,
                note,
            } => self.claim(&agent, resource, mode, ttl_secs, note),
            Request::Renew {
                agent,
                lease,
                ttl_secs,
            } => self.renew(&agent, &lease, ttl_secs),
            Request::Release { agent, lease } => self.release(&agent, &lease),
            Request::Leases { agent, resource } => self.leases(agent.as_deref(), resource),
            Request::Subscribe { .. } | Request::Events | Request::Logs { .. } => {
                Response::error(ErrorCode::Internal, "streaming request routed as unary")
            }
        }
    }

    async fn run(self: &Arc<Self>, spec: AgentSpec) -> Response {
        if spec.command.is_empty() {
            return Response::error(ErrorCode::Invalid, "run needs a command to launch");
        }
        let mut record = AgentRecord::new(spec, true, Utc::now());
        if record.spec.name.is_empty() {
            record.spec.name = default_name(&record.id);
        }
        if let Err(err) = lock(&self.registry).insert(record.clone()) {
            return registry_error(err);
        }
        self.emit(EventKind::AgentCreated {
            agent: record.id.clone(),
            name: record.spec.name.clone(),
        });

        match supervisor::spawn(self, &record).await {
            Ok(spawned) => {
                let pid = spawned.pid;
                let updated = {
                    let mut registry = lock(&self.registry);
                    if let Some(rec) = registry.get_mut(&record.id) {
                        rec.pid = Some(pid);
                    }
                    registry.set_status(&record.id, AgentStatus::Running, Utc::now())
                };
                self.emit(EventKind::AgentStarted {
                    agent: record.id.clone(),
                    pid: Some(pid),
                });
                supervisor::supervise(self.clone(), record.id.clone(), spawned);
                info!(agent = %record.id.short(), name = %record.spec.name, pid, "agent started");
                match updated {
                    Some(agent) => Response::Agent { agent },
                    None => Response::error(ErrorCode::Internal, "agent vanished after spawn"),
                }
            }
            Err(err) => {
                let status = AgentStatus::Failed {
                    reason: format!("{err:#}"),
                };
                lock(&self.registry).set_status(&record.id, status.clone(), Utc::now());
                self.emit(EventKind::AgentExited {
                    agent: record.id,
                    status,
                });
                Response::error(ErrorCode::Internal, format!("{err:#}"))
            }
        }
    }

    fn register(&self, spec: AgentSpec, pid: Option<u32>) -> Response {
        let now = Utc::now();
        let mut record = AgentRecord::new(spec, false, now);
        if record.spec.name.is_empty() {
            record.spec.name = default_name(&record.id);
        }
        record.pid = pid;
        record.status = AgentStatus::Running;
        record.started_at = Some(now);
        if let Err(err) = lock(&self.registry).insert(record.clone()) {
            return registry_error(err);
        }
        self.emit(EventKind::AgentCreated {
            agent: record.id.clone(),
            name: record.spec.name.clone(),
        });
        self.emit(EventKind::AgentStarted {
            agent: record.id.clone(),
            pid,
        });
        info!(agent = %record.id.short(), name = %record.spec.name, "agent registered");
        Response::Agent { agent: record }
    }

    fn deregister(&self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if !self.is_live(&id) {
            return Response::error(ErrorCode::Invalid, "agent has already finished");
        }
        match self.mark_exited(&id, AgentStatus::Exited { code: Some(0) }) {
            Some(agent) => Response::Agent { agent },
            None => Response::error(ErrorCode::NotFound, "agent vanished"),
        }
    }

    fn stop(&self, reference: &str, force: bool) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let Some(record) = lock(&self.registry).get(&id).cloned() else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        if !record.status.is_live() {
            return Response::error(
                ErrorCode::Invalid,
                format!("agent is already {}", record.status),
            );
        }
        if let Some(pid) = record.pid {
            let signal = if force {
                Signal::SIGKILL
            } else {
                Signal::SIGTERM
            };
            if let Err(err) = kill(Pid::from_raw(pid as i32), signal) {
                return Response::error(
                    ErrorCode::Internal,
                    format!("failed to signal pid {pid}: {err}"),
                );
            }
        } else if record.managed {
            return Response::error(ErrorCode::Internal, "managed agent has no pid");
        }
        if record.managed {
            // The supervisor task observes the exit and updates the record.
            return Response::Agent { agent: record };
        }
        match self.mark_exited(&id, AgentStatus::Exited { code: None }) {
            Some(agent) => Response::Agent { agent },
            None => Response::error(ErrorCode::NotFound, "agent vanished"),
        }
    }

    /// SIGTERM every managed live agent; used on daemon shutdown.
    pub fn stop_all(&self) {
        let managed: Vec<(AgentId, u32)> = lock(&self.registry)
            .live()
            .filter(|a| a.managed)
            .filter_map(|a| a.pid.map(|pid| (a.id.clone(), pid)))
            .collect();
        for (id, pid) in managed {
            info!(agent = %id.short(), pid, "stopping agent");
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
    }

    fn remove(&self, reference: &str) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if self.is_live(&id) {
            return Response::error(ErrorCode::Invalid, "agent is still live; stop it first");
        }
        lock(&self.registry).remove(&id);
        lock(&self.inboxes).remove(&id);
        self.emit(EventKind::AgentRemoved { agent: id });
        Response::Ok
    }

    fn inspect(&self, reference: &str) -> Response {
        match self.resolve(reference) {
            Ok(id) => match lock(&self.registry).get(&id) {
                Some(agent) => Response::Agent {
                    agent: agent.clone(),
                },
                None => Response::error(ErrorCode::NotFound, "agent vanished"),
            },
            Err(response) => *response,
        }
    }

    /// Record an exit: update status, release leases, emit the event.
    pub fn mark_exited(&self, id: &AgentId, status: AgentStatus) -> Option<AgentRecord> {
        let record = lock(&self.registry).set_status(id, status.clone(), Utc::now())?;
        for lease in lock(&self.leases).release_all(id) {
            self.emit(EventKind::LeaseReleased { lease });
        }
        info!(agent = %id.short(), name = %record.spec.name, %status, "agent finished");
        self.emit(EventKind::AgentExited {
            agent: id.clone(),
            status,
        });
        Some(record)
    }

    fn touch(&self, id: &AgentId) {
        lock(&self.registry).touch(id, Utc::now());
    }

    fn send(
        &self,
        from: String,
        to: &str,
        kind: String,
        payload: Value,
        reply_to: Option<MessageId>,
    ) -> Response {
        let from = match lock(&self.registry).resolve(&from) {
            Ok(id) => id.to_string(),
            Err(RegistryError::NotFound(_)) => from,
            Err(err) => return registry_error(err),
        };
        let to = match Destination::parse(to) {
            Destination::Agent(reference) => match self.resolve(reference.as_str()) {
                Ok(id) => Destination::Agent(id),
                Err(response) => return *response,
            },
            other => other,
        };
        let envelope = Envelope::new(from, to, kind, payload, reply_to, Utc::now());

        let recipients: Vec<AgentId> = match &envelope.to {
            Destination::Agent(id) => vec![id.clone()],
            Destination::Broadcast => lock(&self.registry)
                .live()
                .filter(|a| a.id.as_str() != envelope.from)
                .map(|a| a.id.clone())
                .collect(),
            Destination::Topic(_) => Vec::new(),
        };
        let offline: Vec<AgentId> = {
            let live = lock(&self.live_subscribers);
            recipients
                .into_iter()
                .filter(|id| !live.contains_key(id))
                .collect()
        };
        if !offline.is_empty() {
            let mut inboxes = lock(&self.inboxes);
            for id in offline {
                let queue = inboxes.entry(id).or_default();
                if queue.len() >= INBOX_CAPACITY {
                    queue.pop_front();
                }
                queue.push_back(envelope.clone());
            }
        }

        let subscribers = self.bus.send(envelope.clone()).unwrap_or(0);
        self.emit(EventKind::MessageSent {
            message: envelope.id.clone(),
            from: envelope.from.clone(),
            to: envelope.to.clone(),
            kind: envelope.kind.clone(),
        });
        self.touch(&AgentId::from(envelope.from.as_str()));
        Response::Sent {
            message: envelope.id,
            subscribers,
        }
    }

    fn inbox(&self, reference: &str, drain: bool) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let messages: Vec<Envelope> = {
            let mut inboxes = lock(&self.inboxes);
            if drain {
                inboxes.remove(&id).map(Vec::from).unwrap_or_default()
            } else {
                inboxes
                    .get(&id)
                    .map(|queue| queue.iter().cloned().collect())
                    .unwrap_or_default()
            }
        };
        self.touch(&id);
        Response::Messages { messages }
    }

    /// Open a live subscription. Returns the filter plus the raw receiver so
    /// the caller can `select!` on the receiver without borrowing the filter.
    pub fn subscribe(
        self: &Arc<Self>,
        agent: Option<&str>,
        topics: Vec<String>,
    ) -> Result<(Subscription, broadcast::Receiver<Envelope>), Box<Response>> {
        let agent = agent.map(|reference| self.resolve(reference)).transpose()?;
        let receiver = self.bus.subscribe();
        let backlog = match &agent {
            Some(id) => {
                *lock(&self.live_subscribers).entry(id.clone()).or_default() += 1;
                lock(&self.inboxes)
                    .remove(id)
                    .map(Vec::from)
                    .unwrap_or_default()
            }
            None => Vec::new(),
        };
        let seen = backlog.iter().map(|m| m.id.clone()).collect();
        Ok((
            Subscription {
                daemon: self.clone(),
                agent,
                topics,
                backlog,
                seen,
            },
            receiver,
        ))
    }

    fn unsubscribe(&self, agent: &AgentId) {
        let mut live = lock(&self.live_subscribers);
        if let Some(count) = live.get_mut(agent) {
            *count -= 1;
            if *count == 0 {
                live.remove(agent);
            }
        }
    }

    fn claim(
        &self,
        reference: &str,
        resource: String,
        mode: LeaseMode,
        ttl_secs: u64,
        note: Option<String>,
    ) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.touch(&holder);
        let result = lock(&self.leases).claim(
            ResourceKey::new(resource),
            holder.clone(),
            mode,
            ttl(ttl_secs),
            note,
            Utc::now(),
        );
        match result {
            Ok(Claimed::New(lease)) => {
                self.emit(EventKind::LeaseClaimed {
                    lease: lease.clone(),
                });
                Response::Lease { lease }
            }
            Ok(Claimed::Renewed(lease)) => {
                self.emit(EventKind::LeaseRenewed {
                    lease: lease.clone(),
                });
                Response::Lease { lease }
            }
            Err(err) => {
                let message = err.to_string();
                match err {
                    LeaseError::Conflict { resource, held_by } => {
                        warn!(agent = %holder.short(), %resource, "lease conflict");
                        self.emit(EventKind::LeaseConflict {
                            resource,
                            requester: holder,
                            held_by: held_by.iter().map(|l| l.holder.clone()).collect(),
                        });
                        Response::Error {
                            code: ErrorCode::Conflict,
                            message,
                            details: Some(json!({ "held_by": held_by })),
                        }
                    }
                    other => lease_error(other),
                }
            }
        }
    }

    fn renew(&self, reference: &str, lease: &LeaseId, ttl_secs: u64) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        self.touch(&holder);
        match lock(&self.leases).renew(lease, &holder, ttl(ttl_secs), Utc::now()) {
            Ok(lease) => {
                self.emit(EventKind::LeaseRenewed {
                    lease: lease.clone(),
                });
                Response::Lease { lease }
            }
            Err(err) => lease_error(err),
        }
    }

    fn release(&self, reference: &str, lease: &LeaseId) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        match lock(&self.leases).release(lease, &holder) {
            Ok(lease) => {
                self.emit(EventKind::LeaseReleased {
                    lease: lease.clone(),
                });
                Response::Lease { lease }
            }
            Err(err) => lease_error(err),
        }
    }

    fn leases(&self, agent: Option<&str>, resource: Option<String>) -> Response {
        let holder = match agent.map(|reference| self.resolve(reference)).transpose() {
            Ok(holder) => holder,
            Err(response) => return *response,
        };
        let key = resource.map(ResourceKey::new);
        self.expire_leases();
        let leases: Vec<Lease> = lock(&self.leases)
            .all()
            .into_iter()
            .filter(|l| holder.as_ref().is_none_or(|h| l.holder == *h))
            .filter(|l| key.as_ref().is_none_or(|k| l.resource.overlaps(k)))
            .cloned()
            .collect();
        Response::Leases { leases }
    }

    /// Drop leases whose TTL has passed. Called on a timer and before listing.
    pub fn expire_leases(&self) {
        let expired = lock(&self.leases).expire(Utc::now());
        for lease in expired {
            info!(lease = %lease.id, holder = %lease.holder.short(), resource = %lease.resource, "lease expired");
            self.emit(EventKind::LeaseExpired { lease });
        }
    }
}

/// A live message subscription. Dropping it releases the agent's live slot
/// so later messages queue in its inbox again.
pub struct Subscription {
    daemon: Arc<Daemon>,
    agent: Option<AgentId>,
    topics: Vec<String>,
    backlog: Vec<Envelope>,
    seen: HashSet<MessageId>,
}

impl Subscription {
    /// Messages that were queued while the agent was offline.
    pub fn take_backlog(&mut self) -> Vec<Envelope> {
        std::mem::take(&mut self.backlog)
    }

    pub fn wants(&self, envelope: &Envelope) -> bool {
        if self.seen.contains(&envelope.id) {
            return false;
        }
        match &envelope.to {
            Destination::Agent(id) => self.agent.as_ref() == Some(id),
            Destination::Broadcast => self
                .agent
                .as_ref()
                .is_none_or(|me| me.as_str() != envelope.from),
            Destination::Topic(topic) => self
                .topics
                .iter()
                .any(|pattern| topic_matches(pattern, topic)),
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(agent) = &self.agent {
            self.daemon.unsubscribe(agent);
        }
    }
}
