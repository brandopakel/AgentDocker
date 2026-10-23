//! The opt-in judge, `[judge]` in `agentd.toml`: an agent's quiet screen
//! read by a TypeSafe System One model (Jev) for whether it waits on the
//! person. Off unless the person names a judgment and a key; then, and
//! only then, screens are read and their last lines sent.
//!
//! Every five seconds the file and the key are read — on their own, so a
//! change is seen even while a pass waits on an answer — and a pass reads
//! the screens of live agents AgentDocker already holds or is bound to: the
//! terminal of a managed agent (its scrollback drawn as the agent drew it,
//! at a bounded size), and the tmux pane an agent registered from, at the
//! server tmux named to it and only while that pane holds the agent's own
//! process. A herdr pane is not read: its id can change when the pane
//! moves, so the id an agent registered with may name another pane, and
//! nothing here binds a pane to the agent's own session before reading it.
//! A screen whose last lines have not changed for
//! [`judgment::QUIET_SECS`] is quiet; a quiet screen is asked about once,
//! and not again until it changes. It is read again just before it is
//! sent and again before its answer is applied: a screen that moved on in
//! between is not sent, or its answer is dropped, and its quiet period
//! starts over. A change of the file or the key drops whatever the pass
//! still holds from before it. Calls are budgeted per minute, made one at
//! a time under a deadline, with no redirects; a failure becomes the
//! judge's status and, at most once a minute, a `judge_failed` event, and
//! never stops the daemon. What an answer may do is core's rule
//! ([`judgment::decide`], [`judgment::raise`]): promote to waiting for the
//! person, never anything else. A promotion is kept on the agent's record
//! until its screen changes, another process is behind it, or the judge is
//! turned off.
//!
//! The key leaves the process only in the `Authorization` header; it is
//! never logged, and no status or event names it or the address.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::time::{Duration as StdDuration, Instant};

use super::*;
use agentdocker_core::Activity;
use agentdocker_core::config::JudgeConfig;
use agentdocker_core::judgment::{
    self, JudgeState, JudgeStatus, Outcome, ScreenJudgment, Thresholds,
};
use agentdocker_core::multiplexer::Evidence;
use sha2::{Digest, Sha256};

/// The whole of one call: connecting, sending and reading the answer.
const CALL_DEADLINE: StdDuration = StdDuration::from_secs(10);
/// After a call fails for a reason that may pass, none for this long.
const BACKOFF: StdDuration = StdDuration::from_secs(60);
/// Failures are announced at most this often.
const NOTICE_EVERY: StdDuration = StdDuration::from_secs(60);
/// The longest answer read.
const ANSWER_BYTES: u64 = 64 * 1024;
/// The budget's window.
const MINUTE: StdDuration = StdDuration::from_secs(60);
/// How far up its parents an agent's process is followed to find the
/// pane's process; deeper than any shell, launcher and agent chain.
const ANCESTRY: usize = 64;

/// Where an agent's screen is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    /// The terminal the daemon holds for a managed agent.
    Terminal,
    /// The tmux pane the agent registered from, at the server it named.
    Tmux { socket: PathBuf, pane: String },
}

/// The pane an agent registered from and the server it is on, when tmux
/// told the agent so itself: a pane seen only in its ancestry cannot be
/// named, and a pane id without its server could be any server's.
fn tmux_pane(record: &AgentRecord) -> Option<(PathBuf, String)> {
    let session = record
        .session
        .as_ref()
        .filter(|s| s.kind == "tmux" && s.evidence == Evidence::Environment)?;
    let socket = PathBuf::from(session.socket.as_deref()?);
    socket
        .is_absolute()
        .then(|| (socket, session.pane.clone()))
        .and_then(|(socket, pane)| Some((socket, pane?)))
}

/// Whether `agent` is `ancestor` or one of its descendants, by a process
/// table of each process's parent.
fn descends(parents: &HashMap<u32, u32>, agent: u32, ancestor: u32) -> bool {
    let mut current = agent;
    for _ in 0..ANCESTRY {
        if current == ancestor {
            return true;
        }
        match parents.get(&current) {
            Some(&parent) if current > 1 && parent != current => current = parent,
            _ => return false,
        }
    }
    false
}

/// One screen being watched.
struct Watched {
    process: Option<DateTime<Utc>>,
    digest: String,
    changed: Instant,
    changed_at: DateTime<Utc>,
    /// Already asked about in this quiet period.
    settled: bool,
}

impl Watched {
    fn fresh(process: Option<DateTime<Utc>>, digest: String) -> Self {
        Self {
            process,
            digest,
            changed: Instant::now(),
            changed_at: Utc::now(),
            settled: false,
        }
    }
}

/// How the judge is set.
#[derive(Clone, Debug, Default)]
enum Setting {
    #[default]
    Off,
    On {
        judgments: Vec<String>,
        model: String,
        calls_per_minute: u32,
    },
    Unusable(String),
}

/// What a pass runs with: the file's judge section and the key as they
/// were read, under the generation they were read in.
pub(in crate::daemon) struct Setup {
    config: JudgeConfig,
    key: String,
    generation: u64,
}

/// The judge's own state, off the state lock.
pub(in crate::daemon) struct Judge {
    setting: Setting,
    /// The setup in force, while on.
    running: Option<Arc<Setup>>,
    /// A digest of the section and the key in force, so a change of
    /// either is a new generation; never the key itself.
    fingerprint: Option<[u8; 32]>,
    /// Bumped by every change of the setup and by every stop: a pass holds
    /// on to nothing from an earlier one.
    generation: u64,
    /// Reads of the file are numbered as they start, and only one newer
    /// than the last applied is applied, so a slow read never undoes a
    /// newer one.
    reads_started: u64,
    reads_applied: u64,
    screens: HashMap<AgentId, Watched>,
    calls: VecDeque<Instant>,
    backoff_until: Option<Instant>,
    /// The last call's failure, until one succeeds.
    problem: Option<String>,
    /// Failures not yet announced, the last one's word, and when the last
    /// announcement was made.
    unannounced: u64,
    last_reason: &'static str,
    announced: Option<Instant>,
    /// How long a screen must be unchanged, how long a call and a capture
    /// may take, and the tmux that is run: constants, changed by tests.
    quiet: StdDuration,
    deadline: StdDuration,
    capture_deadline: StdDuration,
    tmux: PathBuf,
    /// Screens read, for tests that prove none is read while off.
    reads: u64,
    /// A test's hold on a positive answer between its generation check and
    /// its record: the pass meets the first barrier, then waits on the
    /// second.
    #[cfg(test)]
    pause: Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>,
}

impl Default for Judge {
    fn default() -> Self {
        Self {
            setting: Setting::Off,
            running: None,
            fingerprint: None,
            generation: 0,
            reads_started: 0,
            reads_applied: 0,
            screens: HashMap::new(),
            calls: VecDeque::new(),
            backoff_until: None,
            problem: None,
            unannounced: 0,
            last_reason: "",
            announced: None,
            quiet: StdDuration::from_secs(judgment::QUIET_SECS),
            deadline: CALL_DEADLINE,
            capture_deadline: multiplexer::tmux::CAPTURE_DEADLINE,
            tmux: PathBuf::from("tmux"),
            reads: 0,
            #[cfg(test)]
            pause: None,
        }
    }
}

impl Judge {
    fn prune(&mut self, now: Instant) {
        while self
            .calls
            .front()
            .is_some_and(|at| now.duration_since(*at) >= MINUTE)
        {
            self.calls.pop_front();
        }
    }

    /// Forget everything watched and the setup: nothing is kept while off,
    /// and nothing a pass still holds is current any more.
    fn stop(&mut self, setting: Setting) {
        // Both go, whatever either held: a fingerprint left behind would
        // make the same section and key, turned on again, look unchanged
        // and never start a setup.
        self.running = None;
        self.fingerprint = None;
        self.generation += 1;
        self.setting = setting;
        self.screens.clear();
        self.calls.clear();
        self.backoff_until = None;
        self.problem = None;
    }

    /// Whether `generation` is still the one in force.
    fn current(&self, generation: u64) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| running.generation == generation)
    }

    /// Count a failure; the event to emit when one is due.
    fn fail(&mut self, failure: Failure, now: Instant) -> Option<EventKind> {
        self.problem = Some(failure.words());
        self.unannounced += 1;
        self.last_reason = failure.word();
        if failure.passes() {
            self.backoff_until = Some(now + BACKOFF);
        }
        self.announce(now)
    }

    /// The failures not yet announced, once a minute has passed since the
    /// last announcement: a loss inside that minute is said when it lifts.
    fn announce(&mut self, now: Instant) -> Option<EventKind> {
        if self.unannounced == 0
            || self
                .announced
                .is_some_and(|at| now.duration_since(at) < NOTICE_EVERY)
        {
            return None;
        }
        self.announced = Some(now);
        let failures = std::mem::take(&mut self.unannounced);
        Some(EventKind::JudgeFailed {
            reason: self.last_reason.to_owned(),
            failures,
        })
    }
}
/// Why a call came to nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    /// 401 or 403: the key was not taken.
    Unauthorized(u16),
    /// 429.
    RateLimited,
    /// 5xx, 529 among them.
    Unavailable(u16),
    /// Any other answer than 2xx: the request was not taken.
    Refused(u16),
    Timeout,
    Unreachable,
    /// An answer without the contract's shape.
    Malformed,
}

impl Failure {
    fn word(self) -> &'static str {
        match self {
            Self::Unauthorized(_) => "unauthorized",
            Self::RateLimited => "rate_limited",
            Self::Unavailable(_) => "unavailable",
            Self::Refused(_) => "refused",
            Self::Timeout => "timeout",
            Self::Unreachable => "unreachable",
            Self::Malformed => "malformed",
        }
    }

    /// For the status, without the address or anything sent.
    fn words(self) -> String {
        match self {
            Self::Unauthorized(code) => format!("was not authorized (HTTP {code}); check the key"),
            Self::RateLimited => "was rate limited (HTTP 429)".to_owned(),
            Self::Unavailable(code) => format!("found the service unavailable (HTTP {code})"),
            Self::Refused(code) => format!("was refused (HTTP {code})"),
            Self::Timeout => "timed out".to_owned(),
            Self::Unreachable => "could not connect".to_owned(),
            Self::Malformed => "gave an answer the contract does not have".to_owned(),
        }
    }

    /// Whether it may pass by itself: then no call for a while, and the
    /// screen is asked about again. Otherwise the screen is settled: the
    /// same request would be refused the same way.
    fn passes(self) -> bool {
        matches!(
            self,
            Self::Unauthorized(_)
                | Self::RateLimited
                | Self::Unavailable(_)
                | Self::Timeout
                | Self::Unreachable
        )
    }
}

fn transport(error: &ureq::Error) -> Failure {
    match error {
        ureq::Error::Timeout(_) => Failure::Timeout,
        _ => Failure::Unreachable,
    }
}

/// One call: the request, the key as a bearer token and nothing else.
fn call(url: &str, key: &str, body: &Value, deadline: StdDuration) -> Result<Value, Failure> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(deadline))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(url)
        .header("Authorization", &format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .send(body.to_string().as_bytes())
        .map_err(|error| transport(&error))?;
    match response.status().as_u16() {
        200..=299 => {}
        code @ (401 | 403) => return Err(Failure::Unauthorized(code)),
        429 => return Err(Failure::RateLimited),
        code @ 500..=599 => return Err(Failure::Unavailable(code)),
        code => return Err(Failure::Refused(code)),
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(ANSWER_BYTES)
        .read_to_string()
        .map_err(|error| match error {
            ureq::Error::Timeout(_) => Failure::Timeout,
            _ => Failure::Malformed,
        })?;
    serde_json::from_str(&text).map_err(|_| Failure::Malformed)
}

/// The key, read through a verified private handle: a regular file of
/// this user, mode 0600, holding one token.
fn read_key(path: &Path) -> Result<String, String> {
    let key = super::webhooks::read_secret(path)
        .map_err(|reason| format!("judge: {}", reason.replace("secret_file", "key_file")))?;
    String::from_utf8(key)
        .ok()
        .filter(|key| key.bytes().all(|b| b.is_ascii_graphic()))
        .ok_or_else(|| "judge: the key file does not hold a key".to_owned())
}

/// A managed agent's screen, drawn from what it printed into the window
/// it printed into — at most [`supervisor::SCREEN_COLS`] by
/// [`supervisor::SCREEN_ROWS`], whatever a client asked for, since the
/// emulator allocates every cell of the window at once.
fn draw(bytes: &[u8], (cols, rows): (u16, u16)) -> String {
    let mut parser = vt100::Parser::new(
        rows.clamp(1, supervisor::SCREEN_ROWS),
        cols.clamp(1, supervisor::SCREEN_COLS),
        0,
    );
    parser.process(bytes);
    parser.screen().contents()
}

fn digest(lines: &[String]) -> String {
    Sha256::digest(lines.join("\n").as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A live agent whose screen may be judged.
struct Candidate {
    agent: AgentId,
    pid: Option<u32>,
    process: Option<DateTime<Utc>>,
    source: Source,
    /// Working, idle or unknown by its record, before any judgment.
    eligible: bool,
    standing: Option<ScreenJudgment>,
}

/// A quiet screen to ask about, and where to read it again.
struct Due {
    agent: AgentId,
    pid: Option<u32>,
    process: Option<DateTime<Utc>>,
    source: Source,
    lines: Vec<String>,
    digest: String,
    since: DateTime<Utc>,
    read_at: DateTime<Utc>,
}

/// What one pass reads screens with: the managed terminals as they were
/// when it began, and each process's parent, when a tmux pane is to be
/// read.
struct Pass {
    terminals: HashMap<AgentId, supervisor::Session>,
    parents: Option<Arc<HashMap<u32, u32>>>,
}

impl Daemon {
    /// Every tick: read the file and the key, whatever a pass is doing, so
    /// turning the judge off or changing it is seen at once; and start a
    /// pass unless one is under way.
    pub fn judge_screens(self: &Arc<Self>) {
        if !self.judge_watching.swap(true, Ordering::SeqCst) {
            let daemon = self.clone();
            tokio::spawn(async move {
                daemon.judge_setup().await;
                daemon.judge_watching.store(false, Ordering::SeqCst);
            });
        }
        if self.judging.swap(true, Ordering::SeqCst) {
            return;
        }
        let daemon = self.clone();
        tokio::spawn(async move {
            daemon.judge_once().await;
            daemon.judging.store(false, Ordering::SeqCst);
        });
    }

    /// How the judge stands, for `ping`.
    pub(in crate::daemon) fn judge_status(&self) -> JudgeStatus {
        let mut judge = lock(&self.judge);
        judge.prune(Instant::now());
        match &judge.setting {
            Setting::Off => JudgeStatus::Off,
            Setting::Unusable(reason) => JudgeStatus::Unusable {
                reason: reason.clone(),
            },
            Setting::On {
                judgments,
                model,
                calls_per_minute,
            } => JudgeStatus::On {
                judgments: judgments.clone(),
                model: model.clone(),
                calls_last_minute: u32::try_from(judge.calls.len()).unwrap_or(u32::MAX),
                calls_per_minute: *calls_per_minute,
                problem: judge.problem.clone(),
            },
        }
    }

    /// Read the file and the key and bring the judge in step with them: a
    /// new section or key is a new generation, and off or unusable stops
    /// it and takes every standing judgment off. The setup in force after,
    /// if the judge is on.
    pub(in crate::daemon) async fn judge_setup(self: &Arc<Self>) -> Option<Arc<Setup>> {
        let ticket = {
            let mut judge = lock(&self.judge);
            judge.reads_started += 1;
            judge.reads_started
        };
        let home = self.home.clone();
        let read = tokio::task::spawn_blocking(
            move || -> Result<Option<(JudgeConfig, String)>, String> {
                let judge = crate::config::read(&home)?.judge;
                if judge.enabled()?.is_empty() {
                    return Ok(None);
                }
                let path = judge.key_file.clone().expect("checked by enabled");
                let key = read_key(&path)?;
                Ok(Some((judge, key)))
            },
        )
        .await
        .unwrap_or_else(|_| Err("judge: the configuration read did not complete".into()));
        // What this read changed, if anything: the generation it made, how
        // the judge stands under it and why, and a reason to say once.
        let (changed, notice, announced, running) = {
            let mut judge = lock(&self.judge);
            let (mut changed, mut notice) = (None, None);
            if ticket > judge.reads_applied {
                judge.reads_applied = ticket;
                match &read {
                    Ok(Some((config, key))) => {
                        let fingerprint: [u8; 32] =
                            Sha256::digest(format!("{config:?}\n{key}").as_bytes()).into();
                        if judge.fingerprint != Some(fingerprint) || judge.running.is_none() {
                            judge.generation += 1;
                            judge.fingerprint = Some(fingerprint);
                            judge.running = Some(Arc::new(Setup {
                                config: config.clone(),
                                key: key.clone(),
                                generation: judge.generation,
                            }));
                            changed = Some((judge.generation, JudgeState::On, None));
                        }
                        judge.setting = Setting::On {
                            judgments: config.judgments.clone(),
                            model: config.model.clone(),
                            calls_per_minute: config.calls_per_minute,
                        };
                    }
                    Ok(None) => {
                        judge.stop(Setting::Off);
                        changed = Some((judge.generation, JudgeState::Off, None));
                    }
                    Err(reason) => {
                        judge.stop(Setting::Unusable(reason.clone()));
                        changed =
                            Some((judge.generation, JudgeState::Unusable, Some(reason.clone())));
                        notice = Some(reason.clone());
                    }
                }
            }
            let announced = judge
                .running
                .is_some()
                .then(|| judge.announce(Instant::now()))
                .flatten();
            (changed, notice, announced, judge.running.clone())
        };
        if let Some(notice) = notice {
            self.config_notice(notice);
        }
        if let Some((generation, status, reason)) = changed {
            self.judge_sync(generation, status, reason);
        }
        if let Some(event) = announced {
            self.emit(event);
        }
        running
    }

    /// Bring the state lock's view of the judge to `generation`, unless it
    /// has seen a later one already. A change of how it stands — on, off,
    /// or unusable and why — is announced as `judge_status_changed` in the
    /// same hold, so the events keep the order of the generations; a read
    /// that leaves it as it was announces nothing, though a stop makes a
    /// new generation every time. Turned off, every standing judgment
    /// comes off in that hold too: a judgment is recorded only under the
    /// generation this lock holds while the judge is on (`record_judgment`),
    /// so once a stop has cleared here, nothing judged before it can be
    /// written after.
    fn judge_sync(&self, generation: u64, status: JudgeState, reason: Option<String>) {
        let mut state = lock(&self.state);
        if generation <= state.judge_generation {
            return;
        }
        state.judge_generation = generation;
        if (state.judge_status, state.judge_reason.as_ref()) != (status, reason.as_ref()) {
            state.judge_status = status;
            state.judge_reason = reason.clone();
            state.emit(EventKind::JudgeStatusChanged { status, reason });
        }
        if status == JudgeState::On {
            return;
        }
        let standing: Vec<(AgentId, String)> = state
            .registry
            .all()
            .filter_map(|record| {
                record
                    .judged
                    .as_ref()
                    .map(|j| (record.id.clone(), j.screen.clone()))
            })
            .collect();
        for (agent, screen) in standing {
            state.clear_judgment(&agent, &screen, "judge_off");
        }
    }

    fn judge_current(&self, generation: u64) -> bool {
        lock(&self.judge).current(generation)
    }

    /// One pass: the setup in force, then every screen there is to read,
    /// then the quiet ones asked about.
    pub(in crate::daemon) async fn judge_once(self: &Arc<Self>) {
        let Some(setup) = self.judge_setup().await else {
            return;
        };
        let (pass, due) = self.read_screens(&setup).await;
        self.ask_judge(&setup, &pass, due).await;
    }

    /// One screen, as it is now: the terminal drawn, or the tmux pane
    /// captured at its server only while it holds the agent's process —
    /// the process the record names, born when the record says.
    async fn read_source(
        &self,
        pass: &Pass,
        agent: &AgentId,
        source: &Source,
        pid: Option<u32>,
        process: Option<DateTime<Utc>>,
    ) -> Option<Vec<String>> {
        let text = match source {
            Source::Terminal => {
                let (bytes, size) = pass.terminals.get(agent)?.screen();
                draw(&bytes, size)
            }
            Source::Tmux { socket, pane } => {
                let (pid, parents) = (pid?, pass.parents.clone()?);
                let (program, deadline) = {
                    let judge = lock(&self.judge);
                    (judge.tmux.clone(), judge.capture_deadline)
                };
                let (socket, pane) = (socket.clone(), pane.clone());
                tokio::task::spawn_blocking(move || {
                    if process.is_some() && agentdocker_host::procinfo::start_time(pid) != process {
                        return None;
                    }
                    multiplexer::tmux::capture_bound(
                        &program,
                        &socket,
                        &pane,
                        |holder| descends(&parents, pid, holder),
                        deadline,
                    )
                    .ok()
                })
                .await
                .ok()??
            }
        };
        lock(&self.judge).reads += 1;
        Some(judgment::screen_tail(&text))
    }

    /// Read every screen there is to read, keep track of which have gone
    /// quiet, take off judgments whose screen moved on, and say which
    /// quiet screens are due to be asked about.
    async fn read_screens(self: &Arc<Self>, setup: &Setup) -> (Pass, Vec<Due>) {
        let terminals: HashMap<AgentId, supervisor::Session> = lock(&self.sessions).clone();
        let now = Utc::now();
        let mut candidates: Vec<Candidate> = {
            let state = lock(&self.state);
            state
                .registry
                .all()
                .filter(|record| record.status.is_live())
                .filter_map(|record| {
                    let source = if terminals.contains_key(&record.id) {
                        Source::Terminal
                    } else {
                        let (socket, pane) = tmux_pane(record)?;
                        Source::Tmux { socket, pane }
                    };
                    Some(Candidate {
                        agent: record.id.clone(),
                        pid: record.pid,
                        process: record.process_started_at,
                        source,
                        eligible: matches!(
                            state.recorded_activity_of(record, now),
                            Activity::Working { .. } | Activity::Idle { .. } | Activity::Unknown
                        ),
                        standing: record.judged.clone(),
                    })
                })
                .collect()
        };
        // Terminals first: a tmux server that does not answer holds only
        // its own panes, each for at most the capture's deadline.
        candidates.sort_by_key(|c| matches!(c.source, Source::Tmux { .. }));
        let parents = if candidates
            .iter()
            .any(|c| matches!(c.source, Source::Tmux { .. }))
        {
            tokio::task::spawn_blocking(agentdocker_host::procinfo::processes)
                .await
                .ok()
                .and_then(Result::ok)
                .map(|table| Arc::new(table.into_iter().map(|p| (p.pid, p.ppid)).collect()))
        } else {
            None
        };
        let pass = Pass { terminals, parents };
        lock(&self.judge)
            .screens
            .retain(|agent, _| candidates.iter().any(|c| &c.agent == agent));
        let mut due = Vec::new();
        for candidate in candidates {
            if !self.judge_current(setup.generation) {
                return (pass, Vec::new());
            }
            let Some(lines) = self
                .read_source(
                    &pass,
                    &candidate.agent,
                    &candidate.source,
                    candidate.pid,
                    candidate.process,
                )
                .await
            else {
                continue;
            };
            let read_at = Utc::now();
            let screen = digest(&lines);
            let (quiet_for, since) = {
                let mut judge = lock(&self.judge);
                let quiet = judge.quiet;
                let watched = judge
                    .screens
                    .entry(candidate.agent.clone())
                    .or_insert_with(|| Watched::fresh(candidate.process, screen.clone()));
                if watched.digest != screen || watched.process != candidate.process {
                    *watched = Watched::fresh(candidate.process, screen.clone());
                }
                let quiet_now = !watched.settled && watched.changed.elapsed() >= quiet;
                (quiet_now, watched.changed_at)
            };
            if let Some(standing) = &candidate.standing {
                if standing.process_started_at != candidate.process {
                    self.clear_judgment(&candidate.agent, &standing.screen, "process_changed");
                } else if standing.screen != screen {
                    self.clear_judgment(&candidate.agent, &standing.screen, "screen_changed");
                }
                continue;
            }
            if quiet_for && candidate.eligible && !lines.is_empty() {
                due.push(Due {
                    agent: candidate.agent,
                    pid: candidate.pid,
                    process: candidate.process,
                    source: candidate.source,
                    lines,
                    digest: screen,
                    since,
                    read_at,
                });
            }
        }
        // The screen quiet longest is asked about first.
        due.sort_by_key(|d| d.since);
        (pass, due)
    }

    /// Whether the screen is still the one that went quiet. One that
    /// moved on starts its quiet period over, and is not sent or applied.
    async fn unchanged(&self, pass: &Pass, screen: &Due) -> bool {
        let now = self
            .read_source(
                pass,
                &screen.agent,
                &screen.source,
                screen.pid,
                screen.process,
            )
            .await
            .map(|lines| digest(&lines));
        if now.as_deref() == Some(screen.digest.as_str()) {
            return true;
        }
        let mut judge = lock(&self.judge);
        match now {
            Some(moved) => {
                judge
                    .screens
                    .insert(screen.agent.clone(), Watched::fresh(screen.process, moved));
            }
            None => {
                judge.screens.remove(&screen.agent);
            }
        }
        false
    }

    /// Ask about each due screen, one call at a time, within the budget,
    /// while the setup it was read under is in force.
    async fn ask_judge(self: &Arc<Self>, setup: &Setup, pass: &Pass, due: Vec<Due>) {
        let config = &setup.config;
        let thresholds = Thresholds::percent(config.threshold_percent);
        for screen in due {
            let deadline = {
                let mut judge = lock(&self.judge);
                let now = Instant::now();
                judge.prune(now);
                if !judge.current(setup.generation)
                    || judge.backoff_until.is_some_and(|until| now < until)
                    || judge.calls.len() >= config.calls_per_minute as usize
                {
                    break;
                }
                judge.deadline
            };
            if !self.unchanged(pass, &screen).await {
                continue;
            }
            {
                let mut judge = lock(&self.judge);
                if !judge.current(setup.generation) {
                    break;
                }
                judge.calls.push_back(Instant::now());
            }
            let body = judgment::screen_request(&config.model, &screen.lines);
            let (url, key) = (config.url.clone(), setup.key.clone());
            let answered = tokio::task::spawn_blocking(move || call(&url, &key, &body, deadline))
                .await
                .unwrap_or(Err(Failure::Unreachable))
                .and_then(|value| judgment::read_answers(&value).map_err(|_| Failure::Malformed));
            // Switched off or changed while it was out: whatever came back
            // belongs to a setup no longer in force.
            if !self.judge_current(setup.generation) {
                break;
            }
            match answered {
                Ok((model, answers)) => {
                    // The answers alone, never the screen: enough to see
                    // why a screen was or was not promoted.
                    debug!(
                        agent = %screen.agent.short(),
                        %model,
                        approval = answers.approval,
                        question = answers.question,
                        running = answers.running,
                        draft = answers.draft,
                        "judge answered"
                    );
                    lock(&self.judge).problem = None;
                    if !self.unchanged(pass, &screen).await {
                        continue;
                    }
                    self.settle(&screen.agent, &screen.digest);
                    if let Outcome::Promote {
                        reason,
                        probability,
                    } = judgment::decide(&answers, &thresholds)
                    {
                        let judged = ScreenJudgment {
                            reason,
                            permille: ScreenJudgment::permille(probability),
                            since: screen.since,
                            judged_at: screen.read_at,
                            process_started_at: screen.process,
                            screen: screen.digest.clone(),
                            model,
                        };
                        if !self.judge_current(setup.generation) {
                            break;
                        }
                        #[cfg(test)]
                        self.judge_pause().await;
                        self.record_judgment(&screen.agent, judged, setup.generation);
                    }
                }
                Err(failure) => {
                    if !failure.passes() {
                        self.settle(&screen.agent, &screen.digest);
                    }
                    warn!(reason = failure.word(), "a judge call failed");
                    let event = lock(&self.judge).fail(failure, Instant::now());
                    if let Some(event) = event {
                        self.emit(event);
                    }
                    if failure.passes() {
                        break;
                    }
                }
            }
        }
    }

    /// Where a test holds a positive answer after its generation check
    /// and before its record.
    #[cfg(test)]
    async fn judge_pause(&self) {
        let pause = lock(&self.judge).pause.clone();
        if let Some((reached, resume)) = pause {
            reached.wait().await;
            resume.wait().await;
        }
    }

    /// A screen asked about is not asked about again until it changes.
    fn settle(&self, agent: &AgentId, screen: &str) {
        if let Some(watched) = lock(&self.judge).screens.get_mut(agent)
            && watched.digest == screen
        {
            watched.settled = true;
        }
    }

    /// Keep a promotion on the agent's record, if it still applies: the
    /// setup it was asked under still in force by the state lock, the same live process,
    /// nothing reported by its adapter since the screen was read, and
    /// working, idle or unknown by its record.
    fn record_judgment(&self, agent: &AgentId, judgment: ScreenJudgment, generation: u64) -> bool {
        let mut state = lock(&self.state);
        // Checked in the same hold as the write: a stop syncs and clears
        // under this lock, so it either came first and this writes nothing,
        // or comes after and clears what this writes.
        if state.judge_status != JudgeState::On || state.judge_generation != generation {
            return false;
        }
        let Some(mut record) = state.registry.get(agent).cloned() else {
            return false;
        };
        let now = Utc::now();
        if !record.status.is_live()
            || record.process_started_at != judgment.process_started_at
            || record.judged.is_some()
            || record
                .reported_activity
                .as_ref()
                .is_some_and(|observation| observation.observed_at > judgment.judged_at)
            || !matches!(
                state.recorded_activity_of(&record, now),
                Activity::Working { .. } | Activity::Idle { .. } | Activity::Unknown
            )
        {
            return false;
        }
        info!(
            agent = %agent.short(),
            reason = ?judgment.reason,
            permille = judgment.permille,
            "judged waiting for the person from its screen"
        );
        record.judged = Some(judgment.clone());
        let event = Event::new(
            EventKind::AgentJudgedNeedsYou {
                agent: agent.clone(),
                judgment,
            },
            now,
        );
        state.transition(
            "screen judgment",
            event,
            |store, event| store.agent_transition(&record, event),
            |state| {
                if let Some(held) = state.registry.get_mut(agent) {
                    *held = record.clone();
                }
            },
        ) == Persisted::Committed
    }

    /// Take a judgment off an agent's record, if it is still the one
    /// judged from `screen`.
    fn clear_judgment(&self, agent: &AgentId, screen: &str, reason: &str) {
        lock(&self.state).clear_judgment(agent, screen, reason);
    }
}

impl State {
    /// Take a judgment off an agent's record, if it is still the one
    /// judged from `screen`, with its event, in the caller's hold.
    fn clear_judgment(&mut self, agent: &AgentId, screen: &str, reason: &str) {
        let Some(mut record) = self.registry.get(agent).cloned() else {
            return;
        };
        if record.judged.as_ref().is_none_or(|j| j.screen != screen) {
            return;
        }
        record.judged = None;
        let event = Event::new(
            EventKind::AgentJudgmentCleared {
                agent: agent.clone(),
                reason: reason.to_owned(),
            },
            Utc::now(),
        );
        let _ = self.transition(
            "screen judgment cleared",
            event,
            |store, event| store.agent_transition(&record, event),
            |state| {
                if let Some(held) = state.registry.get_mut(agent) {
                    *held = record.clone();
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;

    const KEY: &str = "ts-test-key-0123456789abcdef";

    /// What the fake System One server answers, and what it was sent.
    struct Fake {
        /// Status and the four probabilities to answer with.
        status: u16,
        nouls: [f64; 4],
        /// Hold every answer this long before writing it.
        stall: StdDuration,
        /// Hold every answer until this is set back to false.
        hold: bool,
        /// Each request: its authorization header and body.
        seen: Vec<(String, Value)>,
    }

    type Shared = Arc<StdMutex<Fake>>;

    fn server(nouls: [f64; 4]) -> (String, Shared) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        let shared: Shared = Arc::new(StdMutex::new(Fake {
            status: 200,
            nouls,
            stall: StdDuration::ZERO,
            hold: false,
            seen: Vec::new(),
        }));
        let fake = shared.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let fake = fake.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let (mut length, mut auth) = (0usize, String::new());
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).unwrap();
                        let header = header.trim_end();
                        if header.is_empty() {
                            break;
                        }
                        let (name, value) = header.split_once(':').unwrap();
                        match name.to_ascii_lowercase().as_str() {
                            "content-length" => length = value.trim().parse().unwrap(),
                            "authorization" => auth = value.trim().to_owned(),
                            _ => {}
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let body: Value = serde_json::from_slice(&body).unwrap();
                    let stall = {
                        let mut fake = fake.lock().unwrap();
                        fake.seen.push((auth, body));
                        fake.stall
                    };
                    std::thread::sleep(stall);
                    while fake.lock().unwrap().hold {
                        std::thread::sleep(StdDuration::from_millis(10));
                    }
                    let (status, nouls) = {
                        let fake = fake.lock().unwrap();
                        (fake.status, fake.nouls)
                    };
                    let reply = if status == 200 {
                        json!({
                            "model": "jev-1.13.0",
                            "answers": {
                                "approval": {"type": "noul", "noul": nouls[0]},
                                "question": {"type": "noul", "noul": nouls[1]},
                                "running": {"type": "noul", "noul": nouls[2]},
                                "draft": {"type": "noul", "noul": nouls[3]},
                            },
                            "usage": {"input_tokens": 600, "output_tokens": 20},
                        })
                        .to_string()
                    } else {
                        json!({"detail": "no"}).to_string()
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                });
            }
        });
        (url, shared)
    }

    fn calls(fake: &Shared) -> usize {
        fake.lock().unwrap().seen.len()
    }

    fn open(dir: &TempDir) -> Arc<Daemon> {
        let home = dir.path().to_path_buf();
        let daemon = Arc::new(Daemon::open(home.clone(), home.join("sock")).unwrap());
        lock(&daemon.judge).quiet = StdDuration::ZERO;
        daemon
    }

    fn write_key(dir: &TempDir, mode: u32) -> PathBuf {
        let file = dir.path().join("typesafe.key");
        std::fs::write(&file, format!("{KEY}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        file
    }

    fn configure(dir: &TempDir, url: &str, extra: &str) {
        let key = write_key(dir, 0o600);
        std::fs::write(
            dir.path().join(agentdocker_core::config::FILE_NAME),
            format!(
                "[judge]\njudgments = [\"screens\"]\nkey_file = {:?}\nurl = {url:?}\n{extra}",
                key.display().to_string()
            ),
        )
        .unwrap();
    }

    async fn eventually<T>(mut check: impl AsyncFnMut() -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + StdDuration::from_secs(10);
        loop {
            if let Some(value) = check().await {
                return value;
            }
            assert!(tokio::time::Instant::now() < deadline, "timed out");
            tokio::time::sleep(StdDuration::from_millis(50)).await;
        }
    }

    /// A managed agent with a terminal that prints `screen` and then sits
    /// on it, as an agent stopped at a prompt does.
    async fn showing(daemon: &Arc<Daemon>, dir: &TempDir, name: &str, screen: &str) -> AgentId {
        let spec = AgentSpec {
            name: name.into(),
            workdir: Some(dir.path().to_path_buf()),
            tty: true,
            command: vec![
                "sh".into(),
                "-c".into(),
                format!("printf '%b' {screen:?}; sleep 30"),
            ],
            ..AgentSpec::default()
        };
        let Response::Agent { agent } = daemon.handle(Request::Run { spec }).await else {
            panic!("managed launch failed");
        };
        let session = eventually(async || daemon.session(&agent.id)).await;
        // The screen is written with `\n` for each line break, for printf.
        let last = screen
            .split("\\n")
            .filter(|line| !line.trim().is_empty())
            .last()
            .unwrap_or_default()
            .trim()
            .to_owned();
        eventually(async || {
            let (bytes, _) = session.screen();
            String::from_utf8_lossy(&bytes)
                .contains(&last)
                .then_some(())
        })
        .await;
        agent.id
    }

    async fn activity(daemon: &Arc<Daemon>, agent: &AgentId) -> Activity {
        let Response::Activity { activity } = daemon
            .handle(Request::Activity {
                agent: Some(agent.to_string()),
                project: None,
                all: false,
            })
            .await
        else {
            panic!("no activity");
        };
        activity.into_iter().next().unwrap().activity
    }

    fn record(daemon: &Arc<Daemon>, agent: &AgentId) -> AgentRecord {
        lock(&daemon.state).registry.get(agent).cloned().unwrap()
    }

    async fn stop(daemon: &Arc<Daemon>, agent: &AgentId) {
        daemon
            .handle(Request::Stop {
                agent: agent.to_string(),
                force: true,
            })
            .await;
    }

    const DIALOG: &str =
        "Bash command\\n\\n  rm -rf build\\n\\nDo you want to proceed?\\n❯ 1. Yes\\n  2. No\\n";

    /// Off — no section, or a section that names no judgment — nothing is
    /// read and nothing is sent, whatever screens there are.
    #[tokio::test]
    async fn off_reads_no_screen_and_sends_nothing() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.99, 0.1, 0.0, 0.0]);
        let daemon = open(&dir);
        let agent = showing(&daemon, &dir, "quiet", DIALOG).await;
        daemon.judge_once().await;
        daemon.judge_once().await;
        assert_eq!(daemon.judge_status(), JudgeStatus::Off);
        let key = write_key(&dir, 0o600);
        std::fs::write(
            dir.path().join(agentdocker_core::config::FILE_NAME),
            format!(
                "[judge]\nkey_file = {:?}\nurl = {url:?}\n",
                key.display().to_string()
            ),
        )
        .unwrap();
        daemon.judge_once().await;
        daemon.judge_once().await;
        assert_eq!(daemon.judge_status(), JudgeStatus::Off);
        assert_eq!(lock(&daemon.judge).reads, 0, "no screen was read");
        assert_eq!(calls(&fake), 0, "nothing was sent");
        assert!(record(&daemon, &agent).judged.is_none());
        stop(&daemon, &agent).await;
    }

    /// The acceptance, end to end against a stand-in for the API: a quiet
    /// screen with an approval prompt is judged once, the agent is moved
    /// into Needs you with the reason and probability, the same screen is
    /// not judged again, and a change of screen takes the judgment off.
    #[tokio::test]
    async fn a_quiet_approval_prompt_is_judged_once_and_promotes() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.96, 0.31, 0.02, 0.01]);
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        let agent = showing(&daemon, &dir, "zebra-quokka", DIALOG).await;
        assert!(matches!(
            activity(&daemon, &agent).await,
            Activity::Working { .. } | Activity::Unknown
        ));

        daemon.judge_once().await;
        assert_eq!(calls(&fake), 1);
        let (auth, body) = fake.lock().unwrap().seen[0].clone();
        assert_eq!(
            auth,
            format!("Bearer {KEY}"),
            "the key goes as a bearer token"
        );
        assert_eq!(body["model"], "jev-1.13.0");
        let screen = body["state"]["screen"].as_str().unwrap();
        assert!(screen.contains("Do you want to proceed?"), "{screen}");
        assert!(screen.lines().count() <= judgment::SCREEN_LINES);
        assert!(
            !body.to_string().contains(agent.as_str())
                && !body.to_string().contains("zebra-quokka")
                && !body.to_string().contains(&dir.path().display().to_string()),
            "no id, name or path is sent"
        );

        assert_eq!(
            activity(&daemon, &agent).await,
            Activity::Waiting {
                since: record(&daemon, &agent).judged.unwrap().since,
                reason: Some(agentdocker_core::WaitReason::Permission),
                judged: Some(agentdocker_core::Judged { permille: 960 }),
                detail: None,
            }
        );
        let judged = record(&daemon, &agent).judged.unwrap();
        assert_eq!(judged.model, "jev-1.13.0");
        assert_eq!(judged.permille, 960);
        let promoted = eventually(async || {
            loop {
                match events.try_recv() {
                    Ok(Event {
                        kind: EventKind::AgentJudgedNeedsYou { agent: a, judgment },
                        ..
                    }) if a == agent => return Some(judgment),
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            }
        })
        .await;
        assert_eq!(promoted, judged);

        // The same quiet screen is not asked about again.
        daemon.judge_once().await;
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 1, "one call per quiet period");

        // The screen moves on: the judgment comes off, and the agent is
        // what its record says again.
        let session = daemon.session(&agent).unwrap();
        session.input.send(b"1\n".to_vec()).await.unwrap();
        eventually(async || {
            daemon.judge_once().await;
            record(&daemon, &agent).judged.is_none().then_some(())
        })
        .await;
        assert!(!matches!(
            activity(&daemon, &agent).await,
            Activity::Waiting { .. }
        ));
        stop(&daemon, &agent).await;
    }

    /// Work read as running, or an unsure answer, moves nothing; and a
    /// wait the agent's own adapter reported is never asked about.
    #[tokio::test]
    async fn running_work_and_unsure_answers_move_nothing() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.97, 0.9, 0.8, 0.0]);
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let agent = showing(
            &daemon,
            &dir,
            "busy",
            "Building… (12s · esc to interrupt)\\n",
        )
        .await;
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 1);
        assert!(
            record(&daemon, &agent).judged.is_none(),
            "running work vetoes"
        );

        fake.lock().unwrap().nouls = [0.6, 0.5, 0.1, 0.2];
        let unsure = showing(&daemon, &dir, "unsure", "Something happened.\\n> \\n").await;
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 2, "only the new quiet screen");
        assert!(
            record(&daemon, &unsure).judged.is_none(),
            "below the threshold"
        );

        // A wait its hooks reported is the agent's own word: not asked.
        let reported = showing(&daemon, &dir, "reported", DIALOG).await;
        // As the hooks send it, so fields added to an observation later
        // need nothing here.
        let observation: agentdocker_core::ActivityObservation = serde_json::from_value(json!({
            "activity": "waiting",
            "observed_at": Utc::now(),
            "reason": "permission",
        }))
        .unwrap();
        assert!(matches!(
            daemon
                .handle(Request::ReportActivity {
                    agent: reported.to_string(),
                    observation,
                })
                .await,
            Response::Ok
        ));
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 2, "a reported wait is not judged");
        for agent in [agent, unsure, reported] {
            stop(&daemon, &agent).await;
        }
    }

    /// The budget holds whatever goes quiet: with one call a minute, a
    /// second quiet screen waits for the next minute.
    #[tokio::test]
    async fn calls_stay_within_the_budget() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.1, 0.1, 0.1, 0.1]);
        configure(&dir, &url, "calls_per_minute = 1\n");
        let daemon = open(&dir);
        let first = showing(&daemon, &dir, "one", "first screen\\n").await;
        let second = showing(&daemon, &dir, "two", "second screen\\n").await;
        daemon.judge_once().await;
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 1);
        let JudgeStatus::On {
            calls_last_minute,
            calls_per_minute,
            ..
        } = daemon.judge_status()
        else {
            panic!("on");
        };
        assert_eq!((calls_last_minute, calls_per_minute), (1, 1));
        stop(&daemon, &first).await;
        stop(&daemon, &second).await;
    }

    /// A failing service is a status and, once a minute, an event; the
    /// daemon carries on, and neither names the address or the key.
    #[tokio::test]
    async fn a_failing_service_is_a_status_never_a_crash() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.99, 0.0, 0.0, 0.0]);
        fake.lock().unwrap().status = 503;
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        let agent = showing(&daemon, &dir, "waiting", DIALOG).await;
        daemon.judge_once().await;
        let JudgeStatus::On { problem, .. } = daemon.judge_status() else {
            panic!("on");
        };
        let problem = problem.unwrap();
        assert!(problem.contains("503"), "{problem}");
        assert!(!problem.contains(KEY) && !problem.contains("127.0.0.1"));
        let failed = eventually(async || {
            loop {
                match events.try_recv() {
                    Ok(Event {
                        kind: EventKind::JudgeFailed { reason, failures },
                        ..
                    }) => return Some((reason, failures)),
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            }
        })
        .await;
        assert_eq!(failed, ("unavailable".to_owned(), 1));
        // Backing off: no second call inside the minute.
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 1);
        assert!(record(&daemon, &agent).judged.is_none());

        // A stalled answer runs out of time instead of holding the pass.
        lock(&daemon.judge).backoff_until = None;
        lock(&daemon.judge).deadline = StdDuration::from_millis(300);
        {
            let mut fake = fake.lock().unwrap();
            fake.status = 200;
            fake.stall = StdDuration::from_secs(3);
        }
        let started = Instant::now();
        daemon.judge_once().await;
        assert!(started.elapsed() < StdDuration::from_secs(3));
        let JudgeStatus::On { problem, .. } = daemon.judge_status() else {
            panic!("on");
        };
        assert_eq!(problem.as_deref(), Some("timed out"));
        stop(&daemon, &agent).await;
    }

    /// Turning the judge off takes every standing judgment off with it.
    #[tokio::test]
    async fn turning_it_off_clears_what_it_judged() {
        let dir = TempDir::new().unwrap();
        let (url, _fake) = server([0.1, 0.92, 0.0, 0.0]);
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let agent = showing(&daemon, &dir, "asks", "Which branch should I use?\\n> \\n").await;
        daemon.judge_once().await;
        assert_eq!(
            record(&daemon, &agent).judged.map(|j| j.reason),
            Some(agentdocker_core::WaitReason::Question)
        );
        std::fs::remove_file(dir.path().join(agentdocker_core::config::FILE_NAME)).unwrap();
        daemon.judge_once().await;
        assert_eq!(daemon.judge_status(), JudgeStatus::Off);
        assert!(record(&daemon, &agent).judged.is_none());
        assert!(!matches!(
            activity(&daemon, &agent).await,
            Activity::Waiting { .. }
        ));
        stop(&daemon, &agent).await;
    }

    fn promotions(events: &mut tokio::sync::broadcast::Receiver<Event>) -> usize {
        let mut promoted = 0;
        while let Ok(event) = events.try_recv() {
            if matches!(event.kind, EventKind::AgentJudgedNeedsYou { .. }) {
                promoted += 1;
            }
        }
        promoted
    }

    /// The prompt is answered in the terminal while the answer is out, and
    /// nothing reports it: the screen has moved on, so the answer is dropped
    /// and the screen's quiet period starts over.
    #[tokio::test]
    async fn an_answer_to_a_screen_that_moved_on_is_dropped() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.98, 0.2, 0.01, 0.01]);
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        let agent = showing(&daemon, &dir, "answered", DIALOG).await;
        fake.lock().unwrap().hold = true;
        let pass = {
            let daemon = daemon.clone();
            tokio::spawn(async move { daemon.judge_once().await })
        };
        eventually(async || (calls(&fake) == 1).then_some(())).await;
        let session = daemon.session(&agent).unwrap();
        let before = session.screen().0.len();
        session.input.send(b"1\n".to_vec()).await.unwrap();
        eventually(async || (session.screen().0.len() > before).then_some(())).await;
        fake.lock().unwrap().hold = false;
        pass.await.unwrap();
        assert!(
            record(&daemon, &agent).judged.is_none(),
            "the answer is dropped"
        );
        assert_eq!(promotions(&mut events), 0);
        assert!(
            lock(&daemon.judge)
                .screens
                .get(&agent)
                .is_some_and(|watched| !watched.settled),
            "its quiet period starts over"
        );
        // Its new screen is asked about on its own, once it is quiet.
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 2);
        stop(&daemon, &agent).await;
    }

    /// Switched off while the first of two screens' answers is out: the
    /// second is never sent, and the first answer applies nothing.
    #[tokio::test]
    async fn switching_off_mid_pass_sends_no_more_and_applies_nothing() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.98, 0.95, 0.0, 0.0]);
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        let first = showing(&daemon, &dir, "first", DIALOG).await;
        let second = showing(
            &daemon,
            &dir,
            "second",
            "Which branch should I use?\\n> \\n",
        )
        .await;
        fake.lock().unwrap().hold = true;
        let pass = {
            let daemon = daemon.clone();
            tokio::spawn(async move { daemon.judge_once().await })
        };
        eventually(async || (calls(&fake) == 1).then_some(())).await;
        let key = dir.path().join("typesafe.key");
        std::fs::write(
            dir.path().join(agentdocker_core::config::FILE_NAME),
            format!(
                "[judge]\njudgments = []\nkey_file = {:?}\nurl = {url:?}\n",
                key.display().to_string()
            ),
        )
        .unwrap();
        // The tick's own read of the file, while the pass waits.
        let switched = tokio::time::timeout(StdDuration::from_secs(5), daemon.judge_setup())
            .await
            .expect("seen without waiting for the pass");
        assert!(switched.is_none());
        assert_eq!(daemon.judge_status(), JudgeStatus::Off);
        fake.lock().unwrap().hold = false;
        pass.await.unwrap();
        assert_eq!(calls(&fake), 1, "no second upload");
        assert!(record(&daemon, &first).judged.is_none());
        assert!(record(&daemon, &second).judged.is_none());
        assert_eq!(promotions(&mut events), 0);
        stop(&daemon, &first).await;
        stop(&daemon, &second).await;
    }

    fn statuses(
        events: &mut tokio::sync::broadcast::Receiver<Event>,
    ) -> Vec<(JudgeState, Option<String>)> {
        let mut said = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let EventKind::JudgeStatusChanged { status, reason } = event.kind {
                said.push((status, reason));
            }
        }
        said
    }

    /// How the judge stands is announced when it changes, and only then:
    /// on, off, unusable with why — never the key or the address — and
    /// nothing for a read that leaves it as it was, however many there are.
    #[tokio::test]
    async fn a_change_of_standing_is_announced_once() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.1, 0.1, 0.0, 0.0]);
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        let file = dir.path().join(agentdocker_core::config::FILE_NAME);

        daemon.judge_setup().await;
        daemon.judge_setup().await;
        assert!(
            statuses(&mut events).is_empty(),
            "off from the start says nothing"
        );

        configure(&dir, &url, "");
        daemon.judge_setup().await;
        daemon.judge_setup().await;
        assert_eq!(statuses(&mut events), [(JudgeState::On, None)]);

        configure(&dir, &url, "calls_per_minute = 2\n");
        daemon.judge_setup().await;
        assert!(statuses(&mut events).is_empty(), "another setup, still on");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let key = dir.path().join("typesafe.key");
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
            daemon.judge_setup().await;
            daemon.judge_setup().await;
            daemon.judge_setup().await;
            let said = statuses(&mut events);
            assert_eq!(said.len(), 1, "{said:?}");
            assert_eq!(said[0].0, JudgeState::Unusable);
            let reason = said[0].1.clone().unwrap();
            assert!(reason.contains("0600"), "{reason}");
            assert!(!reason.contains(KEY) && !reason.contains(&url), "{reason}");
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
            daemon.judge_setup().await;
            assert_eq!(statuses(&mut events), [(JudgeState::On, None)]);
        }

        std::fs::remove_file(&file).unwrap();
        for _ in 0..3 {
            daemon.judge_setup().await;
        }
        assert_eq!(
            statuses(&mut events),
            [(JudgeState::Off, None)],
            "a stop on every disabled read, one announcement"
        );
        assert_eq!(calls(&fake), 0);
    }

    /// Off for exactly one read, then the same section and key on again:
    /// a new generation with a setup of its own, and screens are judged.
    #[tokio::test]
    async fn the_same_setup_turned_off_once_and_on_again_judges_again() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.97, 0.1, 0.0, 0.0]);
        configure(&dir, &url, "");
        let file = dir.path().join(agentdocker_core::config::FILE_NAME);
        let on = std::fs::read_to_string(&file).unwrap();
        let daemon = open(&dir);
        let first = daemon.judge_setup().await.expect("on");
        std::fs::remove_file(&file).unwrap();
        assert!(daemon.judge_setup().await.is_none(), "off for one read");
        {
            let judge = lock(&daemon.judge);
            assert!(judge.running.is_none() && judge.fingerprint.is_none());
        }
        std::fs::write(&file, on).unwrap();
        let again = daemon.judge_setup().await.expect("on again");
        assert!(again.generation > first.generation, "a fresh generation");
        assert!(lock(&daemon.judge).running.is_some());
        let agent = showing(&daemon, &dir, "again", DIALOG).await;
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 1, "a new screen is judged");
        assert!(record(&daemon, &agent).judged.is_some());
        stop(&daemon, &agent).await;
    }

    /// A positive answer held after its generation check and before its
    /// record, while the judge is switched off and every standing judgment
    /// cleared: once let go it records nothing and announces nothing.
    #[tokio::test]
    async fn a_switch_off_between_the_check_and_the_record_wins() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.98, 0.1, 0.0, 0.0]);
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let mut events = daemon.subscribe_events();
        let agent = showing(&daemon, &dir, "held", DIALOG).await;
        let (reached, resume) = (
            Arc::new(tokio::sync::Barrier::new(2)),
            Arc::new(tokio::sync::Barrier::new(2)),
        );
        lock(&daemon.judge).pause = Some((reached.clone(), resume.clone()));
        let pass = {
            let daemon = daemon.clone();
            tokio::spawn(async move { daemon.judge_once().await })
        };
        reached.wait().await;
        assert_eq!(calls(&fake), 1, "the answer is back and checked");
        std::fs::remove_file(dir.path().join(agentdocker_core::config::FILE_NAME)).unwrap();
        assert!(
            daemon.judge_setup().await.is_none(),
            "switched off and cleared"
        );
        resume.wait().await;
        pass.await.unwrap();
        assert!(
            record(&daemon, &agent).judged.is_none(),
            "nothing persisted"
        );
        assert_eq!(promotions(&mut events), 0, "nothing announced");
        assert_eq!(daemon.judge_status(), JudgeStatus::Off);
        stop(&daemon, &agent).await;
    }

    /// A tmux that never answers is given up on at the capture's deadline:
    /// a managed terminal is still judged in the same pass, and switching
    /// the judge off is seen while a capture hangs.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hanging_tmux_holds_neither_a_terminal_nor_the_switch_off() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.98, 0.2, 0.0, 0.0]);
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let marker = dir.path().join("hung");
        let stand_in = dir.path().join("tmux");
        std::fs::write(
            &stand_in,
            format!("#!/bin/sh\necho $$ > {}\nexec sleep 30\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
        {
            let mut judge = lock(&daemon.judge);
            judge.tmux = stand_in;
            judge.capture_deadline = StdDuration::from_millis(500);
        }
        let mut guest = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let Response::Agent { agent: in_tmux } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "in-tmux".into(),
                    workdir: Some(dir.path().to_path_buf()),
                    ..AgentSpec::default()
                },
                pid: Some(guest.id()),
                session: Some(agentdocker_core::multiplexer::Session {
                    kind: "tmux".into(),
                    session: Some("0".into()),
                    pane: Some("%0".into()),
                    socket: Some(dir.path().join("server").display().to_string()),
                    evidence: Evidence::Environment,
                }),
            })
            .await
        else {
            panic!("registered");
        };
        let managed = showing(&daemon, &dir, "managed", DIALOG).await;

        let started = Instant::now();
        daemon.judge_once().await;
        assert!(started.elapsed() < StdDuration::from_secs(5));
        assert!(
            record(&daemon, &managed).judged.is_some(),
            "the terminal was judged"
        );
        assert_eq!(calls(&fake), 1, "nothing from the pane was sent");
        let hung: u32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            !agentdocker_host::procinfo::alive(hung),
            "killed and reaped"
        );

        std::fs::remove_file(&marker).unwrap();
        let pass = {
            let daemon = daemon.clone();
            tokio::spawn(async move { daemon.judge_once().await })
        };
        eventually(async || marker.exists().then_some(())).await;
        std::fs::remove_file(dir.path().join(agentdocker_core::config::FILE_NAME)).unwrap();
        let switched = tokio::time::timeout(StdDuration::from_secs(2), daemon.judge_setup())
            .await
            .expect("the switch-off does not wait on the capture");
        assert!(switched.is_none());
        assert_eq!(daemon.judge_status(), JudgeStatus::Off);
        assert!(
            record(&daemon, &managed).judged.is_none(),
            "taken off with it"
        );
        pass.await.unwrap();
        assert_eq!(calls(&fake), 1);
        let _ = guest.kill();
        let _ = guest.wait();
        stop(&daemon, &managed).await;
        let _ = in_tmux;
    }

    /// tmux servers of the test's own, each on its own socket, ended
    /// however the test ends.
    #[cfg(unix)]
    struct Servers {
        sockets: Vec<PathBuf>,
        pids: Vec<u32>,
    }

    #[cfg(unix)]
    impl Servers {
        fn start(&mut self, socket: &Path, text: &str) {
            let status = std::process::Command::new("tmux")
                .args(["-f", "/dev/null", "-S"])
                .arg(socket)
                .args(["new-session", "-d", "-x", "80", "-y", "24"])
                .arg(format!("printf '{text}\\n'; trap '' HUP; exec sleep 120"))
                .status()
                .unwrap();
            assert!(status.success());
            self.sockets.push(socket.to_owned());
            // A pane that ignores hangups outlives its server; it is ended
            // with the rest.
            self.pids.push(Self::pane_pid(socket));
        }

        fn ask(socket: &Path, args: &[&str]) -> String {
            let output = std::process::Command::new("tmux")
                .arg("-S")
                .arg(socket)
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }

        fn pane_pid(socket: &Path) -> u32 {
            Self::ask(
                socket,
                &["display-message", "-p", "-t", "%0", "#{pane_pid}"],
            )
            .parse()
            .unwrap()
        }

        fn shows(socket: &Path, text: &str) -> bool {
            Self::ask(socket, &["capture-pane", "-p", "-t", "%0"]).contains(text)
        }
    }

    #[cfg(unix)]
    impl Drop for Servers {
        fn drop(&mut self) {
            for socket in &self.sockets {
                let _ = std::process::Command::new("tmux")
                    .arg("-S")
                    .arg(socket)
                    .arg("kill-server")
                    .output();
            }
            for pid in &self.pids {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .output();
            }
        }
    }

    /// Two tmux servers each have a `%0`. Only the screen of the agent's
    /// own server and pane is sent; once the pane or the server is
    /// replaced, the pane no longer holds the agent's process and nothing
    /// of it is sent. The test's servers are its own, on sockets of its
    /// own; nothing here touches any other server.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tmux_pane_is_read_only_at_the_agents_server_while_it_holds_the_agent() {
        if !multiplexer::tmux::available() {
            eprintln!("tmux is not installed here; skipping");
            return;
        }
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.1, 0.1, 0.0, 0.0]);
        configure(&dir, &url, "");
        let daemon = open(&dir);
        let mut servers = Servers {
            sockets: Vec::new(),
            pids: Vec::new(),
        };
        let other = dir.path().join("o");
        let own = dir.path().join("a");
        servers.start(&other, "SENTINEL_OTHER_SERVER");
        servers.start(&own, "SENTINEL_AGENTS_SERVER");
        for socket in [&other, &own] {
            assert_eq!(
                Servers::ask(socket, &["display-message", "-p", "-t", "%0", "#{pane_id}"]),
                "%0"
            );
        }
        eventually(async || Servers::shows(&own, "SENTINEL_AGENTS_SERVER").then_some(())).await;
        eventually(async || Servers::shows(&other, "SENTINEL_OTHER_SERVER").then_some(())).await;
        let pid = Servers::pane_pid(&own);
        let Response::Agent { agent } = daemon
            .handle(Request::Register {
                spec: AgentSpec {
                    name: "paned".into(),
                    workdir: Some(dir.path().to_path_buf()),
                    ..AgentSpec::default()
                },
                pid: Some(pid),
                session: Some(agentdocker_core::multiplexer::Session {
                    kind: "tmux".into(),
                    session: Some("0".into()),
                    pane: Some("%0".into()),
                    socket: Some(own.display().to_string()),
                    evidence: Evidence::Environment,
                }),
            })
            .await
        else {
            panic!("registered");
        };
        let sent = |fake: &Shared| -> String {
            fake.lock()
                .unwrap()
                .seen
                .iter()
                .map(|(_, body)| body["state"]["screen"].as_str().unwrap_or("").to_owned())
                .collect::<Vec<_>>()
                .join("\n")
        };
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 1);
        let screens = sent(&fake);
        assert!(screens.contains("SENTINEL_AGENTS_SERVER"), "{screens}");
        assert!(!screens.contains("SENTINEL_OTHER_SERVER"), "{screens}");

        // The pane is given another process under the same id; the agent
        // lives on outside it.
        assert!(
            std::process::Command::new("tmux")
                .arg("-S")
                .arg(&own)
                .args(["respawn-pane", "-k", "-t", "%0"])
                .arg("printf 'SENTINEL_REPLACED_PANE\\n'; exec sleep 120")
                .status()
                .unwrap()
                .success()
        );
        eventually(async || Servers::shows(&own, "SENTINEL_REPLACED_PANE").then_some(())).await;
        assert_ne!(Servers::pane_pid(&own), pid);
        daemon.judge_once().await;
        daemon.judge_once().await;
        assert_eq!(calls(&fake), 1, "nothing of a pane that lost the agent");

        // The server is replaced by another on the same socket, with a %0
        // of its own.
        Servers::ask(&own, &["kill-server"]);
        servers.start(&own, "SENTINEL_NEW_SERVER");
        eventually(async || Servers::shows(&own, "SENTINEL_NEW_SERVER").then_some(())).await;
        daemon.judge_once().await;
        daemon.judge_once().await;
        assert_eq!(
            calls(&fake),
            1,
            "nothing of a server that is not the agent's"
        );
        let screens = sent(&fake);
        assert!(!screens.contains("SENTINEL_REPLACED_PANE"));
        assert!(!screens.contains("SENTINEL_NEW_SERVER"));
        let _ = agent;
    }

    /// A key file anyone else can read is refused, as is a relative path
    /// or an address that is neither https nor this machine; the judge
    /// says why and sends nothing.
    #[tokio::test]
    async fn an_unsafe_key_or_address_is_refused() {
        let dir = TempDir::new().unwrap();
        let (url, fake) = server([0.99, 0.0, 0.0, 0.0]);
        let daemon = open(&dir);
        let agent = showing(&daemon, &dir, "kept", DIALOG).await;
        #[cfg(unix)]
        for mode in [0o640, 0o604, 0o644] {
            let key = write_key(&dir, mode);
            std::fs::write(
                dir.path().join(agentdocker_core::config::FILE_NAME),
                format!(
                    "[judge]\njudgments = [\"screens\"]\nkey_file = {:?}\nurl = {url:?}\n",
                    key.display().to_string()
                ),
            )
            .unwrap();
            daemon.judge_once().await;
            let JudgeStatus::Unusable { reason } = daemon.judge_status() else {
                panic!("a key readable by others is refused ({mode:o})");
            };
            assert!(reason.contains("0600"), "{reason}");
            assert!(!reason.contains(KEY));
        }
        for (key_file, address) in [
            ("typesafe.key".to_owned(), url.clone()),
            (
                write_key(&dir, 0o600).display().to_string(),
                "http://api.typesafe.ai/v1/systemone".to_owned(),
            ),
        ] {
            std::fs::write(
                dir.path().join(agentdocker_core::config::FILE_NAME),
                format!(
                    "[judge]\njudgments = [\"screens\"]\nkey_file = {key_file:?}\nurl = {address:?}\n"
                ),
            )
            .unwrap();
            daemon.judge_once().await;
            assert!(
                matches!(daemon.judge_status(), JudgeStatus::Unusable { .. }),
                "{key_file} {address}"
            );
        }
        assert_eq!(calls(&fake), 0, "nothing was sent");
        assert_eq!(lock(&daemon.judge).reads, 0, "no screen was read");
        stop(&daemon, &agent).await;
    }

    /// A pane is read only when tmux named it to the agent itself, with
    /// the server it is on.
    #[test]
    fn only_a_pane_tmux_named_with_its_server_is_read() {
        let mut record = AgentRecord::new(AgentSpec::default(), false, Utc::now());
        assert_eq!(tmux_pane(&record), None);
        let session =
            |kind: &str, socket: Option<&str>, evidence| agentdocker_core::multiplexer::Session {
                kind: kind.into(),
                session: Some("0".into()),
                pane: Some("%3".into()),
                socket: socket.map(str::to_owned),
                evidence,
            };
        let server = "/private/tmp/tmux-501/default";
        let socket = Some(server);
        record.session = Some(session("tmux", socket, Evidence::Environment));
        assert_eq!(
            tmux_pane(&record),
            Some((PathBuf::from(server), "%3".to_owned()))
        );
        record.session = Some(session("tmux", None, Evidence::Environment));
        assert_eq!(tmux_pane(&record), None, "a pane id alone is any server's");
        record.session = Some(session("tmux", Some("default"), Evidence::Environment));
        assert_eq!(tmux_pane(&record), None, "a server is an absolute socket");
        record.session = Some(session("tmux", socket, Evidence::Ancestry));
        assert_eq!(tmux_pane(&record), None, "ancestry cannot name a pane");
        record.session = Some(session("herdr", socket, Evidence::Environment));
        assert_eq!(tmux_pane(&record), None, "herdr panes are not read");
    }

    /// The agent must be the pane's process or descend from it.
    #[test]
    fn an_agent_belongs_to_a_pane_only_by_descent() {
        let parents: HashMap<u32, u32> =
            [(500, 400), (400, 300), (300, 1), (600, 1), (1, 0)].into();
        assert!(descends(&parents, 500, 500), "the pane's own process");
        assert!(descends(&parents, 500, 300), "a grandchild");
        assert!(!descends(&parents, 600, 300), "a stranger");
        assert!(!descends(&parents, 300, 500), "an ancestor is not a child");
        assert!(
            !descends(&parents, 700, 700 - 1),
            "a process not in the table"
        );
        let cycle: HashMap<u32, u32> = [(10, 11), (11, 10)].into();
        assert!(!descends(&cycle, 10, 99), "a loop ends");
    }

    /// A window as large as a client can ask for is drawn at a bounded
    /// size: the emulator never allocates billions of cells.
    #[tokio::test]
    async fn an_oversized_window_is_drawn_at_a_bounded_size() {
        let dir = TempDir::new().unwrap();
        let daemon = open(&dir);
        let agent = showing(&daemon, &dir, "huge", "Do you want to proceed?\\n").await;
        let session = daemon.session(&agent).unwrap();
        session.resize(u16::MAX, u16::MAX).unwrap();
        let (bytes, size) = session.screen();
        assert_eq!(size, (supervisor::SCREEN_COLS, supervisor::SCREEN_ROWS));
        let started = Instant::now();
        let drawn = judgment::screen_tail(&draw(&bytes, (u16::MAX, u16::MAX)));
        assert!(
            drawn
                .iter()
                .any(|line| line.contains("Do you want to proceed?"))
        );
        assert!(started.elapsed() < StdDuration::from_secs(2));
        session.resize(0, 0).unwrap();
        assert_eq!(
            session.screen().1,
            (supervisor::SCREEN_COLS, supervisor::SCREEN_ROWS),
            "a zero size is not a size"
        );
        stop(&daemon, &agent).await;
    }

    /// A managed agent's screen is what a terminal would show, escapes
    /// drawn, not the bytes it printed.
    #[test]
    fn a_terminal_is_drawn_as_the_agent_drew_it() {
        let printed = b"old line\r\n\x1b[2J\x1b[H\x1b[1mDo you want to proceed?\x1b[0m\r\n\x1b[32m> 1. Yes\x1b[0m\r\n";
        let screen = judgment::screen_tail(&draw(printed, (80, 24)));
        assert_eq!(screen, ["Do you want to proceed?", "> 1. Yes"]);
        assert_eq!(digest(&screen).len(), 16);
        assert_ne!(digest(&screen), digest(&screen[..1]));
    }
}
