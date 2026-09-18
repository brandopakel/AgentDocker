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
    let mut child = command
        .spawn()
        .map_err(|e| format!("{}: {e}", launch.executable.display()))?;
    let pid = child
        .id()
        .ok_or_else(|| "the process ended at once".to_owned())?;
    let Some(started_at) = agentdocker_host::procinfo::start_time(pid) else {
        // A process nobody can name later cannot be stopped later either.
        let _ = child.start_kill();
        return Err(format!("pid {pid}: start time unreadable; stopped"));
    };
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
            if state.fenced() {
                // A fenced coordinator writes nothing and starts nothing:
                // the successor tends the controllers once it serves.
                return;
            }
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
                ControllerStep::Upgrade { kill } => {
                    let state = lock(&self.state);
                    if state.fenced()
                        || state
                            .registry
                            .get(&id)
                            .and_then(|r| r.input_binding.as_ref())
                            != Some(&binding)
                    {
                        continue;
                    }
                    // Retained intent makes a crash after commit recoverable.
                    // Never let malformed restored state target the provider.
                    if binding.controller != binding.provider.process {
                        signal(
                            &binding.controller,
                            if kill {
                                Signal::SIGKILL
                            } else {
                                Signal::SIGTERM
                            },
                        );
                    }
                }
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
                    // Under the guard, and only if the binding is still the
                    // one decided on: a bind that landed since made this
                    // process the controller, and a fence since makes it
                    // the successor's to judge.
                    let state = lock(&self.state);
                    if state.fenced()
                        || state
                            .registry
                            .get(&id)
                            .and_then(|r| r.input_binding.as_ref())
                            != Some(&binding)
                    {
                        continue;
                    }
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
                    if state.fenced()
                        || state
                            .registry
                            .get(&id)
                            .and_then(|r| r.input_binding.as_ref())
                            != Some(&binding)
                    {
                        continue;
                    }
                    let spawned = state
                        .pin_controller(&id, &launch)
                        .map_err(|e| format!("release of {}: {e}", launch.executable.display()))
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
        // One guard from the look to the pins: a transfer that begins in
        // between would otherwise find pins taken past the fence.
        let mut state = lock(&self.state);
        if state.fenced() {
            // Held pins stay held; new ones are the successor's to take.
            return;
        }
        let wanted: Vec<(AgentId, ControllerLaunch)> = state
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
            .collect();
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
        if self.persist("legacy offers", |store| store.upsert_agent(&record))
            == Persisted::Committed
        {
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
        if self.persist("input bookkeeping", |store| store.upsert_agent(&record))
            == Persisted::Committed
        {
            *self.registry.get_mut(id).expect("resolved agent") = record;
        }
    }

    /// Hold the release a launch descriptor's executable belongs to, if it
    /// is inside a managed installation and not held already. An error
    /// means the release is being removed or is gone: nothing may be
    /// started from it.
    fn pin_controller(
        &mut self,
        id: &AgentId,
        launch: &ControllerLaunch,
    ) -> Result<(), std::io::Error> {
        if self.controller_pins.contains_key(id) {
            return Ok(());
        }
        match agentdocker_host::installation::pin_executable(&launch.executable) {
            Ok(Some(pin)) => {
                self.controller_pins.insert(id.clone(), pin);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// A pin that could not be taken, as an answer: a release being
    /// removed may come back into reach, a removed one will not, and
    /// anything else is the descriptor's own problem.
    fn pin_refused(launch: &ControllerLaunch, error: &std::io::Error) -> Response {
        let code = match error.kind() {
            std::io::ErrorKind::WouldBlock => ErrorCode::Unavailable,
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            _ => ErrorCode::Invalid,
        };
        Response::error(
            code,
            format!(
                "the launch descriptor's release ({}) cannot be held: {error}",
                launch.executable.display()
            ),
        )
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
        if self.persist("controller restart", |store| {
            store.agent_transition(&record, &event)
        }) != Persisted::Committed
        {
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
        // One thread of one profile has one queue: a second record bound
        // to it would be two receivers feeding one persisted conversation.
        // The explicit route for a dead predecessor is resume_input.
        if let Some(other) = self.registry.all().find(|r| {
            r.id != id
                && r.input_binding.as_ref().is_some_and(|b| {
                    b.provider.session == provider.session && b.provider.profile == provider.profile
                })
        }) {
            return Response::Error {
                code: ErrorCode::Conflict,
                message: "another record is bound to this thread and profile; resume it or unbind it first"
                    .to_owned(),
                details: Some(serde_json::json!({ "agent": other.id })),
            };
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
            return Self::pin_refused(launch, &error);
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
        let committed = self.persist("input binding", |store| {
            store.agent_transition(&record, &event)
        });
        if committed != Persisted::Committed {
            return self
                .write_failure()
                .expect("refused input binding write has a reason");
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

    pub(super) fn upgrade_controller(
        &mut self,
        reference: &str,
        expected: (ProviderGeneration, ProcessIdentity, ControllerLaunch),
        launch: ControllerLaunch,
        token: &str,
        now: DateTime<Utc>,
    ) -> Response {
        let (provider, controller, previous) = expected;
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(error) => return *error,
        };
        if self.fenced() {
            return self.transferring().expect("fenced coordinator");
        }
        if let Some(error) = self.write_failure() {
            return error;
        }
        let record = self.registry.get(&id).expect("resolved agent");
        let Some(existing) = &record.input_binding else {
            return Response::error(ErrorCode::Conflict, "input is no longer bound");
        };
        if !token_valid(token) || !existing.accepts_digest(&token_digest(token)) {
            return Response::error(ErrorCode::Forbidden, "the binding token does not match");
        }
        if existing.provider != provider
            || record.pid != Some(provider.process.pid)
            || record.process_started_at != Some(provider.process.started_at)
            || !record.status.is_live()
            || !is_running(&provider.process)
        {
            return Response::error(ErrorCode::Conflict, "the provider generation changed");
        }
        if !launch.valid()
            || !previous.valid()
            || launch.args != previous.args
            || launch.cwd != previous.cwd
            || launch.env != previous.env
        {
            return Response::error(
                ErrorCode::Invalid,
                "a receiver upgrade may change only its executable",
            );
        }
        if existing.launch.as_ref() == Some(&launch) {
            // The successful reply may have been lost. Never signal the successor
            // or change the restart episode when answering the same request again.
            return Response::InputBound {
                agent: id,
                binding: existing.clone(),
                resumed: false,
            };
        }
        if existing.controller != controller || existing.launch.as_ref() != Some(&previous) {
            return Response::error(
                ErrorCode::Conflict,
                "the controller or its launch descriptor changed",
            );
        }
        if controller == provider.process {
            return Response::error(
                ErrorCode::Invalid,
                "a receiver upgrade cannot stop its provider",
            );
        }
        if existing.restart.launched.as_ref().is_some_and(is_running) {
            return Response::error(
                ErrorCode::Conflict,
                "a controller restart is already in progress",
            );
        }
        if is_running(&controller) {
            let actual = agentdocker_host::procinfo::executable_path_of(controller.pid)
                .and_then(|path| path.canonicalize());
            if actual.ok().as_ref() != previous.executable.canonicalize().ok().as_ref()
                || !previous.executable.is_file()
            {
                return Response::error(
                    ErrorCode::Conflict,
                    "the live controller executable differs from its descriptor",
                );
            }
        }
        match launch.executable.canonicalize() {
            Ok(path) if path == launch.executable && path.is_file() => (),
            _ => {
                return Response::error(
                    ErrorCode::Invalid,
                    "the new receiver executable must be an existing canonical file",
                );
            }
        }
        // Hold both generations until the transition commits. A refused write
        // drops this temporary pin and leaves the old pin and process intact.
        let next_pin = match agentdocker_host::installation::pin_executable(&launch.executable) {
            Ok(pin) => pin,
            Err(error) => return Self::pin_refused(&launch, &error),
        };
        let mut record = record.clone();
        let binding = record.input_binding.as_mut().expect("bound input");
        binding.launch = Some(launch.clone());
        binding.restart = agentdocker_core::ControllerRestart {
            upgrade_requested_at: Some(now),
            ..Default::default()
        };
        let binding = binding.clone();
        pause_delivery(&mut record, "the input receiver is being upgraded", now);
        let mut event = Event::new(
            EventKind::InputControllerUpgraded {
                agent: id.clone(),
                controller: controller.clone(),
                previous_executable: previous.executable,
                executable: launch.executable,
            },
            now,
        );
        event.seq = self.next_seq;
        if self.persist("controller upgrade", |store| {
            store.agent_transition(&record, &event)
        }) != Persisted::Committed
        {
            return self.write_failure().expect("refused upgrade has a reason");
        }
        *self.registry.get_mut(&id).expect("resolved agent") = record;
        self.next_seq += 1;
        if let Some(pin) = next_pin {
            self.controller_pins.insert(id.clone(), pin);
        } else {
            self.controller_pins.remove(&id);
        }
        let _ = self.events.send(event);
        // The durable descriptor now names the successor. Normal supervision
        // waits for the old process to end and for its ledger lock to be free.
        // The next supervision tick executes the persisted stop intent. Keeping
        // execution out of this request also exercises crash-before-signal recovery.
        Response::InputBound {
            agent: id,
            binding,
            resumed: false,
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
        let committed = self.persist("input unbinding", |store| {
            store.agent_transition(&record, &event)
        });
        if committed != Persisted::Committed {
            return self
                .write_failure()
                .expect("refused input unbinding write has a reason");
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

    /// A provider session came back as a new process and, registering by
    /// pid and birth, as a new record; its thread's queue, binding and the
    /// controller's ledger are on the record of the process that ended.
    /// Join them: the prior record stays canonical and takes the new
    /// process, its binding takes the new generation and descriptor, the
    /// caller's queued messages move across and everything queued becomes
    /// uncertain, and the caller's id is retired into an alias. Only the
    /// one prior record of exactly this thread, profile and checkout, and
    /// only once every process of that generation is gone: two matches,
    /// a live one, or a caller that already holds leases or channel
    /// membership is refused rather than guessed at.
    pub(super) fn resume_input(
        &mut self,
        reference: &str,
        predecessor: &str,
        provider: ProviderGeneration,
        launch: ControllerLaunch,
        now: DateTime<Utc>,
    ) -> Response {
        if !launch.valid() || !provider.valid() || !Path::new(&provider.profile).is_absolute() {
            return Response::error(
                ErrorCode::Invalid,
                "resuming needs a valid provider generation with an absolute profile path and a valid launch descriptor",
            );
        }
        // A lost reply, asked again: the caller's id is an alias by now.
        // The same request gets the same answer; a different one is not
        // a repeat, and there is no record left to resume from.
        let retired = AgentId::from(reference);
        if let Some(canonical) = self.registry.aliases().get(&retired).cloned() {
            let same_predecessor = self.resolve(predecessor).is_ok_and(|id| id == canonical);
            let binding = self
                .registry
                .get(&canonical)
                .and_then(|r| r.input_binding.clone());
            return match binding {
                Some(binding)
                    if same_predecessor
                        && binding.provider == provider
                        && binding.launch.as_ref() == Some(&launch) =>
                {
                    Response::InputResumed {
                        agent: canonical,
                        retired,
                        binding,
                    }
                }
                _ => Response::error(
                    ErrorCode::Conflict,
                    "this id was already resumed into another record with a different predecessor, generation or descriptor",
                ),
            };
        }
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let caller = self.registry.get(&id).expect("resolved agent").clone();
        if !caller.status.is_live() || caller.managed || caller.input_binding.is_some() {
            return Response::error(
                ErrorCode::Invalid,
                "the caller must be a live, externally registered record without a binding of its own",
            );
        }
        if caller.pid != Some(provider.process.pid)
            || caller.process_started_at != Some(provider.process.started_at)
        {
            return Response::error(
                ErrorCode::Invalid,
                "the provider process must be the caller's registered pid and birth",
            );
        }
        if caller.spec.labels.get("session_id") != Some(&provider.session) {
            return Response::error(
                ErrorCode::Invalid,
                "the provider session must be the caller's registered session_id",
            );
        }
        if !is_running(&provider.process) {
            return Response::error(ErrorCode::Invalid, "the caller's process is not running");
        }
        let Some(workdir) = caller.spec.workdir.as_deref().map(project::canonical) else {
            return Response::error(ErrorCode::Invalid, "the caller has no working directory");
        };
        let prior_id = match self.resolve(predecessor) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let prior = self
            .registry
            .get(&prior_id)
            .expect("resolved agent")
            .clone();
        let Some(binding) = prior.input_binding.clone() else {
            return Response::error(ErrorCode::Invalid, "the predecessor has no input binding");
        };
        if prior.id == caller.id
            || prior.spec.runtime != caller.spec.runtime
            || binding.provider.session != provider.session
            || binding.provider.profile != provider.profile
            || prior.spec.workdir.as_deref().map(project::canonical) != Some(workdir)
        {
            return Response::error(
                ErrorCode::Invalid,
                "the predecessor must be another record of the same runtime bound to the same thread, profile and checkout",
            );
        }
        // The same checkout can be another project by now (a repository
        // re-made under the same path has another fingerprint); the alias
        // would then file the prior record's conversations under the
        // caller's project. Refuse rather than move history across.
        if prior.project.as_ref().map(agentdocker_core::ProjectRef::id)
            != caller
                .project
                .as_ref()
                .map(agentdocker_core::ProjectRef::id)
        {
            return Response::error(
                ErrorCode::Conflict,
                "the predecessor's checkout is another project now; resolve by hand",
            );
        }
        if let Some(other) = self.registry.all().find(|r| {
            r.id != prior.id
                && r.id != caller.id
                && r.input_binding.as_ref().is_some_and(|b| {
                    b.provider.session == provider.session && b.provider.profile == provider.profile
                })
        }) {
            return Response::Error {
                code: ErrorCode::Conflict,
                message: "another record is bound to this thread as well; resolve by hand"
                    .to_owned(),
                details: Some(serde_json::json!({ "agent": other.id })),
            };
        }
        if is_running(&binding.provider.process)
            || is_running(&binding.controller)
            || binding.restart.launched.as_ref().is_some_and(is_running)
        {
            return Response::Error {
                code: ErrorCode::Conflict,
                message: "a process of the prior generation is still running".to_owned(),
                details: Some(serde_json::json!({ "agent": prior.id })),
            };
        }
        if !self.leases.by_holder(&caller.id).is_empty()
            || self
                .channels
                .values()
                .any(|c| c.is_open() && c.has(&caller.id))
        {
            return Response::error(
                ErrorCode::Conflict,
                "the caller already holds leases or channel membership; resolve by hand",
            );
        }
        // Fail closed: a store that cannot say whether the caller observed
        // anything must not authorise retiring it.
        match self
            .store
            .document::<Vec<agentdocker_core::ReadMark>>("reads", caller.id.as_str())
        {
            Ok(Some(reads)) if !reads.is_empty() => {
                return Response::error(
                    ErrorCode::Conflict,
                    "the caller has recorded observations of its own; resolve by hand",
                );
            }
            Ok(_) => {}
            Err(error) => {
                return Response::error(
                    ErrorCode::StorageUnavailable,
                    format!("the caller's observations could not be read: {error}"),
                );
            }
        }
        let (pid, started_at) = (provider.process.pid, provider.process.started_at);
        // The prior record with the caller's process, and its binding on
        // the new generation. What was uncertain stays uncertain and the
        // caller's own legacy offers join it; nothing else is flagged: the
        // controller's ledger proves what it never prepared, and a row
        // flagged for no reason would hang ordinary input.
        let mut canonical = prior.clone();
        canonical.pid = Some(pid);
        canonical.process_started_at = Some(started_at);
        canonical.process_group = caller.process_group;
        canonical.status = caller.status;
        canonical.last_seen = now;
        canonical.vcs = caller.vcs.clone();
        for (key, value) in &caller.spec.labels {
            canonical
                .spec
                .labels
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        for (message, at) in &caller.legacy_offers {
            canonical
                .legacy_offers
                .entry(message.clone())
                .or_insert(*at);
        }
        let mut merged: Vec<Envelope> = self
            .inboxes
            .get(&prior.id)
            .into_iter()
            .chain(self.inboxes.get(&caller.id))
            .flatten()
            .cloned()
            .collect();
        merged.sort_by_key(|m| m.sent_at);
        let mut binding = binding;
        binding.provider = provider;
        binding.launch = Some(launch.clone());
        binding.restart = agentdocker_core::ControllerRestart {
            ended_at: Some(now),
            ..Default::default()
        };
        let queued: HashSet<&MessageId> = merged.iter().map(|m| &m.id).collect();
        for message in caller.legacy_offers.keys() {
            if queued.contains(message) && !binding.uncertain.contains(message) {
                binding.uncertain.push(message.clone());
            }
        }
        binding.uncertain.retain(|m| queued.contains(m));
        canonical.input_binding = Some(binding.clone());
        // The new descriptor may run from another release: hold that one
        // before letting go of the old, and put the old back if the
        // resume does not commit.
        let previous_pin = self.controller_pins.remove(&prior.id);
        if let Err(error) = self.pin_controller(&prior.id, &launch) {
            if let Some(pin) = previous_pin {
                self.controller_pins.insert(prior.id.clone(), pin);
            }
            return Self::pin_refused(&launch, &error);
        }
        let alias = agentdocker_core::identity::AgentAlias {
            retired: caller.id.clone(),
            canonical: prior.id.clone(),
            retired_name: Some(caller.spec.name.clone()),
            reconciled_at: now,
        };
        let mut event = Event::new(
            EventKind::InputResumed {
                agent: prior.id.clone(),
                retired: caller.id.clone(),
                provider: binding.provider.clone(),
            },
            now,
        );
        event.seq = self.next_seq;
        let committed = self.persist("input resume", |store| {
            store.resume_input(&canonical, &alias, &event)
        });
        if committed != Persisted::Committed {
            self.controller_pins.remove(&prior.id);
            if let Some(pin) = previous_pin {
                self.controller_pins.insert(prior.id.clone(), pin);
            }
            return self
                .write_failure()
                .expect("refused input resume write has a reason");
        }
        if let Err(error) = self.registry.retire_into(&caller.id, &prior.id) {
            // Checked above; the store has the alias, memory must follow.
            error!(%error, "retiring a resumed record");
        }
        *self.registry.get_mut(&prior.id).expect("prior record") = canonical;
        let moved: usize = merged.iter().map(message_bytes).sum();
        self.inboxes.remove(&caller.id);
        self.inbox_bytes.remove(&caller.id);
        self.inboxes
            .insert(prior.id.clone(), merged.into_iter().collect());
        self.inbox_bytes.insert(prior.id.clone(), moved);
        self.next_seq += 1;
        let _ = self.events.send(event);
        Response::InputResumed {
            agent: prior.id,
            retired: caller.id,
            binding,
        }
    }

    /// A person's retry: the restart episode starts over on the binding as
    /// it is, so the next tick launches the descriptor again. Nothing else
    /// moves: the queue stays, the provider generation stays, and a
    /// controller that is running, bound or just launched, keeps its place.
    pub(super) fn retry_controller(&mut self, reference: &str, now: DateTime<Utc>) -> Response {
        let id = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let record = self.registry.get(&id).expect("resolved agent");
        let Some(binding) = &record.input_binding else {
            return Response::error(ErrorCode::NotFound, "this agent's input is not bound");
        };
        if binding.launch.is_none() {
            return Response::error(
                ErrorCode::Invalid,
                "this binding has no launch descriptor; its controller is started by the runtime's own hook",
            );
        }
        if is_running(&binding.controller) {
            return Response::error(ErrorCode::Conflict, "the bound controller is running");
        }
        if binding.restart.launched.as_ref().is_some_and(is_running) {
            return Response::error(
                ErrorCode::Conflict,
                "a controller the daemon launched is starting",
            );
        }
        let expected = binding.clone();
        let applied = self.transition_binding(&id, &expected, now, |record| {
            let binding = record.input_binding.as_mut().expect("checked");
            binding.restart.attempts = 0;
            binding.restart.exhausted = false;
            binding.restart.launched = None;
            binding.restart.launched_at = None;
            binding.restart.ended_at = Some(now);
            EventKind::InputRestartsReset { agent: id.clone() }
        });
        if applied {
            Response::Ok
        } else {
            self.storage_failure()
                .unwrap_or_else(|| Response::error(ErrorCode::Conflict, "the binding changed"))
        }
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
                    answers_routed: true,
                }
            }
            Response::InputWaiting { .. } => Response::InputBatch {
                agent: id,
                messages: Vec::new(),
                uncertain: Vec::new(),
                answers_routed: true,
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
                links: Vec::new(),
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

    /// A provider that came back as a new process registered as a new
    /// record; resuming joins it to the prior record of its thread: one
    /// identity, one queue, the binding on the new generation, everything
    /// queued uncertain, and the daemon starts the new descriptor.
    #[tokio::test]
    async fn a_resumed_provider_joins_the_record_that_holds_its_queue() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let old_provider = Other::spawn();
        let register = async |name: &str, pid: u32| {
            let mut spec = AgentSpec {
                name: name.into(),
                runtime: "custom".into(),
                workdir: Some(dir.path().to_path_buf()),
                ..Default::default()
            };
            spec.labels.insert("session_id".into(), "thread-9".into());
            match daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(pid),
                    session: None,
                })
                .await
            {
                Response::Agent { agent } => agent,
                other => panic!("{other:?}"),
            }
        };
        let prior = register("codex-old", old_provider.0.id()).await;
        let sender = peer(&daemon, "sender").await;
        let controller = Other::spawn();
        let profile = dir.path().join("profile").display().to_string();
        let old_launch = ControllerLaunch {
            args: vec!["-c".into(), "exit 1".into()],
            ..descriptor(&dir)
        };
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: prior.id.to_string(),
                    provider: ProviderGeneration {
                        process: old_provider.identity(),
                        session: "thread-9".into(),
                        profile: profile.clone(),
                    },
                    controller: controller.identity(),
                    token: TOKEN.into(),
                    launch: Some(old_launch),
                })
                .await,
            Response::InputBound { .. }
        ));
        let before = send(&daemon, &sender, &prior, "before the restart").await;
        // The provider and its controller end; the thread comes back as a
        // new process, which registers as a new record and gets a message.
        drop(old_provider);
        drop(controller);
        let fresh = register("codex-new", std::process::id()).await;
        assert_ne!(fresh.id, prior.id);
        let after = send(&daemon, &sender, &fresh, "after the restart").await;
        let generation = |profile: String| ProviderGeneration {
            process: me(),
            session: "thread-9".into(),
            profile,
        };
        // The wrong profile is not the predecessor's thread.
        assert!(matches!(
            daemon
                .handle(Request::ResumeInput {
                    agent: fresh.id.to_string(),
                    predecessor: prior.id.to_string(),
                    provider: generation(dir.path().join("other").display().to_string()),
                    launch: descriptor(&dir),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        let seq = lock(&daemon.state).next_seq;
        let Response::InputResumed {
            agent,
            retired,
            binding,
        } = daemon
            .handle(Request::ResumeInput {
                agent: fresh.id.to_string(),
                predecessor: prior.id.to_string(),
                provider: generation(profile.clone()),
                launch: descriptor(&dir),
            })
            .await
        else {
            panic!("resume failed");
        };
        assert_eq!(agent, prior.id);
        assert_eq!(retired, fresh.id);
        assert_eq!(binding.provider.process, me());
        assert_eq!(binding.provider.session, "thread-9");
        assert_eq!(binding.launch, Some(descriptor(&dir)));
        assert_eq!(binding.restart.attempts, 0);
        assert!(
            binding.uncertain.is_empty(),
            "no legacy reader offered anything, so nothing is uncertain: the controller's ledger settles the rest"
        );
        assert!(
            binding.accepts_digest(&token_digest(TOKEN)),
            "the token stays"
        );
        {
            let state = lock(&daemon.state);
            let record = state
                .registry
                .get(&fresh.id)
                .expect("resolves through the alias");
            assert_eq!(record.id, prior.id);
            assert_eq!(record.pid, Some(std::process::id()));
            assert!(record.status.is_live());
            assert_eq!(
                state.inboxes[&prior.id]
                    .iter()
                    .map(|m| m.id.clone())
                    .collect::<Vec<_>>(),
                vec![before.clone(), after.clone()],
                "one queue, in order"
            );
            assert!(!state.inboxes.contains_key(&fresh.id));
        }
        assert!(matches!(
            &events_since(&daemon, seq).await[..],
            [EventKind::InputResumed { agent, retired, .. }] if *agent == prior.id && *retired == fresh.id
        ));
        // The tick starts the new descriptor at once.
        daemon.tend_controllers();
        let launched = match &events_since(&daemon, seq).await[..] {
            [
                _,
                EventKind::InputControllerLaunched {
                    controller,
                    attempt: 1,
                    ..
                },
            ] => controller.clone(),
            other => panic!("{other:?}"),
        };
        assert!(is_running(&launched));
        // It binds with the same token, as a resume of the dead controller.
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: fresh.id.to_string(),
                    provider: binding.provider.clone(),
                    controller: launched.clone(),
                    token: TOKEN.into(),
                    launch: Some(descriptor(&dir)),
                })
                .await,
            Response::InputBound { resumed: true, agent, .. } if agent == prior.id
        ));
        signal(&launched, Signal::SIGKILL);
        // A lost reply asked again is answered the same way, with nothing
        // new recorded; a different request through the alias is refused.
        let seq = lock(&daemon.state).next_seq;
        assert!(matches!(
            daemon
                .handle(Request::ResumeInput {
                    agent: fresh.id.to_string(),
                    predecessor: prior.id.to_string(),
                    provider: generation(profile.clone()),
                    launch: descriptor(&dir),
                })
                .await,
            Response::InputResumed { agent, retired, .. } if agent == prior.id && retired == fresh.id
        ));
        assert_eq!(lock(&daemon.state).next_seq, seq);
        assert!(matches!(
            daemon
                .handle(Request::ResumeInput {
                    agent: fresh.id.to_string(),
                    predecessor: prior.id.to_string(),
                    provider: generation(profile.clone()),
                    launch: ControllerLaunch {
                        args: vec!["-c".into(), "true".into()],
                        ..descriptor(&dir)
                    },
                })
                .await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::ResumeInput {
                    agent: fresh.id.to_string(),
                    predecessor: sender.id.to_string(),
                    provider: generation(profile.clone()),
                    launch: descriptor(&dir),
                })
                .await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        // Durable: a reopened daemon knows the alias and the joined queue.
        drop(daemon);
        let daemon = open(&dir);
        let state = lock(&daemon.state);
        assert_eq!(
            state.registry.get(&fresh.id).map(|r| r.id.clone()),
            Some(prior.id.clone())
        );
        assert_eq!(state.inboxes[&prior.id].len(), 2);
        assert!(state.registry.all().all(|r| r.id != fresh.id));
    }

    /// Resuming refuses to guess: a generation still running, two prior
    /// records, or a caller that already has state of its own.
    #[tokio::test]
    async fn resuming_refuses_a_live_generation_and_an_entangled_caller() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let provider_process = Other::spawn();
        let register = async |name: &str, pid: u32, session: &str| {
            let mut spec = AgentSpec {
                name: name.into(),
                runtime: "custom".into(),
                workdir: Some(dir.path().to_path_buf()),
                ..Default::default()
            };
            spec.labels.insert("session_id".into(), session.into());
            match daemon
                .handle(Request::Register {
                    spec,
                    pid: Some(pid),
                    session: None,
                })
                .await
            {
                Response::Agent { agent } => agent,
                other => panic!("{other:?}"),
            }
        };
        let prior = register("codex-old", provider_process.0.id(), "thread-9").await;
        let profile = dir.path().join("profile").display().to_string();
        let controller = Other::spawn();
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: prior.id.to_string(),
                    provider: ProviderGeneration {
                        process: provider_process.identity(),
                        session: "thread-9".into(),
                        profile: profile.clone(),
                    },
                    controller: controller.identity(),
                    token: TOKEN.into(),
                    launch: None,
                })
                .await,
            Response::InputBound { .. }
        ));
        let fresh = register("codex-new", std::process::id(), "thread-9").await;
        let resume = || {
            daemon.handle(Request::ResumeInput {
                agent: fresh.id.to_string(),
                predecessor: prior.id.to_string(),
                provider: ProviderGeneration {
                    process: me(),
                    session: "thread-9".into(),
                    profile: profile.clone(),
                },
                launch: descriptor(&dir),
            })
        };
        // The old provider still runs: not a restart.
        assert!(matches!(
            resume().await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        drop(provider_process);
        // The old controller still runs: not yet.
        assert!(matches!(
            resume().await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        drop(controller);
        // The caller holds a lease: it has a life of its own already.
        let Response::Lease { lease } = daemon
            .handle(Request::Claim {
                agent: fresh.id.to_string(),
                resource: "task:something".into(),
                mode: agentdocker_core::LeaseMode::Exclusive,
                amount: None,
                ttl_secs: 60,
                note: None,
                wait_secs: 0,
                automatic: false,
            })
            .await
        else {
            panic!("claim failed");
        };
        assert!(matches!(
            resume().await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        let released = daemon
            .handle(Request::Release {
                agent: fresh.id.to_string(),
                lease: lease.id,
                summary: None,
                summary_source: Default::default(),
            })
            .await;
        assert!(!matches!(released, Response::Error { .. }), "{released:?}");
        assert!(matches!(resume().await, Response::InputResumed { .. }));
    }

    /// After the daemon gave up, a person's retry starts the episode over
    /// on the same binding; while a controller runs there is nothing to
    /// retry.
    #[tokio::test]
    async fn a_person_can_retry_an_exhausted_controller_without_unbinding() {
        let dir = TempDir::new().unwrap();
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
        let retry = || {
            daemon.handle(Request::RetryController {
                agent: receiver.id.to_string(),
            })
        };
        assert!(matches!(
            retry().await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        drop(controller);
        daemon.tend_controllers();
        // The daemon gave up.
        {
            let mut state = lock(&daemon.state);
            let mut record = state.registry.get(&receiver.id).cloned().unwrap();
            let binding = record.input_binding.as_mut().unwrap();
            binding.restart.attempts = agentdocker_core::CONTROLLER_RESTARTS;
            binding.restart.exhausted = true;
            assert_eq!(
                state.persist("test", |store| store.upsert_agent(&record)),
                Persisted::Committed
            );
            *state.registry.get_mut(&receiver.id).unwrap() = record;
        }
        let seq = lock(&daemon.state).next_seq;
        daemon.tend_controllers();
        assert!(events_since(&daemon, seq).await.is_empty(), "given up");
        assert!(matches!(retry().await, Response::Ok));
        let binding = binding_of(&daemon, &receiver.id);
        assert_eq!(binding.restart.attempts, 0);
        assert!(!binding.restart.exhausted);
        assert!(
            binding.controller_generations == 1,
            "the binding itself is untouched"
        );
        daemon.tend_controllers();
        let events = events_since(&daemon, seq).await;
        let launched = match &events[..] {
            [
                EventKind::InputRestartsReset { .. },
                EventKind::InputControllerLaunched {
                    controller,
                    attempt: 1,
                    ..
                },
            ] => controller.clone(),
            other => panic!("{other:?}"),
        };
        // Starting: nothing to retry either.
        assert!(matches!(
            retry().await,
            Response::Error {
                code: ErrorCode::Conflict,
                ..
            }
        ));
        signal(&launched, Signal::SIGKILL);
        // An unmanaged binding has nothing the daemon could start.
        let plain = provider(&daemon, "plain", "sess-2").await;
        let other = Other::spawn();
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: plain.id.to_string(),
                    provider: generation(&plain, "sess-2"),
                    controller: other.identity(),
                    token: OTHER_TOKEN.into(),
                    launch: None,
                })
                .await,
            Response::InputBound { .. }
        ));
        drop(other);
        assert!(matches!(
            daemon
                .handle(Request::RetryController {
                    agent: plain.id.to_string(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
    }

    /// A fenced coordinator neither notes an end nor launches anything:
    /// the tick leaves memory and disk as they are, and the successor,
    /// or this daemon once it takes authority back, does the work.
    #[tokio::test]
    async fn a_fenced_tick_neither_notes_an_end_nor_launches() {
        let dir = TempDir::new().unwrap();
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
        daemon.offer_transfer(1).unwrap();
        let seq = lock(&daemon.state).next_seq;
        daemon.tend_controllers();
        daemon.tend_controllers();
        let binding = binding_of(&daemon, &receiver.id);
        assert_eq!(binding.restart.ended_at, None, "nothing noted while fenced");
        assert_eq!(
            binding.restart.launched, None,
            "nothing launched while fenced"
        );
        assert_eq!(lock(&daemon.state).next_seq, seq, "no event while fenced");
        assert!(
            !dir.path().join("mark").exists(),
            "the descriptor did not run"
        );
        // A bind while fenced is refused as transferring, memory untouched.
        let other = Other::spawn();
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: generation(&receiver, "sess-1"),
                    controller: other.identity(),
                    token: TOKEN.into(),
                    launch: Some(descriptor(&dir)),
                })
                .await,
            Response::Error {
                code: ErrorCode::Transferring,
                ..
            }
        ));
        drop(other);
        assert!(daemon.abort_transfer("cleanup"));
        daemon.tend_controllers();
        daemon.tend_controllers();
        let launched = binding_of(&daemon, &receiver.id)
            .restart
            .launched
            .expect("launched once authority is back");
        assert!(is_running(&launched));
        // Its bind grace runs out while a new offer is up: a fenced tick
        // signals nothing either, since the successor is the one to judge;
        // once authority is back the tick tells it to stop.
        {
            let mut state = lock(&daemon.state);
            let record = state.registry.get_mut(&receiver.id).unwrap();
            let binding = record.input_binding.as_mut().unwrap();
            binding.restart.launched_at = Some(
                Utc::now() - agentdocker_core::CONTROLLER_BIND_GRACE - chrono::Duration::seconds(1),
            );
        }
        daemon.offer_transfer(1).unwrap();
        daemon.tend_controllers();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(is_running(&launched), "a fenced tick signals nobody");
        assert!(daemon.abort_transfer("cleanup"));
        daemon.tend_controllers();
        // Yield to the runtime while waiting: it reaps the child, which
        // on Linux stays a visible zombie until then.
        let gone = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while is_running(&launched) && std::time::Instant::now() < gone {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            !is_running(&launched),
            "told to stop once authority is back"
        );
    }

    /// A delivery read records the offer it makes, so it is a mutation: a
    /// fenced daemon refuses it rather than hand a hook a message whose
    /// exposure nobody would record, which a controller binding later
    /// would take for never offered. A person's look stays a read.
    #[tokio::test]
    async fn a_fenced_delivery_read_is_refused_rather_than_unrecorded() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "sess-1").await;
        let sender = peer(&daemon, "sender").await;
        let queued = send(&daemon, &sender, &receiver, "one").await;
        daemon.offer_transfer(1).unwrap();
        assert!(matches!(
            daemon
                .handle(Request::DeliveryQueue {
                    agent: receiver.id.to_string(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Transferring,
                ..
            }
        ));
        assert!(matches!(
            daemon
                .handle(Request::PeekInput {
                    agent: receiver.id.to_string(),
                })
                .await,
            Response::Messages { messages } if messages.iter().map(|m| &m.id).eq([&queued])
        ));
        assert!(
            lock(&daemon.state)
                .registry
                .get(&receiver.id)
                .unwrap()
                .legacy_offers
                .is_empty(),
            "nothing was offered while fenced"
        );
        assert!(daemon.abort_transfer("cleanup"));
        assert!(matches!(
            daemon
                .handle(Request::DeliveryQueue {
                    agent: receiver.id.to_string(),
                })
                .await,
            Response::Messages { messages } if messages.len() == 1
        ));
        assert!(
            lock(&daemon.state)
                .registry
                .get(&receiver.id)
                .unwrap()
                .legacy_offers
                .contains_key(&queued),
            "the offer is recorded once the write can land"
        );
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
    #[tokio::test]
    async fn receiver_upgrade_commits_before_stop_and_retains_queue_across_reopen() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "upgrade-session").await;
        let sender = peer(&daemon, "sender").await;
        let message = send(&daemon, &sender, &receiver, "retained through upgrade").await;
        let mut old = Other::spawn();
        let old_process = old.identity();
        let previous = ControllerLaunch {
            executable: agentdocker_host::procinfo::executable_path_of(old_process.pid)
                .unwrap()
                .canonicalize()
                .unwrap(),
            args: vec!["30".into()],
            cwd: dir.path().canonicalize().unwrap(),
            env: Default::default(),
        };
        let new_executable = dir.path().join("successor");
        std::fs::copy(&previous.executable, &new_executable).unwrap();
        let launch = ControllerLaunch {
            executable: new_executable.canonicalize().unwrap(),
            ..previous.clone()
        };
        let provider = generation(&receiver, "upgrade-session");
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: provider.clone(),
                    controller: old_process.clone(),
                    token: TOKEN.into(),
                    launch: Some(previous.clone()),
                })
                .await,
            Response::InputBound { .. }
        ));
        let before = binding_of(&daemon, &receiver.id);
        let request = Request::UpgradeController {
            agent: receiver.id.to_string(),
            provider: provider.clone(),
            controller: old_process.clone(),
            previous: previous.clone(),
            launch: launch.clone(),
            token: TOKEN.into(),
        };
        let seq = lock(&daemon.state).next_seq;
        for invalid in [
            "token",
            "controller",
            "provider",
            "arguments",
            "missing",
            "wrong_executable",
        ] {
            let mut bad = request.clone();
            let Request::UpgradeController {
                token,
                controller,
                provider,
                launch,
                previous,
                ..
            } = &mut bad
            else {
                unreachable!()
            };
            match invalid {
                "token" => *token = OTHER_TOKEN.into(),
                "controller" => *controller = me(),
                "provider" => provider.session = "another-thread".into(),
                "arguments" => launch.args.push("unexpected".into()),
                "missing" => launch.executable = dir.path().join("missing"),
                "wrong_executable" => previous.executable = launch.executable.clone(),
                _ => unreachable!(),
            }
            assert!(
                matches!(daemon.handle(bad).await, Response::Error { .. }),
                "{invalid}"
            );
            assert_eq!(binding_of(&daemon, &receiver.id), before);
            assert_eq!(lock(&daemon.state).next_seq, seq);
            assert!(
                is_running(&old_process),
                "refused upgrade stopped the old controller"
            );
        }
        daemon.offer_transfer(1).unwrap();
        assert!(matches!(
            daemon.handle(request.clone()).await,
            Response::Error {
                code: ErrorCode::Transferring,
                ..
            }
        ));
        assert_eq!(binding_of(&daemon, &receiver.id), before);
        assert!(is_running(&old_process));
        assert!(daemon.abort_transfer("upgrade test"));
        // Offer and abort intentionally emitted their two coordinator events.
        // The rejected upgrade itself must add none after that boundary.
        let seq = lock(&daemon.state).next_seq;
        lock(&daemon.state)
            .store
            .reject_event_for_test("input_controller_upgraded");
        assert!(matches!(
            daemon.handle(request.clone()).await,
            Response::Error {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ));
        assert!(
            is_running(&old_process),
            "a failed write must not signal the controller"
        );
        assert_eq!(binding_of(&daemon, &receiver.id), before);
        assert_eq!(lock(&daemon.state).next_seq, seq);
        drop(daemon);
        let daemon = open(&dir);
        assert_eq!(binding_of(&daemon, &receiver.id), before);
        assert!(matches!(
            daemon.handle(request.clone()).await,
            Response::InputBound { .. }
        ));
        let committed = binding_of(&daemon, &receiver.id);
        assert_eq!(committed.launch, Some(launch.clone()));
        assert_eq!(committed.provider, before.provider);
        assert_eq!(committed.token_sha256, before.token_sha256);
        assert_eq!(committed.bound_at, before.bound_at);
        assert_eq!(committed.uncertain, before.uncertain);
        assert_eq!(committed.controller, old_process);
        assert!(
            is_running(&old_process),
            "stop is driven by the retained intent"
        );
        drop(daemon);
        let daemon = open(&dir);
        assert_eq!(binding_of(&daemon, &receiver.id), committed);
        daemon.offer_transfer(2).unwrap();
        daemon.tend_controllers();
        assert!(
            is_running(&old_process),
            "a fenced tick cannot execute the retained stop"
        );
        assert!(daemon.abort_transfer("resume retained upgrade"));
        daemon.tend_controllers();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while old.0.try_wait().unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "old receiver did not stop"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(daemon);
        let daemon = open(&dir);
        assert_eq!(binding_of(&daemon, &receiver.id), committed);
        let current = Other::spawn();
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider,
                    controller: current.identity(),
                    token: TOKEN.into(),
                    launch: Some(launch),
                })
                .await,
            Response::InputBound { resumed: true, .. }
        ));
        let current_binding = binding_of(&daemon, &receiver.id);
        let seq = lock(&daemon.state).next_seq;
        assert!(matches!(
            daemon.handle(request).await,
            Response::InputBound { .. }
        ));
        assert!(
            is_running(&current.identity()),
            "a lost upgrade reply must not stop the successor"
        );
        assert_eq!(binding_of(&daemon, &receiver.id), current_binding);
        assert_eq!(lock(&daemon.state).next_seq, seq);
        assert!(
            matches!(daemon.handle(Request::PeekInput { agent: receiver.id.to_string() }).await,
            Response::Messages { messages } if messages.iter().map(|m| &m.id).collect::<Vec<_>>() == vec![&message])
        );
        assert!(is_running(&me()), "provider retained");
    }
    #[tokio::test]
    async fn receiver_upgrade_never_targets_the_provider_process() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let receiver = provider(&daemon, "receiver", "same-process").await;
        let previous = ControllerLaunch {
            executable: agentdocker_host::procinfo::executable_path()
                .unwrap()
                .canonicalize()
                .unwrap(),
            args: vec![],
            cwd: dir.path().canonicalize().unwrap(),
            env: Default::default(),
        };
        let provider = generation(&receiver, "same-process");
        assert!(matches!(
            daemon
                .handle(Request::BindInput {
                    agent: receiver.id.to_string(),
                    provider: provider.clone(),
                    controller: me(),
                    token: TOKEN.into(),
                    launch: Some(previous.clone()),
                })
                .await,
            Response::InputBound { .. }
        ));
        let mut launch = previous.clone();
        launch.executable = dir.path().join("unused-new-receiver");
        let seq = lock(&daemon.state).next_seq;
        let binding = binding_of(&daemon, &receiver.id);
        assert!(matches!(
            daemon
                .handle(Request::UpgradeController {
                    agent: receiver.id.to_string(),
                    provider,
                    controller: me(),
                    previous,
                    launch,
                    token: TOKEN.into(),
                })
                .await,
            Response::Error {
                code: ErrorCode::Invalid,
                ..
            }
        ));
        assert_eq!(lock(&daemon.state).next_seq, seq);
        assert_eq!(binding_of(&daemon, &receiver.id), binding);
        daemon.tend_controllers();
        assert!(is_running(&me()));
    }
}
