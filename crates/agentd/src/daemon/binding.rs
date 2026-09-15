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
//!
//! A controller that ends leaves an idle provider with no hook to start it
//! again, so a binding may carry a launch descriptor: the exact command
//! the controller was started with. Once a second the daemon looks at the
//! bound controller by pid and birth; when it is gone the daemon starts
//! the descriptor again, a bounded number of times with backoff, and the
//! new process binds itself with the same token. The daemon restarts
//! receivers, never provider sessions, and never rebinds anything itself.
use std::collections::HashSet;

use agentdocker_core::{
    AgentId, AgentRecord, ControllerLaunch, ControllerStep, ErrorCode, EventKind, InputBinding,
    MessageId, ProcessIdentity, ProviderGeneration, Response,
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

/// Signal a process the daemon launched, if it is still the one launched.
fn signal(process: &ProcessIdentity, signal: Signal) {
    if is_running(process) {
        let _ = kill(Pid::from_raw(process.pid as i32), signal);
    }
}

/// Delivery evidence stops at the controller's end: whatever it last
/// reported, nothing receives input now.
fn pause_delivery(record: &mut AgentRecord, reason: &str, now: DateTime<Utc>) {
    let delivery = record
        .input_delivery
        .get_or_insert_with(|| agentdocker_core::InputDelivery {
            process_started_at: record.process_started_at.unwrap_or(now),
            paused: false,
            pause_reason: None,
            reported_at: now,
            received: None,
            received_at: None,
        });
    delivery.paused = true;
    delivery.pause_reason = Some(reason.to_owned());
    delivery.reported_at = now;
}

/// Where a launched controller's output goes: appended, next to the
/// daemon's other logs, so a controller that keeps dying leaves a trace.
fn controller_log(home: &Path, id: &AgentId) -> PathBuf {
    home.join("logs").join(format!("{id}.controller.log"))
}

/// Start a launch descriptor, detached: its own process group, nothing on
/// stdin, output appended to the controller log, the daemon's environment
/// plus the descriptor's, and `AGENTDOCKER_HOME` pointing at this daemon
/// so the new process binds here. Tokio reaps the child in the background.
fn spawn_controller(
    home: &Path,
    id: &AgentId,
    launch: &ControllerLaunch,
) -> Result<ProcessIdentity, String> {
    let path = controller_log(home, id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("controller log: {e}"))?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("controller log: {e}"))?;
    let stderr = log
        .try_clone()
        .map_err(|e| format!("controller log: {e}"))?;
    let mut command = tokio::process::Command::new(&launch.executable);
    command
        .args(&launch.args)
        .current_dir(&launch.cwd)
        .envs(&launch.env)
        .env("AGENTDOCKER_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(stderr))
        .process_group(0);
    let child = command
        .spawn()
        .map_err(|e| format!("{}: {e}", launch.executable.display()))?;
    let pid = child
        .id()
        .ok_or_else(|| "the process ended at once".to_owned())?;
    let started_at = agentdocker_host::procinfo::start_time(pid)
        .ok_or_else(|| format!("pid {pid}: start time unreadable"))?;
    Ok(ProcessIdentity { pid, started_at })
}

impl Daemon {
    /// Once a second: note a bound controller that ended and, when the
    /// binding says how, start it again. Liveness is read off the lock;
    /// each transition re-reads the binding under it and skips when a
    /// bind or unbind changed it in between. A launch happens under the
    /// lock too, so the process is recorded before it can bind.
    pub fn tend_controllers(&self) {
        // Pins first: a daemon that restored bindings holds their releases
        // before it could start anything from them.
        self.pin_controllers();
        let bindings: Vec<(AgentId, InputBinding)> = {
            let state = lock(&self.state);
            state
                .registry
                .list(true)
                .into_iter()
                .filter_map(|r| r.input_binding.clone().map(|b| (r.id.clone(), b)))
                .collect()
        };
        for (id, binding) in bindings {
            let now = Utc::now();
            match binding.controller_step(now, is_running) {
                ControllerStep::Keep => {}
                ControllerStep::Ended => {
                    let controller = binding
                        .restart
                        .launched
                        .clone()
                        .unwrap_or_else(|| binding.controller.clone());
                    let mut state = lock(&self.state);
                    state.transition_binding(&id, &binding, now, |record| {
                        // A fresh heartbeat from the controller that just
                        // died must not keep showing delivery as verified.
                        pause_delivery(record, "the bound controller ended", now);
                        record
                            .input_binding
                            .as_mut()
                            .expect("checked")
                            .note_controller_ended(now);
                        EventKind::InputControllerEnded {
                            agent: id.clone(),
                            controller,
                        }
                    });
                }
                ControllerStep::Exhausted => {
                    let mut state = lock(&self.state);
                    state.transition_binding(&id, &binding, now, |record| {
                        pause_delivery(record, "the bound controller could not be restarted", now);
                        let binding = record.input_binding.as_mut().expect("checked");
                        binding.restart.exhausted = true;
                        EventKind::InputRestartsExhausted {
                            agent: id.clone(),
                            attempts: binding.restart.attempts,
                        }
                    });
                }
                ControllerStep::Terminate { kill: hard } => {
                    if let Some(launched) = &binding.restart.launched {
                        warn!(agent = %id, pid = launched.pid, "launched controller did not bind in time");
                        signal(
                            launched,
                            if hard {
                                Signal::SIGKILL
                            } else {
                                Signal::SIGTERM
                            },
                        );
                    }
                }
                ControllerStep::Launch { attempt } => {
                    let Some(launch) = binding.launch.clone() else {
                        continue;
                    };
                    // Under one guard: the binding is still the one decided
                    // on, the release is held, the process is started and
                    // the launch recorded. A child that binds at once finds
                    // itself already noted as the launched process, so its
                    // bind resumes the binding rather than overtaking it.
                    let mut state = lock(&self.state);
                    if state
                        .registry
                        .get(&id)
                        .and_then(|r| r.input_binding.as_ref())
                        != Some(&binding)
                    {
                        continue;
                    }
                    let spawned = state
                        .pin_controller(&id, &launch)
                        .and_then(|()| spawn_controller(&self.home, &id, &launch));
                    let recorded = state.transition_binding(&id, &binding, now, |record| {
                        let b = record.input_binding.as_mut().expect("checked");
                        b.note_launch(spawned.as_ref().ok().cloned(), now);
                        match &spawned {
                            Ok(controller) => EventKind::InputControllerLaunched {
                                agent: id.clone(),
                                controller: controller.clone(),
                                attempt,
                            },
                            Err(error) => EventKind::InputControllerLaunchFailed {
                                agent: id.clone(),
                                attempt,
                                error: error.clone(),
                            },
                        }
                    });
                    match &spawned {
                        Ok(controller) if recorded => {
                            info!(agent = %id, pid = controller.pid, attempt, "launched the bound controller");
                        }
                        Ok(controller) => {
                            // Storage refused the record: nothing says this
                            // process was launched, so it must not stay.
                            signal(controller, Signal::SIGTERM);
                        }
                        Err(error) => {
                            warn!(agent = %id, attempt, %error, "could not launch the bound controller");
                        }
                    }
                }
            }
        }
    }

    /// Hold a shared installation pin on every launch descriptor's
    /// executable, and only those: a binding that ended drops its pin.
    /// Called before serving and on every tick; a pin that cannot be
    /// taken is logged here and refused at the launch that needs it.
    pub fn pin_controllers(&self) {
        let wanted: Vec<(AgentId, ControllerLaunch)> = {
            let state = lock(&self.state);
            state
                .registry
                .list(true)
                .into_iter()
                .filter_map(|r| {
                    r.input_binding
                        .as_ref()
                        .and_then(|b| b.launch.clone())
                        .filter(|_| !state.controller_pins.contains_key(&r.id))
                        .map(|launch| (r.id.clone(), launch))
                })
                .collect()
        };
        let mut state = lock(&self.state);
        for (id, launch) in wanted {
            if let Err(error) = state.pin_controller(&id, &launch) {
                warn!(agent = %id, %error, "could not pin the controller's release");
            }
        }
        let managed: HashSet<AgentId> = state
            .registry
            .list(true)
            .into_iter()
            .filter(|r| r.input_binding.as_ref().is_some_and(|b| b.launch.is_some()))
            .map(|r| r.id.clone())
            .collect();
        state.controller_pins.retain(|id, _| managed.contains(id));
    }
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
        let Some(mut record) = self.registry.get(id).cloned() else {
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
        if !changed {
            return;
        }
        // Stored first, then in memory, so the two never disagree about
        // what is still uncertain.
        self.persist("input bookkeeping", |store| store.upsert_agent(&record));
        if self.storage_error.is_none() {
            *self.registry.get_mut(id).expect("resolved agent") = record;
        }
    }

    /// Hold the release a launch descriptor's executable belongs to, if it
    /// is inside a managed installation and not held already. An error
    /// means the release is being removed or is gone: nothing may be
    /// started from it.
    fn pin_controller(&mut self, id: &AgentId, launch: &ControllerLaunch) -> Result<(), String> {
        if self.controller_pins.contains_key(id) {
            return Ok(());
        }
        match agentdocker_host::installation::pin_executable(&launch.executable) {
            Ok(Some(pin)) => {
                self.controller_pins.insert(id.clone(), pin);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) => Err(format!(
                "release of {}: {error}",
                launch.executable.display()
            )),
        }
    }

    /// One restart transition, applied only when the binding is still the
    /// one the decision was made from. Returns whether it was applied.
    fn transition_binding(
        &mut self,
        id: &AgentId,
        expected: &InputBinding,
        now: DateTime<Utc>,
        change: impl FnOnce(&mut AgentRecord) -> EventKind,
    ) -> bool {
        let Some(record) = self.registry.get(id) else {
            return false;
        };
        if record.input_binding.as_ref() != Some(expected) {
            return false;
        }
        let mut record = record.clone();
        let mut event = Event::new(change(&mut record), now);
        event.seq = self.next_seq;
        self.persist("controller restart", |store| {
            store.agent_transition(&record, &event)
        });
        if self.storage_error.is_some() {
            return false;
        }
        *self.registry.get_mut(id).expect("resolved agent") = record;
        self.next_seq += 1;
        let _ = self.events.send(event);
        true
    }

    pub(super) fn bind_input(
        &mut self,
        reference: &str,
        provider: ProviderGeneration,
        controller: ProcessIdentity,
        token: &str,
        launch: Option<ControllerLaunch>,
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
        if launch.as_ref().is_some_and(|l| !l.valid()) {
            return Response::error(
                ErrorCode::Invalid,
                "a launch descriptor needs an absolute executable and working directory, and bounded arguments and environment",
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
                controller_since: now,
                token_sha256: digest,
                bound_at: now,
                controller_generations: 1,
                // Whatever a legacy reader was already offered may be inside
                // the provider already; the controller finds out, not us.
                uncertain: record.legacy_offers.keys().cloned().collect(),
                launch,
                restart: Default::default(),
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
                if launch.is_some() && launch != existing.launch {
                    return Response::error(
                        ErrorCode::Invalid,
                        "the launch descriptor is fixed for the binding's life; unbind first to change it",
                    );
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
                // A process the daemon launched that is not the one binding
                // has been overtaken; it would be refused from here on.
                if let Some(launched) = &existing.restart.launched
                    && *launched != controller
                {
                    signal(launched, Signal::SIGTERM);
                }
                resumed = true;
                let mut resumed_binding = existing.clone();
                resumed_binding.note_controller_bound(controller.clone(), now);
                resumed_binding.controller_generations =
                    existing.controller_generations.saturating_add(1);
                resumed_binding
            }
        };
        let mut record = record.clone();
        // The release the controller runs from stays installed for as
        // long as the daemon may have to start it again: held before the
        // binding is accepted, and refused when it cannot be.
        if let Some(launch) = &binding.launch
            && let Err(error) = self.pin_controller(&id, launch)
        {
            return Response::error(
                ErrorCode::Invalid,
                format!("the launch descriptor's release cannot be held: {error}"),
            );
        }
        // Uncertainty is only ever about messages still queued.
        let queued: HashSet<&MessageId> = self
            .inboxes
            .get(&id)
            .into_iter()
            .flatten()
            .map(|m| &m.id)
            .collect();
        binding.uncertain.retain(|m| queued.contains(m));
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
        // A process the daemon launched that has not bound yet has nothing
        // left to bind to.
        let launched = binding.restart.launched.clone();
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
        self.controller_pins.remove(&id);
        if let Some(launched) = launched {
            signal(&launched, Signal::SIGTERM);
        }
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
            launch: None,
        }
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const OTHER_TOKEN: &str = "fedcba9876543210fedcba9876543210";

    /// A descriptor that leaves a mark when it runs and then stays alive
    /// like a receiver waiting for input would.
    fn descriptor(dir: &TempDir) -> ControllerLaunch {
        ControllerLaunch {
            executable: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "echo launched >> \"$MARK\"; exec sleep 30".into(),
            ],
            cwd: dir.path().to_path_buf(),
            env: [(
                "MARK".to_owned(),
                dir.path().join("mark").display().to_string(),
            )]
            .into_iter()
            .collect(),
        }
    }

    fn binding_of(daemon: &Arc<Daemon>, id: &AgentId) -> InputBinding {
        lock(&daemon.state)
            .registry
            .get(id)
            .unwrap()
            .input_binding
            .clone()
            .expect("bound")
    }

    /// Durable events from `since` on: what a replaying subscriber gets.
    async fn events_since(daemon: &Arc<Daemon>, since: u64) -> Vec<EventKind> {
        let state = lock(&daemon.state);
        let mut events: Vec<Event> = state
            .store
            .recent_events(100)
            .unwrap()
            .into_iter()
            .filter(|e| e.seq >= since)
            .collect();
        events.sort_by_key(|e| e.seq);
        events.into_iter().map(|e| e.kind).collect()
    }

    /// The controller ends; the daemon notes it, pauses delivery evidence,
    /// starts the descriptor, and the started process resumes the binding
    /// with the same token while the episode keeps counting. A launched
    /// process that has not bound is stopped by an unbind.
    #[tokio::test]
    async fn a_controller_that_ended_is_started_again_from_its_descriptor() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "sess-1").await;
        let controller = Other::spawn();
        let launch = descriptor(&dir);
        let bound = daemon
            .handle(Request::BindInput {
                agent: receiver.id.to_string(),
                provider: generation(&receiver, "sess-1"),
                controller: controller.identity(),
                token: TOKEN.into(),
                launch: Some(launch.clone()),
            })
            .await;
        let Response::InputBound { binding, .. } = bound else {
            panic!("{bound:?}");
        };
        assert_eq!(binding.launch, Some(launch.clone()));
        // A different descriptor on a resume is refused; none keeps it.
        let other = ControllerLaunch {
            args: vec!["-c".into(), "true".into()],
            ..launch.clone()
        };
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: generation(&receiver, "sess-1"),
                    controller: controller.identity(),
                    token: TOKEN.into(),
                    launch: Some(other),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        // Alive: nothing happens.
        let seq = lock(&daemon.state).next_seq;
        daemon.tend_controllers();
        assert!(events_since(&daemon, seq).await.is_empty());
        // The controller dies.
        let ended = controller.identity();
        drop(controller);
        daemon.tend_controllers();
        let events = events_since(&daemon, seq).await;
        assert!(
            matches!(&events[..], [EventKind::InputControllerEnded { agent, controller }] if *agent == receiver.id && *controller == ended),
            "{events:?}"
        );
        let record = lock(&daemon.state)
            .registry
            .get(&receiver.id)
            .cloned()
            .unwrap();
        let delivery = record.input_delivery.expect("paused delivery");
        assert!(delivery.paused);
        assert_eq!(
            delivery.pause_reason.as_deref(),
            Some("the bound controller ended")
        );
        // The first launch is immediate.
        daemon.tend_controllers();
        let events = events_since(&daemon, seq).await;
        let launched = match &events[..] {
            [
                _,
                EventKind::InputControllerLaunched {
                    agent,
                    controller,
                    attempt: 1,
                },
            ] if *agent == receiver.id => controller.clone(),
            other => panic!("{other:?}"),
        };
        assert!(is_running(&launched));
        let binding = binding_of(&daemon, &receiver.id);
        assert_eq!(binding.restart.launched, Some(launched.clone()));
        assert_eq!(binding.restart.attempts, 1);
        for _ in 0..50 {
            if dir.path().join("mark").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("mark")).unwrap(),
            "launched\n"
        );
        // Starting: it holds its place, nothing more is launched.
        daemon.tend_controllers();
        assert_eq!(events_since(&daemon, seq).await.len(), 2);
        // The started process binds with the same token: a resume that
        // keeps the episode's count.
        let resumed = daemon
            .handle(Request::BindInput {
                agent: receiver.id.to_string(),
                provider: generation(&receiver, "sess-1"),
                controller: launched.clone(),
                token: TOKEN.into(),
                launch: Some(launch.clone()),
            })
            .await;
        let Response::InputBound {
            binding, resumed, ..
        } = resumed
        else {
            panic!("{resumed:?}");
        };
        assert!(resumed);
        assert_eq!(binding.controller, launched);
        assert_eq!(binding.controller_generations, 2);
        assert_eq!(binding.restart.attempts, 1);
        assert_eq!(binding.restart.launched, None);
        assert_eq!(binding.restart.ended_at, None);
        daemon.tend_controllers();
        assert_eq!(events_since(&daemon, seq).await.len(), 3);
        // It dies too, within the stable window: the second launch waits
        // out its backoff.
        signal(&launched, Signal::SIGKILL);
        for _ in 0..50 {
            if !is_running(&launched) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        daemon.tend_controllers();
        daemon.tend_controllers();
        let events = events_since(&daemon, seq).await;
        assert_eq!(events.len(), 4, "{events:?}");
        assert!(matches!(events[3], EventKind::InputControllerEnded { .. }));
        tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
        daemon.tend_controllers();
        let events = events_since(&daemon, seq).await;
        let second = match &events[..] {
            [
                ..,
                EventKind::InputControllerLaunched {
                    controller,
                    attempt: 2,
                    ..
                },
            ] => controller.clone(),
            other => panic!("{other:?}"),
        };
        assert!(is_running(&second));
        // An unbind stops the process the daemon launched, since it has
        // nothing left to bind to.
        assert!(matches!(
            daemon
                .handle(Request::UnbindInput {
                    agent: receiver.id.to_string(),
                    token: Some(TOKEN.into()),
                    force: false,
                })
                .await,
            Response::Ok
        ));
        for _ in 0..50 {
            if !is_running(&second) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(!is_running(&second));
        assert!(lock(&daemon.state).controller_pins.is_empty());
    }

    /// Bookkeeping that cannot be stored leaves memory as it was, so the
    /// store and the daemon never disagree about what is still uncertain
    /// or offered.
    #[tokio::test]
    async fn bookkeeping_that_cannot_be_stored_leaves_memory_as_it_was() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "sess-1").await;
        let sender = peer(&daemon, "sender").await;
        let first = send(&daemon, &sender, &receiver, "one").await;
        assert!(matches!(
            daemon
                .handle(Request::DeliveryQueue {
                    agent: receiver.id.to_string(),
                })
                .await,
            Response::Messages { .. }
        ));
        assert!(matches!(
            daemon.handle(bind(&receiver, "sess-1", me(), TOKEN)).await,
            Response::InputBound { binding, .. } if binding.uncertain == vec![first.clone()]
        ));
        {
            let mut state = lock(&daemon.state);
            state.storage_error = Some("disk gone".to_owned());
            state.forget_delivered(&receiver.id, std::slice::from_ref(&first));
            let record = state.registry.get(&receiver.id).unwrap();
            assert_eq!(
                record.input_binding.as_ref().unwrap().uncertain,
                vec![first.clone()],
                "memory did not move ahead of a store that took nothing"
            );
            assert!(record.legacy_offers.contains_key(&first));
            state.storage_error = None;
            state.forget_delivered(&receiver.id, std::slice::from_ref(&first));
            let record = state.registry.get(&receiver.id).unwrap();
            assert!(record.input_binding.as_ref().unwrap().uncertain.is_empty());
            assert!(record.legacy_offers.is_empty());
        }
        let stored = lock(&daemon.state)
            .store
            .load_agents()
            .unwrap()
            .into_iter()
            .find(|a| a.id == receiver.id)
            .unwrap();
        assert!(stored.input_binding.unwrap().uncertain.is_empty());
        assert!(stored.legacy_offers.is_empty());
    }

    /// The restart record is on the agent record: a daemon opened again
    /// on the same state continues where the last one stopped.
    #[tokio::test]
    async fn a_reopened_daemon_continues_the_restart_it_restored() {
        let dir = TempDir::new().unwrap();
        let receiver = {
            let daemon = open(&dir);
            let receiver = provider(&daemon, "receiver", "sess-1").await;
            let controller = Other::spawn();
            assert!(matches!(
                daemon
                    .handle(Request::BindInput {
                        agent: receiver.id.to_string(),
                        provider: generation(&receiver, "sess-1"),
                        controller: controller.identity(),
                        token: TOKEN.into(),
                        launch: Some(descriptor(&dir)),
                    })
                    .await,
                Response::InputBound { .. }
            ));
            drop(controller);
            daemon.tend_controllers();
            let binding = binding_of(&daemon, &receiver.id);
            assert!(binding.restart.ended_at.is_some());
            receiver
        };
        let daemon = open(&dir);
        let binding = binding_of(&daemon, &receiver.id);
        assert!(binding.launch.is_some());
        assert!(binding.restart.ended_at.is_some());
        let seq = lock(&daemon.state).next_seq;
        daemon.tend_controllers();
        let events = events_since(&daemon, seq).await;
        let launched = match &events[..] {
            [
                EventKind::InputControllerLaunched {
                    controller,
                    attempt: 1,
                    ..
                },
            ] => controller.clone(),
            other => panic!("{other:?}"),
        };
        assert!(is_running(&launched));
        signal(&launched, Signal::SIGKILL);
    }

    /// A descriptor that cannot start counts as an attempt and waits out
    /// the backoff like an ended process; invalid descriptors are refused.
    #[tokio::test]
    async fn a_launch_that_fails_counts_and_a_descriptor_is_checked() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "sess-1").await;
        let controller = Other::spawn();
        let relative = ControllerLaunch {
            executable: "sh".into(),
            ..descriptor(&dir)
        };
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: generation(&receiver, "sess-1"),
                    controller: controller.identity(),
                    token: TOKEN.into(),
                    launch: Some(relative),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let missing = ControllerLaunch {
            executable: dir.path().join("no-such-receiver"),
            ..descriptor(&dir)
        };
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: generation(&receiver, "sess-1"),
                    controller: controller.identity(),
                    token: TOKEN.into(),
                    launch: Some(missing),
                })
                .await,
            Response::InputBound { .. }
        ));
        let seq = lock(&daemon.state).next_seq;
        drop(controller);
        daemon.tend_controllers();
        daemon.tend_controllers();
        let events = events_since(&daemon, seq).await;
        assert!(
            matches!(
                &events[..],
                [
                    EventKind::InputControllerEnded { .. },
                    EventKind::InputControllerLaunchFailed { attempt: 1, error, .. }
                ] if error.contains("no-such-receiver")
            ),
            "{events:?}"
        );
        let binding = binding_of(&daemon, &receiver.id);
        assert_eq!(binding.restart.attempts, 1);
        assert_eq!(binding.restart.launched, None);
        assert!(binding.restart.ended_at.is_some());
        // Attempt two is not due yet.
        daemon.tend_controllers();
        assert_eq!(events_since(&daemon, seq).await.len(), 2);
    }

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
        // A look without draining may be the session's own reader, which
        // the daemon cannot tell from a person, and an exposure between the
        // controller's snapshot and its enqueue can never be reported to
        // it in time: so a bound queue refuses that read too. A person
        // looks with `peek_input`, which records no offer.
        assert!(matches!(
            daemon
                .handle(Request::Inbox {
                    agent: receiver.id.to_string(),
                    drain: false,
                })
                .await,
            Response::InputOwned { .. }
        ));
        assert!(matches!(
            daemon
                .handle(Request::PeekInput {
                    agent: receiver.id.to_string(),
                })
                .await,
            Response::Messages { ref messages } if messages.len() == 2
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
            vec![first.clone()],
            "a peek adds no uncertainty"
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
        assert_eq!(uncertain, vec![first.clone()]);

        // Between that snapshot and the controller's enqueue nothing can
        // expose the queue to the model through a legacy read: every such
        // read is refused, so the snapshot is exactly what the controller
        // reconciles against.
        let third = send(&daemon, &sender, &receiver, "three").await;
        for request in [
            Request::Inbox {
                agent: receiver.id.to_string(),
                drain: false,
            },
            Request::DeliveryQueue {
                agent: receiver.id.to_string(),
            },
        ] {
            assert!(matches!(
                daemon.handle(request).await,
                Response::InputOwned { .. }
            ));
        }
        assert_eq!(
            lock(&daemon.state)
                .registry
                .get(&receiver.id)
                .unwrap()
                .input_binding
                .as_ref()
                .unwrap()
                .uncertain,
            vec![first.clone()],
            "nothing new became uncertain after the snapshot"
        );

        // A legacy reader finishing its in-flight delivery may still
        // acknowledge what it was offered, and nothing else: the third
        // message nobody legacy has seen is the controller's alone.
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
                    launch: None,
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
                    launch: None,
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
                launch: None,
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
                    launch: None,
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
