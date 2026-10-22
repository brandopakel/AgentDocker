//! The opt-in judge, `[judge]` in `agentd.toml`: an agent's quiet screen
//! read by a TypeSafe System One model (Jev) for whether it waits on the
//! person. Off unless the person names a judgment and a key; then, and
//! only then, screens are read and their last lines sent.
//!
//! Every five seconds a pass reads the file and the key, then the screens
//! of live agents AgentDocker already holds or is bound to: the terminal
//! of a managed agent (its scrollback drawn as the agent drew it), and the
//! tmux pane an agent registered from. A screen whose last lines have not
//! changed for [`judgment::QUIET_SECS`] is quiet; a quiet screen is asked
//! about once, and not again until it changes. Calls are budgeted per
//! minute, made one at a time under a deadline, with no redirects; a
//! failure becomes the judge's status and, at most once a minute, a
//! `judge_failed` event, and never stops the daemon. What an answer may
//! do is core's rule ([`judgment::decide`], [`judgment::raise`]): promote
//! to waiting for the person, never anything else. A promotion is kept on
//! the agent's record until its screen changes, another process is behind
//! it, or the judge is turned off.
//!
//! The key leaves the process only in the `Authorization` header; it is
//! never logged, and no status or event names it or the address.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::time::{Duration as StdDuration, Instant};

use super::*;
use agentdocker_core::Activity;
use agentdocker_core::config::JudgeConfig;
use agentdocker_core::judgment::{self, JudgeStatus, Outcome, ScreenJudgment, Thresholds};
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

/// Where an agent's screen is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    /// The terminal the daemon holds for a managed agent.
    Terminal,
    /// The tmux pane, by id, that the agent registered from.
    Tmux(String),
}

/// The pane an agent registered from, when tmux told it so itself: a
/// pane seen only in its ancestry cannot be named, and is not read.
fn tmux_pane(record: &AgentRecord) -> Option<String> {
    record
        .session
        .as_ref()
        .filter(|s| s.kind == "tmux" && s.evidence == Evidence::Environment)
        .and_then(|s| s.pane.clone())
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

/// The judge's own state, off the state lock.
pub(in crate::daemon) struct Judge {
    setting: Setting,
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
    /// How long a screen must be unchanged, and how long a call may take;
    /// constants, shortened by tests.
    quiet: StdDuration,
    deadline: StdDuration,
    /// Screens read, for tests that prove none is read while off.
    reads: u64,
}

impl Default for Judge {
    fn default() -> Self {
        Self {
            setting: Setting::Off,
            screens: HashMap::new(),
            calls: VecDeque::new(),
            backoff_until: None,
            problem: None,
            unannounced: 0,
            last_reason: "",
            announced: None,
            quiet: StdDuration::from_secs(judgment::QUIET_SECS),
            deadline: CALL_DEADLINE,
            reads: 0,
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

    /// Forget everything watched: nothing is kept while off.
    fn stop(&mut self, setting: Setting) {
        self.setting = setting;
        self.screens.clear();
        self.calls.clear();
        self.backoff_until = None;
        self.problem = None;
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
/// it printed into.
fn draw(bytes: &[u8], (cols, rows): (u16, u16)) -> String {
    let mut parser = vt100::Parser::new(rows.max(1), cols.max(1), 0);
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
    process: Option<DateTime<Utc>>,
    source: Source,
    /// Working, idle or unknown by its record, before any judgment.
    eligible: bool,
    standing: Option<ScreenJudgment>,
}

/// A quiet screen to ask about.
struct Due {
    agent: AgentId,
    process: Option<DateTime<Utc>>,
    lines: Vec<String>,
    digest: String,
    since: DateTime<Utc>,
    read_at: DateTime<Utc>,
}

impl Daemon {
    /// Start a judge pass unless one is under way.
    pub fn judge_screens(self: &Arc<Self>) {
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

    /// One pass: the file and the key, then every screen there is to read.
    pub(in crate::daemon) async fn judge_once(self: &Arc<Self>) {
        let home = self.home.clone();
        let setup = tokio::task::spawn_blocking(
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
        let (config, key) = match setup {
            Ok(Some(found)) => found,
            Ok(None) => return self.judge_stop(Setting::Off),
            Err(reason) => {
                self.config_notice(reason.clone());
                return self.judge_stop(Setting::Unusable(reason));
            }
        };
        let announced = {
            let mut judge = lock(&self.judge);
            judge.setting = Setting::On {
                judgments: config.judgments.clone(),
                model: config.model.clone(),
                calls_per_minute: config.calls_per_minute,
            };
            judge.announce(Instant::now())
        };
        if let Some(event) = announced {
            self.emit(event);
        }
        let due = self.read_screens().await;
        self.ask_judge(&config, &key, due).await;
    }

    /// Read every screen there is to read, keep track of which have gone
    /// quiet, take off judgments whose screen moved on, and say which
    /// quiet screens are due to be asked about.
    async fn read_screens(self: &Arc<Self>) -> Vec<Due> {
        let terminals: HashMap<AgentId, supervisor::Session> = lock(&self.sessions).clone();
        let now = Utc::now();
        let candidates: Vec<Candidate> = {
            let state = lock(&self.state);
            state
                .registry
                .all()
                .filter(|record| record.status.is_live())
                .filter_map(|record| {
                    let source = if terminals.contains_key(&record.id) {
                        Source::Terminal
                    } else {
                        Source::Tmux(tmux_pane(record)?)
                    };
                    Some(Candidate {
                        agent: record.id.clone(),
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
        lock(&self.judge)
            .screens
            .retain(|agent, _| candidates.iter().any(|c| &c.agent == agent));
        let mut due = Vec::new();
        for candidate in candidates {
            let text = match &candidate.source {
                Source::Terminal => match terminals.get(&candidate.agent) {
                    Some(session) => {
                        let (bytes, size) = session.screen();
                        draw(&bytes, size)
                    }
                    None => continue,
                },
                Source::Tmux(pane) => {
                    let pane = pane.clone();
                    match tokio::task::spawn_blocking(move || multiplexer::tmux::capture(&pane))
                        .await
                    {
                        Ok(Ok(text)) => text,
                        _ => continue,
                    }
                }
            };
            let read_at = Utc::now();
            let lines = judgment::screen_tail(&text);
            let screen = digest(&lines);
            let (quiet_for, since) = {
                let mut judge = lock(&self.judge);
                judge.reads += 1;
                let quiet = judge.quiet;
                let instant = Instant::now();
                let watched = judge
                    .screens
                    .entry(candidate.agent.clone())
                    .or_insert_with(|| Watched {
                        process: candidate.process,
                        digest: screen.clone(),
                        changed: instant,
                        changed_at: read_at,
                        settled: false,
                    });
                if watched.digest != screen || watched.process != candidate.process {
                    *watched = Watched {
                        process: candidate.process,
                        digest: screen.clone(),
                        changed: instant,
                        changed_at: read_at,
                        settled: false,
                    };
                }
                let quiet_now =
                    !watched.settled && instant.duration_since(watched.changed) >= quiet;
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
                    process: candidate.process,
                    lines,
                    digest: screen,
                    since,
                    read_at,
                });
            }
        }
        // The screen quiet longest is asked about first.
        due.sort_by_key(|d| d.since);
        due
    }

    /// Ask about each due screen, one call at a time, within the budget.
    async fn ask_judge(self: &Arc<Self>, config: &JudgeConfig, key: &str, due: Vec<Due>) {
        let thresholds = Thresholds::percent(config.threshold_percent);
        for screen in due {
            let deadline = {
                let mut judge = lock(&self.judge);
                let now = Instant::now();
                judge.prune(now);
                if judge.backoff_until.is_some_and(|until| now < until)
                    || judge.calls.len() >= config.calls_per_minute as usize
                {
                    break;
                }
                judge.calls.push_back(now);
                judge.deadline
            };
            let body = judgment::screen_request(&config.model, &screen.lines);
            let (url, key) = (config.url.clone(), key.to_owned());
            let answered = tokio::task::spawn_blocking(move || call(&url, &key, &body, deadline))
                .await
                .unwrap_or(Err(Failure::Unreachable))
                .and_then(|value| judgment::read_answers(&value).map_err(|_| Failure::Malformed));
            match answered {
                Ok((model, answers)) => {
                    self.settle(&screen.agent, &screen.digest);
                    lock(&self.judge).problem = None;
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
                        self.record_judgment(&screen.agent, judged);
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

    /// A screen asked about is not asked about again until it changes.
    fn settle(&self, agent: &AgentId, screen: &str) {
        if let Some(watched) = lock(&self.judge).screens.get_mut(agent)
            && watched.digest == screen
        {
            watched.settled = true;
        }
    }

    /// Keep a promotion on the agent's record, if it still applies: the
    /// same live process, nothing reported by its adapter since the screen
    /// was read, and working, idle or unknown by its record.
    fn record_judgment(&self, agent: &AgentId, judgment: ScreenJudgment) -> bool {
        let mut state = lock(&self.state);
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
        let mut state = lock(&self.state);
        let Some(mut record) = state.registry.get(agent).cloned() else {
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
        let _ = state.transition(
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

    /// Off, or unable to run: forget every screen, read none, and take
    /// every standing judgment off, since nothing watches it any more.
    fn judge_stop(&self, setting: Setting) {
        lock(&self.judge).stop(setting);
        let standing: Vec<(AgentId, String)> = lock(&self.state)
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
            self.clear_judgment(&agent, &screen, "judge_off");
        }
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
                    let (status, nouls, stall) = {
                        let mut fake = fake.lock().unwrap();
                        fake.seen.push((auth, body));
                        (fake.status, fake.nouls, fake.stall)
                    };
                    std::thread::sleep(stall);
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
        let observation = agentdocker_core::ActivityObservation {
            activity: agentdocker_core::ReportedActivity::Waiting,
            observed_at: Utc::now(),
            reason: Some(agentdocker_core::WaitReason::Permission),
        };
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

    /// A pane is read only when tmux named it to the agent itself.
    #[test]
    fn only_a_pane_tmux_named_is_read() {
        let mut record = AgentRecord::new(AgentSpec::default(), false, Utc::now());
        assert_eq!(tmux_pane(&record), None);
        let session = |kind: &str, evidence| agentdocker_core::multiplexer::Session {
            kind: kind.into(),
            session: Some("0".into()),
            pane: Some("%3".into()),
            evidence,
        };
        record.session = Some(session("tmux", Evidence::Environment));
        assert_eq!(tmux_pane(&record).as_deref(), Some("%3"));
        record.session = Some(session("tmux", Evidence::Ancestry));
        assert_eq!(tmux_pane(&record), None, "ancestry cannot name a pane");
        record.session = Some(session("herdr", Evidence::Environment));
        assert_eq!(tmux_pane(&record), None, "herdr panes are not read");
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
