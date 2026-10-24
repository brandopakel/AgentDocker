//! `agentdocker up` / `down`: start and stop the agents in an Agentfile.
//!
//! `up` is a client of the daemon like any other: a `run` for each entry,
//! and, for an entry that waits on another's exit (`after`), the daemon's
//! own events to learn how that one ended. What starts, what waits and
//! what is never started is decided in `agentdocker_core::agentfile`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use agentdocker_core::agentfile::{self, Ending, Entry, Progress, Readiness, Seen};
use agentdocker_core::{AgentId, AgentRecord, AgentSpec, ErrorCode, EventKind};
use agentdocker_core::{Request, Response};
use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;

use crate::agentfile::Agentfile;
use crate::client::Client;

/// Start every agent in the file (or just `only`) that is not already live,
/// each once what it waits on allows. Prints the id of each agent it
/// started on stdout, one per line, so scripts can capture them; progress
/// goes to stderr. With an `after` in the file it stays until every entry
/// has started or is known never to; an entry that is never started says
/// why, and makes the command fail.
pub async fn up(client: &Client, file: Option<&Path>, only: &[String]) -> Result<()> {
    let (agentfile, path) = Agentfile::load(file)?;
    // Refuses a name that is not in the file.
    agentfile.specs(&path, only)?;
    let specs = agentfile.specs(&path, &[])?;
    let entries = agentfile.entries();
    let endings = if entries.iter().any(|entry| !entry.after.is_empty()) {
        // Subscribed before anything starts, so no ending is missed.
        Endings::subscribe(client).await
    } else {
        None
    };
    let mut run = Bringup {
        client,
        order: agentfile::order(&entries),
        waited: entries
            .iter()
            .flat_map(|entry| entry.after.iter().map(|after| after.name.clone()))
            .collect(),
        entries,
        specs,
        progress: BTreeMap::new(),
        watching: HashMap::new(),
        seen: HashMap::new(),
        shown: HashMap::new(),
        not_started: 0,
        endings,
    };
    run.seed(&path, only).await?;
    run.run().await?;
    if run.not_started > 0 {
        bail!("{} agent(s) not started", run.not_started);
    }
    Ok(())
}

/// One `up`, from the first `run` until every entry has started or will
/// not.
struct Bringup<'a> {
    client: &'a Client,
    /// File order, and the specs `run` is sent, index for index.
    entries: Vec<Entry>,
    specs: Vec<AgentSpec>,
    /// Indices into `entries`, what is waited on first.
    order: Vec<usize>,
    progress: BTreeMap<String, Progress>,
    /// The agent behind each started entry, until it has ended for good.
    watching: HashMap<AgentId, String>,
    /// What the events (and looks at the records) have shown of each
    /// watched agent's run: a stop, an exit 0 delivered.
    seen: HashMap<AgentId, Seen>,
    /// Entries something waits for with `after`, whose endings are said.
    waited: HashSet<String>,
    /// What each waiting entry was last said to wait on.
    shown: HashMap<String, Vec<String>>,
    not_started: usize,
    /// The daemon's events about endings; `None` once there is no stream,
    /// and each watched agent is looked at once a second instead.
    endings: Option<Endings>,
}

/// How often `up` looks at what it waits on when it has no event stream.
const POLL: Duration = Duration::from_secs(1);

impl Bringup<'_> {
    /// What is known before anything starts: an entry already live is
    /// left running and watched; one left out by `only` is what its latest
    /// record from this file says, or has not run. Nothing here watched
    /// that record end, so an exit 0 on it counts only when the record
    /// shows the run was the entry's own and no stop was asked of it
    /// (`agentfile::own_run`); a stop marked on it is a stop.
    async fn seed(&mut self, path: &Path, only: &[String]) -> Result<()> {
        let live = live_agents(self.client).await?;
        let recorded = if only.is_empty() {
            Vec::new()
        } else {
            recorded_agents(self.client, path).await?
        };
        for i in 0..self.entries.len() {
            let name = self.entries[i].name.clone();
            let selected = only.is_empty() || only.contains(&name);
            if let Some(agent) = live.iter().find(|a| a.spec.name == name) {
                if selected {
                    eprintln!("{name:<24} already running   {}", agent.id.short());
                }
                self.track(&name, agent);
            } else if selected {
                self.progress.insert(name, Progress::Pending);
            } else {
                match recorded
                    .iter()
                    .filter(|a| a.spec.name == name)
                    .max_by_key(|a| a.created_at)
                {
                    Some(agent) => match Ending::before(agent, &self.specs[i]) {
                        Some(ending) => self.end(&name, ending),
                        // Live, or about to be restarted: watched from here.
                        None => self.track(&name, agent),
                    },
                    None => self.end(&name, Ending::NeverRan),
                }
            }
        }
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        loop {
            self.decide().await;
            if !self.progress.values().any(|p| *p == Progress::Pending) {
                return Ok(());
            }
            // What still waits, waits on an agent that is running; with
            // none being watched nothing could ever decide it.
            if self.watching.is_empty() {
                self.give_up("nothing it waits on is running");
                return Ok(());
            }
            let look = tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    self.give_up("up was interrupted");
                    return Ok(());
                }
                look = self.next() => look,
            };
            for id in look {
                self.look(&id).await?;
            }
        }
    }

    /// Decide every pending entry, what it waits on first, until a pass
    /// changes nothing: a chain of starts or of failures settles at once.
    async fn decide(&mut self) {
        loop {
            let mut changed = false;
            for i in self.order.clone() {
                let name = self.entries[i].name.clone();
                if self.progress.get(&name) != Some(&Progress::Pending) {
                    continue;
                }
                match agentfile::readiness(&self.entries[i], &self.progress) {
                    Readiness::Ready => {
                        self.start(i).await;
                        changed = true;
                    }
                    Readiness::Waiting(on) => {
                        if self.shown.get(&name) != Some(&on) {
                            eprintln!("{name:<24} waiting: {}", on.join(", "));
                            self.shown.insert(name, on);
                        }
                    }
                    Readiness::Blocked { reason, ending } => {
                        eprintln!("{name:<24} not started: {reason}");
                        self.progress.insert(name, Progress::Ended(ending));
                        self.not_started += 1;
                        changed = true;
                    }
                }
            }
            if !changed {
                return;
            }
        }
    }

    async fn start(&mut self, i: usize) {
        let name = self.entries[i].name.clone();
        // Ordering alone is not enough: a dependency that is `created`
        // has not run its first line yet, and the agent about to start
        // may be a client of it. Wait for it to actually be running.
        for needed in self.entries[i].depends_on.clone() {
            if let Err(err) = wait_for(self.client, &needed).await {
                let cause = format!("{err:#}");
                eprintln!("{name:<24} not started: {cause}");
                self.progress
                    .insert(name, Progress::Ended(Ending::NotStarted { cause }));
                self.not_started += 1;
                return;
            }
        }
        let mut spec = self.specs[i].clone();
        let roles = spec.labels.remove(agentdocker_core::agent::ROLE_LABEL);
        let reason = match self.client.call(&Request::Run { spec }).await {
            Ok(Response::Agent { agent }) => {
                eprintln!("{name:<24} started           {}", agent.id.short());
                println!("{}", agent.id);
                if let Some(roles) = roles {
                    give_roles(self.client, agent.id.as_str(), &roles).await;
                }
                self.track(&name, &agent);
                return;
            }
            Ok(other) => {
                eprintln!("{name:<24} unexpected reply: {other:?}");
                "the daemon's reply was not an agent".to_owned()
            }
            Err(err) => {
                eprintln!("{name:<24} failed: {err:#}");
                format!("{err:#}")
            }
        };
        self.progress
            .insert(name, Progress::Ended(Ending::FailedToStart { reason }));
        self.not_started += 1;
    }

    /// Watch an entry's agent from a record read of it, or record how it
    /// ended. The read says only that it is live or how it failed; success
    /// is an exit 0 taken from its own event, and with no stream to bring
    /// that event, an exit 0 does not count.
    fn track(&mut self, name: &str, agent: &AgentRecord) {
        let seen = self
            .seen
            .entry(agent.id.clone())
            .or_insert_with(|| Seen::new(agent));
        seen.record(agent);
        match Ending::judged(agent, *seen, self.endings.is_some()) {
            Some(ending) => {
                self.watching.remove(&agent.id);
                self.end(name, ending);
            }
            None => {
                self.watching.insert(agent.id.clone(), name.to_owned());
                self.progress.insert(name.to_owned(), Progress::Started);
            }
        }
    }

    fn end(&mut self, name: &str, ending: Ending) {
        if self.waited.contains(name) {
            eprintln!("{name:<24} {ending}");
        }
        self.progress
            .insert(name.to_owned(), Progress::Ended(ending));
    }

    /// The agents to look at again: those the daemon has said something
    /// about, or, with no event stream, every one being watched.
    async fn next(&mut self) -> Vec<AgentId> {
        let Some(endings) = self.endings.as_mut() else {
            tokio::time::sleep(POLL).await;
            return self.watching.keys().cloned().collect();
        };
        let Some(first) = endings.rx.recv().await else {
            eprintln!(
                "agentdocker: the daemon's event stream ended; looking at what is waited on every second, \
                 where an exit 0 cannot be shown to be its own and does not count"
            );
            self.endings = None;
            return self.watching.keys().cloned().collect();
        };
        let mut look = Vec::new();
        let mut kinds = vec![first];
        while let Ok(kind) = endings.rx.try_recv() {
            kinds.push(kind);
        }
        // In the order the daemon published them, so a stop is known
        // before the exit it ends in.
        for kind in kinds {
            let Some(agent) = agentfile::lifecycle_agent(&kind) else {
                continue;
            };
            if !self.watching.contains_key(agent) {
                continue;
            }
            if let Some(seen) = self.seen.get_mut(agent) {
                seen.event(&kind);
            }
            if !look.contains(agent) {
                look.push(agent.clone());
            }
        }
        look
    }

    async fn look(&mut self, id: &AgentId) -> Result<()> {
        let Some(name) = self.watching.get(id).cloned() else {
            return Ok(());
        };
        let request = Request::Inspect {
            agent: id.to_string(),
        };
        let answer = self
            .client
            .call_raw(&request)
            .await
            .with_context(|| format!("while waiting on `{name}`"))?;
        match answer {
            Response::Agent { agent } => self.track(&name, &agent),
            Response::Error {
                code: ErrorCode::NotFound,
                ..
            } => {
                self.watching.remove(id);
                self.end(
                    &name,
                    Ending::Failed {
                        reason: "its record is gone".to_owned(),
                    },
                );
            }
            other => {
                crate::client::into_result(other)
                    .with_context(|| format!("while waiting on `{name}`"))?;
            }
        }
        Ok(())
    }

    /// Stop waiting (Ctrl-C, say): whatever still waits is not started.
    /// What already started is the daemon's and runs on.
    fn give_up(&mut self, cause: &str) {
        for (name, progress) in &mut self.progress {
            if *progress == Progress::Pending {
                eprintln!("{name:<24} not started: {cause}");
                *progress = Progress::Ended(Ending::NotStarted {
                    cause: cause.to_owned(),
                });
                self.not_started += 1;
            }
        }
    }
}

/// The daemon's events about endings — a stop, an exit, a restart — passed
/// on from a stream of their own.
struct Endings {
    rx: mpsc::UnboundedReceiver<EventKind>,
    task: tokio::task::JoinHandle<()>,
}

/// How long the daemon has to acknowledge the subscription.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(5);

impl Endings {
    /// `None` when the daemon does not acknowledge the subscription in
    /// time; `up` then looks at what it waits on once a second.
    async fn subscribe(client: &Client) -> Option<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let client = client.clone();
        let task = tokio::spawn(async move {
            let _ = client
                .stream_after(
                    &Request::Events {
                        replay: 0,
                        ready: true,
                    },
                    async move {
                        let _ = ready_tx.send(());
                        Ok(())
                    },
                    move |(), response| {
                        if let Response::Event { event } = response
                            && agentfile::lifecycle_agent(&event.kind).is_some()
                        {
                            return Ok(tx.send(event.kind).is_ok());
                        }
                        Ok(!tx.is_closed())
                    },
                )
                .await;
        });
        match tokio::time::timeout(SUBSCRIBE_TIMEOUT, ready_rx).await {
            Ok(Ok(())) => Some(Self { rx, task }),
            _ => {
                task.abort();
                eprintln!(
                    "agentdocker: no event stream from the daemon; looking at what is waited on every second, \
                     where an exit 0 cannot be shown to be its own and does not count"
                );
                None
            }
        }
    }
}

impl Drop for Endings {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Give a launched agent the roles its spec named. A launch never carries
/// a role in (the daemon drops it): roles are the person's to give, so this
/// asks as the person, and the daemon refuses it when this runs inside an
/// agent's session. A role it cannot give is said, and the agent runs on.
pub(crate) async fn give_roles(client: &Client, agent: &str, roles: &str) {
    let add: Vec<String> = roles
        .split(',')
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(str::to_owned)
        .collect();
    if add.is_empty() {
        return;
    }
    let request = Request::RoleAssign {
        from: agentdocker_core::HUMAN.to_owned(),
        agent: agent.to_owned(),
        add,
        remove: Vec::new(),
    };
    if let Err(error) = client.call(&request).await {
        eprintln!(
            "{agent}: started, but its roles ({roles}) were not given: {error:#}; give them with `agentdocker role`"
        );
    }
}

/// Wait until an agent is running, or say why it never will be.
async fn wait_for(client: &Client, name: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + DEPENDENCY_TIMEOUT;
    loop {
        let live = live_agents(client).await?;
        match live.iter().find(|a| a.spec.name == name) {
            Some(agent) if agent.status == agentdocker_core::AgentStatus::Running => {
                return Ok(());
            }
            Some(_) => {}
            None => {}
        }
        if std::time::Instant::now() >= deadline {
            bail!("`{name}` did not start within {DEPENDENCY_TIMEOUT:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// How long `up` waits for a dependency before giving up on it. Long
/// enough for a server to bind a port, short enough that a typo does not
/// hang the terminal.
const DEPENDENCY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Stop every live agent named in the file (or just `only`).
pub async fn down(
    client: &Client,
    file: Option<&Path>,
    only: &[String],
    force: bool,
) -> Result<()> {
    let (agentfile, path) = Agentfile::load(file)?;
    let specs = agentfile.specs(&path, only)?;
    let live = live_agents(client).await?;
    let mut failed = 0;
    for spec in specs {
        let name = spec.name;
        let Some(agent) = live.iter().find(|a| a.spec.name == name) else {
            println!("{name:<24} not running");
            continue;
        };
        let request = Request::Stop {
            agent: agent.id.to_string(),
            force,
        };
        match client.call(&request).await {
            Ok(_) => println!("{name:<24} stopping          {}", agent.id.short()),
            Err(err) => {
                eprintln!("{name:<24} failed: {err:#}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        bail!("{failed} agent(s) failed to stop");
    }
    Ok(())
}

async fn live_agents(client: &Client) -> Result<Vec<AgentRecord>> {
    match client
        .call(&Request::List {
            all: false,
            project: None,
            labels: Default::default(),
        })
        .await?
    {
        Response::Agents { agents, .. } => Ok(agents),
        other => bail!("unexpected reply to list: {other:?}"),
    }
}

/// Every agent on record from this file, live or not.
async fn recorded_agents(client: &Client, path: &Path) -> Result<Vec<AgentRecord>> {
    let labels = BTreeMap::from([("agentfile".to_owned(), path.display().to_string())]);
    match client
        .call(&Request::List {
            all: true,
            project: None,
            labels,
        })
        .await?
    {
        Response::Agents { agents, .. } => Ok(agents),
        other => bail!("unexpected reply to list: {other:?}"),
    }
}
