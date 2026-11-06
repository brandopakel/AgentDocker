//! The daemon's own configuration: `agentd.toml` in its home.
//!
//! This is distinct from admission policy (`policy.toml`), which decides what
//! agents may do. `agentd.toml` decides how the daemon keeps house for itself.
//! Reading the file is host work; what it means is decided here.

use crate::event::EventKind;
use chrono::Duration;
use serde::{Deserialize, Serialize};

/// The file's name inside the daemon's home.
pub const FILE_NAME: &str = "agentd.toml";

/// How many journal rows one retention tick deletes per project, so the
/// once-a-minute tick stays short however far behind retention has fallen.
pub const RETENTION_BATCH: usize = 1_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default)]
    pub usage: UsageConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub journal: JournalConfig,
    #[serde(default)]
    pub messages: MessagesConfig,
    /// How many observations one agent's read set holds.
    #[serde(default)]
    pub read_set: ReadSetConfig,
    /// Where copies of chosen events are posted, signed. Nothing is
    /// posted anywhere unless the person writes a sink here.
    #[serde(default)]
    pub webhooks: Vec<WebhookConfig>,
    /// What becomes of a project's memory beyond this machine: nothing,
    /// unless the person turns a mirror on here.
    #[serde(default)]
    pub memory: MemoryConfig,
    /// What is in source and complete but still under acceptance on real
    /// binaries, switched here; `daemon status` says how each stands.
    #[serde(default)]
    pub experimental: ExperimentalConfig,
    /// The opt-in judge: narrow questions put to a TypeSafe model about
    /// text the daemon holds. Nothing is read for it or sent anywhere
    /// unless the person names judgments here.
    #[serde(default)]
    pub judge: JudgeConfig,
}

/// The experimental switches. Each is off until the file turns it on,
/// except the receiver for an existing Codex session, which every such
/// session has had since it shipped and which `native_codex = false`
/// turns off. The daemon reports the switches as it applies them, so a
/// bridge or hook follows the daemon's word rather than its own
/// environment; the environment variables each switch had before the
/// file still turn it on, for one process, so an installation that set
/// them keeps working.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentalConfig {
    /// `daemon reload`: the live replacement of the serving daemon by the
    /// activated release, with every agent kept running.
    #[serde(default)]
    pub reload: bool,
    /// The managed Codex bridge's secret-input broker, which answers a
    /// provider's request for a secret through the person without the
    /// value passing through a message or a ledger.
    #[serde(default)]
    pub secret_input: bool,
    /// The receiver that feeds an existing Codex terminal session (one
    /// AgentDocker did not start) through Codex's native queue. Off, such
    /// a session gets its messages at tool boundaries through its hooks,
    /// as an existing Claude session without the channel does.
    #[serde(default = "on")]
    pub native_codex: bool,
}

fn on() -> bool {
    true
}

impl Default for ExperimentalConfig {
    fn default() -> Self {
        Self {
            reload: false,
            secret_input: false,
            native_codex: true,
        }
    }
}

impl ExperimentalConfig {
    /// The environment variable that turned `reload` on before the file
    /// existed; `1` still does, for the process that carries it.
    pub const RELOAD_ENV: &str = "AGENTDOCKER_EXPERIMENTAL_RELOAD";
    /// Likewise for `secret_input`.
    pub const SECRET_INPUT_ENV: &str = "AGENTDOCKER_EXPERIMENTAL_SECRET_INPUT";

    /// These switches with one process's environment on top: a variable
    /// set to exactly `1` turns its switch on, as it has since the switch
    /// existed. The file is where a switch is meant to live; the
    /// environment cannot turn one off.
    pub fn with_env(mut self, var: impl Fn(&str) -> Option<String>) -> Self {
        let on = |name: &str| var(name).as_deref() == Some("1");
        self.reload |= on(Self::RELOAD_ENV);
        self.secret_input |= on(Self::SECRET_INPUT_ENV);
        self
    }

    /// One line for a person: which switches are on.
    pub fn summary(&self) -> String {
        let word = |on: bool| if on { "on" } else { "off" };
        format!(
            "reload {}, secret input {}, native codex {}",
            word(self.reload),
            word(self.secret_input),
            word(self.native_codex)
        )
    }
}

/// How many finished agents one retention tick forgets, so the
/// once-a-minute tick stays short however many have piled up.
pub const AGENT_RETENTION_BATCH: usize = 100;

/// The records of agents that have ended.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    /// How long the record of an agent that has ended is kept, as `30m`,
    /// `12h`, `180d` or plain seconds, or `off` to keep every record until
    /// `rm`. The default is 30 days: every record is loaded at startup and
    /// looked at on every tick, and one that old holds nothing and answers
    /// nothing; the journal and the ledger keep its id regardless.
    #[serde(default = "agents_retention")]
    pub retention: String,
}

fn agents_retention() -> String {
    "30d".to_owned()
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            retention: agents_retention(),
        }
    }
}

/// Local accounting roots only; transcript text is parsed but never retained.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Empty uses the provider's ordinary local sessions/projects directories.
    #[serde(default)]
    pub codex_roots: Vec<std::path::PathBuf>,
    #[serde(default)]
    pub claude_roots: Vec<std::path::PathBuf>,
    #[serde(default = "usage_retention_days")]
    pub retention_days: u32,
}

fn usage_retention_days() -> u32 {
    30
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            codex_roots: Vec::new(),
            claude_roots: Vec::new(),
            retention_days: 30,
        }
    }
}

impl UsageConfig {
    pub fn check(&self) -> Result<(), String> {
        if !(1..=3650).contains(&self.retention_days) {
            return Err("usage.retention_days must be 1–3650".into());
        }
        if self.codex_roots.len() + self.claude_roots.len() > 16
            || self
                .codex_roots
                .iter()
                .chain(&self.claude_roots)
                .any(|path| !path.is_absolute() || path.to_str().is_none_or(|s| s.len() > 8192))
        {
            return Err(
                "usage roots must be at most sixteen absolute UTF-8 paths of at most 8192 bytes"
                    .into(),
            );
        }
        Ok(())
    }
}

/// At most this many sinks; each carries its own bounded queue and its
/// own connection, so the count is a bound on the daemon's outbound work.
pub const WEBHOOKS: usize = 8;
/// A sink listens to at most this many event kinds.
pub const WEBHOOK_EVENTS: usize = 64;

/// One sink: an `https` address (plain `http` only on this machine), a
/// file holding the shared secret, the event kinds to post, and the
/// shape of the body.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    /// How the sink is named in logs and events, so its address never is.
    pub name: String,
    pub url: String,
    /// A private file (a regular file of this user, mode 0600) whose
    /// trimmed content is the HMAC secret.
    pub secret_file: std::path::PathBuf,
    /// Event kinds to post, by their wire names (`question_opened`,
    /// `agent_exited`, `lease_deadlock`…, [`EventKind::WIRE_NAMES`]).
    /// Empty posts nothing; a name that is no kind is refused.
    #[serde(default)]
    pub events: Vec<String>,
    /// Only events of this project (id, root or unique prefix), when set.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub format: WebhookFormat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookFormat {
    /// The projected event as JSON.
    #[default]
    Json,
    /// `{"text": "…"}`, one line, for Slack-shaped receivers.
    Slack,
}

impl WebhookConfig {
    /// The shape a sink must have: a name that can be said, an address
    /// that goes nowhere surprising, kinds that can be kinds. The address
    /// keeps no credentials in it and no fragment, and plain `http` is
    /// allowed only to this machine.
    pub fn check(&self) -> Result<(), String> {
        let name = &self.name;
        if name.is_empty()
            || name.len() > 40
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("a webhook name is 1–40 lowercase letters, digits or hyphens".into());
        }
        check_outbound(&self.url, "put a secret in secret_file")
            .map_err(|error| format!("webhook {name}: {error}"))?;
        if self.secret_file.as_os_str().is_empty() || !self.secret_file.is_absolute() {
            return Err(format!("webhook {name}: secret_file is an absolute path"));
        }
        if self.events.len() > WEBHOOK_EVENTS {
            return Err(format!(
                "webhook {name}: at most {WEBHOOK_EVENTS} event kinds"
            ));
        }
        for kind in &self.events {
            // Said back for correction, bounded: a name is at most 64 bytes
            // when it is a kind, so a longer one is not a kind and is not
            // echoed whole.
            let shown: String = kind.chars().take(64).collect();
            if kind.starts_with("webhook_") {
                return Err(format!(
                    "webhook {name}: `{shown}` is a webhook's own event and is never posted"
                ));
            }
            if !EventKind::is_wire_name(kind) {
                let near = closest_wire_names(kind);
                let hint = if near.is_empty() {
                    "the wire names are the EventKind variants in snake_case; \
                     see Events in docs/ARCHITECTURE.md"
                        .to_owned()
                } else {
                    let named: Vec<String> = near.iter().map(|n| format!("`{n}`")).collect();
                    format!("did you mean {}?", named.join(" or "))
                };
                return Err(format!(
                    "webhook {name}: `{shown}` is not an event kind; {hint}"
                ));
            }
        }
        Ok(())
    }
}

/// The emitted kinds nearest `name` by edit distance, when any is near
/// enough to be the one meant — within a third of the name's length,
/// at least two edits — so a misspelt or renamed name is answered with
/// the right one and an invented one is not answered with a guess.
/// Kinds within an edit of the nearest are named too, up to three, since
/// `question_asked` is as near `question_closed` as `question_opened`.
fn closest_wire_names(name: &str) -> Vec<&'static str> {
    let budget = (name.chars().count() / 3).max(2);
    let mut near: Vec<(usize, &'static str)> = EventKind::WIRE_NAMES
        .iter()
        .map(|candidate| (edit_distance(name, candidate), *candidate))
        .filter(|(distance, _)| *distance <= budget)
        .collect();
    near.sort_unstable();
    let Some(&(best, _)) = near.first() else {
        return Vec::new();
    };
    near.iter()
        .take_while(|(distance, _)| *distance <= best + 1)
        .take(3)
        .map(|(_, candidate)| *candidate)
        .collect()
}

/// Levenshtein distance over characters; both inputs are short.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != *cb);
            current.push(substitute.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

/// The shape of an address the daemon sends to: a host, no credentials
/// or fragment in it, and `https` — plain `http` only to this machine.
/// `hint` says where a credential belongs instead.
fn check_outbound(url: &str, hint: &str) -> Result<(), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "the url needs a scheme".to_owned())?;
    if rest.contains('#') {
        return Err("the url has a fragment".into());
    }
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    if authority.contains('@') {
        return Err(format!("the url carries credentials; {hint}"));
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(host, _)| host)
        .trim_matches(['[', ']']);
    if host.is_empty() {
        return Err("the url has no host".into());
    }
    let local = matches!(host, "127.0.0.1" | "::1" | "localhost");
    match scheme {
        "https" => Ok(()),
        "http" if local => Ok(()),
        "http" => Err(
            "plain http only reaches this machine (127.0.0.1, ::1 or localhost); use https".into(),
        ),
        other => Err(format!("{other}:// is not http or https")),
    }
}

/// `[judge]`: which judgments run, the key they are sent with, the model
/// and address that answer, and how many calls a minute they may make.
/// With no judgment named — the default — nothing is read for judging
/// and nothing is sent.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeConfig {
    /// Judgments by name; `screens` is the one this build knows (see
    /// `crate::judgment::Judgment`).
    #[serde(default)]
    pub judgments: Vec<String>,
    /// A private file (a regular file of this user, mode 0600) whose
    /// trimmed content is the TypeSafe API key.
    #[serde(default)]
    pub key_file: Option<std::path::PathBuf>,
    /// A versioned model, so the threshold keeps its meaning; an alias
    /// such as `jev-latest` moves underneath it.
    #[serde(default = "judge_model")]
    pub model: String,
    #[serde(default = "judge_url")]
    pub url: String,
    /// Calls a minute at most, 1–60.
    #[serde(default = "judge_calls")]
    pub calls_per_minute: u32,
    /// The least probability of yes that promotes, in percent, 50–99.
    #[serde(default = "judge_threshold")]
    pub threshold_percent: u8,
}

fn judge_model() -> String {
    crate::judgment::DEFAULT_MODEL.to_owned()
}

fn judge_url() -> String {
    crate::judgment::DEFAULT_URL.to_owned()
}

fn judge_calls() -> u32 {
    crate::judgment::DEFAULT_CALLS_PER_MINUTE
}

fn judge_threshold() -> u8 {
    crate::judgment::DEFAULT_THRESHOLD_PERCENT
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            judgments: Vec::new(),
            key_file: None,
            model: judge_model(),
            url: judge_url(),
            calls_per_minute: judge_calls(),
            threshold_percent: judge_threshold(),
        }
    }
}

impl JudgeConfig {
    /// The judgments to run, checked with everything they need: a key
    /// file at an absolute path, a model name, an address that goes
    /// nowhere surprising, a budget and a threshold in range. None named
    /// is valid and runs nothing, whatever else is set.
    pub fn enabled(&self) -> Result<std::collections::BTreeSet<crate::judgment::Judgment>, String> {
        use crate::judgment::{Judgment, MAX_CALLS_PER_MINUTE, valid_model};
        let mut enabled = std::collections::BTreeSet::new();
        for name in &self.judgments {
            let judgment = Judgment::parse(name).ok_or_else(|| {
                format!(
                    "judge: `{name}` is not a judgment ({})",
                    Judgment::ALL.map(Judgment::as_str).join(", ")
                )
            })?;
            if !enabled.insert(judgment) {
                return Err(format!("judge: `{name}` is named twice"));
            }
        }
        if enabled.is_empty() {
            return Ok(enabled);
        }
        match &self.key_file {
            Some(path) if path.is_absolute() => {}
            Some(_) => return Err("judge: key_file must be an absolute path".into()),
            None => return Err("judge: key_file names no key; nothing is sent without one".into()),
        }
        if !valid_model(&self.model) {
            return Err(
                "judge: model is 1–64 letters, digits, dots, hyphens or underscores".into(),
            );
        }
        check_outbound(&self.url, "put the key in key_file")
            .map_err(|error| format!("judge: {error}"))?;
        if !(1..=MAX_CALLS_PER_MINUTE).contains(&self.calls_per_minute) {
            return Err(format!(
                "judge: calls_per_minute must be 1–{MAX_CALLS_PER_MINUTE}"
            ));
        }
        if !(50..=99).contains(&self.threshold_percent) {
            return Err("judge: threshold_percent must be 50–99".into());
        }
        Ok(enabled)
    }
}

/// `[memory]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// `[memory.supermemory]`: a copy of chosen projects' memories kept in
    /// Supermemory, its hosted API or a server of the person's own. Off
    /// unless `enabled = true` and at least one project is named.
    #[serde(default)]
    pub supermemory: Option<SupermemoryConfig>,
}

/// Supermemory's own service, unless the person names another.
pub const SUPERMEMORY_URL: &str = "https://api.supermemory.ai";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupermemoryConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Projects whose memories are copied: ids, unique id prefixes or
    /// roots. No other project's are.
    #[serde(default)]
    pub projects: Vec<String>,
    /// A regular file of this user, mode 0600, holding the API key.
    pub key_file: std::path::PathBuf,
    /// The server: Supermemory's (`https://api.supermemory.ai`) unless
    /// named. Anything else must be `https://`, or `http://` on loopback.
    #[serde(default)]
    pub base_url: Option<String>,
    /// How the server forms memories from what is sent, one of
    /// [`DREAMING`]: sent with every addition, and then a document counts
    /// as there only once its memories are formed. Absent, nothing is sent
    /// and the server's default applies.
    #[serde(default)]
    pub dreaming: Option<String>,
    /// Background sent with every addition to guide how the server forms
    /// memories from it, at most [`SUPPORTING_CONTEXT_CHARS`] characters.
    /// Absent, nothing is sent.
    #[serde(default)]
    pub supporting_context: Option<String>,
}

/// The `dreaming` modes Supermemory takes.
pub const DREAMING: [&str; 2] = ["dynamic", "instant"];

/// The longest `supporting_context` Supermemory takes, in characters.
pub const SUPPORTING_CONTEXT_CHARS: usize = 1_500;

impl SupermemoryConfig {
    pub fn check(&self) -> Result<(), String> {
        if !self.key_file.is_absolute() {
            return Err("memory.supermemory.key_file must be an absolute path".into());
        }
        if self.enabled && self.projects.is_empty() {
            return Err("memory.supermemory names no projects; name the ones to copy".into());
        }
        if self
            .dreaming
            .as_deref()
            .is_some_and(|mode| !DREAMING.contains(&mode))
        {
            return Err(
                "memory.supermemory.dreaming must be \"dynamic\" or \"instant\", or left out"
                    .into(),
            );
        }
        if let Some(context) = &self.supporting_context {
            let length = context.chars().count();
            if length > SUPPORTING_CONTEXT_CHARS {
                return Err(format!(
                    "memory.supermemory.supporting_context is {length} characters; \
                     Supermemory takes at most {SUPPORTING_CONTEXT_CHARS}"
                ));
            }
        }
        let url = self.base_url();
        let bad = |why: &str| Err(format!("memory.supermemory.base_url {why}"));
        let Some((scheme, rest)) = url.split_once("://") else {
            return bad("needs a scheme");
        };
        if rest.contains(['?', '#']) {
            return bad("takes no query or fragment");
        }
        let authority = rest.split('/').next().unwrap_or("");
        if authority.contains('@') {
            return bad("carries no credentials; the key goes in key_file");
        }
        let host = authority
            .rsplit_once(':')
            .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
            .map_or(authority, |(host, _)| host);
        if host.is_empty() {
            return bad("has no host");
        }
        let local = matches!(host, "127.0.0.1" | "[::1]" | "localhost");
        match scheme {
            "https" => {}
            "http" if local => {}
            _ => {
                return bad(
                    "must be https://, or http:// on this machine (127.0.0.1, [::1], localhost)",
                );
            }
        }
        Ok(())
    }

    pub fn base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or(SUPERMEMORY_URL)
            .trim_end_matches('/')
    }

    /// The namespace a project's memories are kept under there: letters,
    /// digits, `_`, `:` and `-` only, at most 100 characters.
    pub fn namespace(project: &crate::ProjectId) -> String {
        format!("agentdocker_{}", project.as_str())
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .take(100)
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagesConfig {
    /// How long archived messages are kept, as `30m`, `12h`, `180d` or
    /// plain seconds. Absent means the per-conversation cap alone bounds
    /// the archive.
    #[serde(default)]
    pub retention: Option<String>,
    /// How many messages each topic keeps for subscribers that join later,
    /// 1 to [`TOPIC_LIMIT_MAX`]. Absent means [`TOPIC_LIMIT`].
    #[serde(default)]
    pub topic_limit: Option<usize>,
    /// How long a topic keeps a message, as `30m`, `12h`, `7d` or plain
    /// seconds. Absent means [`TOPIC_RETENTION_DAYS`] days.
    #[serde(default)]
    pub topic_retention: Option<String>,
}

/// How many messages a topic keeps unless `[messages] topic_limit` says.
pub const TOPIC_LIMIT: usize = 1_000;
/// The most `[messages] topic_limit` may say.
pub const TOPIC_LIMIT_MAX: usize = 100_000;
/// How many days a topic keeps a message unless `[messages]
/// topic_retention` says.
pub const TOPIC_RETENTION_DAYS: i64 = 7;

/// What each topic keeps: at most `limit` messages, none older than
/// `window`. Always bounded: there is no setting that keeps a topic for
/// ever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopicRetention {
    pub limit: usize,
    pub window: Duration,
}

impl Default for TopicRetention {
    fn default() -> Self {
        Self {
            limit: TOPIC_LIMIT,
            window: Duration::days(TOPIC_RETENTION_DAYS),
        }
    }
}

/// Observations one agent's read set holds unless `[read_set] capacity`
/// says otherwise: three times both the largest read set a real session
/// has held and a session that observes every file of a checkout of six
/// hundred, rounded up. Cost follows the set's size, not the capacity.
pub const READ_SET_CAPACITY: usize = 2_000;
/// The least capacity the file may set: five times the read set that
/// ninety-nine sessions in a hundred stay within.
pub const READ_SET_CAPACITY_MIN: usize = 100;
/// The most capacity the file may set. Every observation rewrites the
/// whole set under the daemon's one lock, so its cost grows with the set;
/// at this size each takes tens of milliseconds and writes megabytes.
pub const READ_SET_CAPACITY_MAX: usize = 10_000;
const _: () = assert!(
    READ_SET_CAPACITY_MIN <= READ_SET_CAPACITY && READ_SET_CAPACITY <= READ_SET_CAPACITY_MAX
);

/// `[read_set]`: how much one agent's read set holds. An observation that
/// would take the set past `capacity` is refused; nothing is evicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadSetConfig {
    /// Observations (distinct physical paths) one agent's read set holds
    /// at most, [`READ_SET_CAPACITY_MIN`]–[`READ_SET_CAPACITY_MAX`].
    #[serde(default = "read_set_capacity")]
    pub capacity: usize,
}

fn read_set_capacity() -> usize {
    READ_SET_CAPACITY
}

impl Default for ReadSetConfig {
    fn default() -> Self {
        Self {
            capacity: READ_SET_CAPACITY,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalConfig {
    /// How long journal entries are kept, as `30m`, `12h`, `180d` or plain
    /// seconds. Absent means forever: the journal is the audit trail.
    #[serde(default)]
    pub retention: Option<String>,
}

impl DaemonConfig {
    /// The memory mirror, when it is on and its settings hold.
    pub fn supermemory(&self) -> Result<Option<&SupermemoryConfig>, String> {
        match &self.memory.supermemory {
            Some(mirror) if mirror.enabled => mirror.check().map(|()| Some(mirror)),
            _ => Ok(None),
        }
    }

    /// The journal retention window, when one is configured. A window that
    /// cannot be read is a broken file, not a file that keeps everything:
    /// callers treat the error as "ignore the file and say so".
    pub fn journal_retention(&self) -> Result<Option<Duration>, String> {
        self.journal
            .retention
            .as_deref()
            .map(|text| parse_duration(text).map_err(|error| format!("journal.retention: {error}")))
            .transpose()
    }

    /// The sinks, each checked, their names distinct, no more than
    /// [`WEBHOOKS`] of them.
    pub fn webhooks(&self) -> Result<&[WebhookConfig], String> {
        if self.webhooks.len() > WEBHOOKS {
            return Err(format!("at most {WEBHOOKS} webhooks"));
        }
        let mut names = std::collections::BTreeSet::new();
        for sink in &self.webhooks {
            sink.check()?;
            if !names.insert(sink.name.as_str()) {
                return Err(format!("webhook {} is named twice", sink.name));
            }
        }
        Ok(&self.webhooks)
    }

    /// How long finished agents' records are kept: thirty days unless the
    /// file says otherwise, `None` when it says `off`.
    pub fn agents_retention(&self) -> Result<Option<Duration>, String> {
        let text = self.agents.retention.trim();
        if text.eq_ignore_ascii_case("off") || text.eq_ignore_ascii_case("never") {
            return Ok(None);
        }
        parse_duration(text)
            .map(Some)
            .map_err(|error| format!("agents.retention: {error}"))
    }

    /// The message archive's retention window, when one is configured.
    pub fn messages_retention(&self) -> Result<Option<Duration>, String> {
        self.messages
            .retention
            .as_deref()
            .map(|text| {
                parse_duration(text).map_err(|error| format!("messages.retention: {error}"))
            })
            .transpose()
    }

    /// How many observations one agent's read set holds: the file's word
    /// when it is in range, [`READ_SET_CAPACITY`] when it says nothing. A
    /// value out of range is a broken setting, not a request for the
    /// nearest bound: callers keep the default and say so.
    pub fn read_set_capacity(&self) -> Result<usize, String> {
        let capacity = self.read_set.capacity;
        if (READ_SET_CAPACITY_MIN..=READ_SET_CAPACITY_MAX).contains(&capacity) {
            Ok(capacity)
        } else {
            Err(format!(
                "read_set.capacity must be {READ_SET_CAPACITY_MIN}–{READ_SET_CAPACITY_MAX}, not {capacity}"
            ))
        }
    }

    /// What each topic keeps: the file's `topic_limit` and
    /// `topic_retention` where it gives them, the defaults where it does
    /// not. A value that cannot be read is an error, for the caller to say
    /// once and keep the defaults.
    pub fn topic_retention(&self) -> Result<TopicRetention, String> {
        let mut retention = TopicRetention::default();
        if let Some(limit) = self.messages.topic_limit {
            if !(1..=TOPIC_LIMIT_MAX).contains(&limit) {
                return Err(format!("messages.topic_limit must be 1–{TOPIC_LIMIT_MAX}"));
            }
            retention.limit = limit;
        }
        if let Some(text) = &self.messages.topic_retention {
            retention.window = parse_duration(text)
                .map_err(|error| format!("messages.topic_retention: {error}"))?;
        }
        Ok(retention)
    }
}

/// `45s`, `30m`, `12h`, `7d`, or a plain number of seconds. Whitespace around
/// the value is tolerated; anything else, including zero, is refused so an
/// empty or negative window is never applied by accident.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (digits, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((at, _)) => text.split_at(at),
        None => (text, "s"),
    };
    let amount: i64 = digits
        .parse()
        .map_err(|_| format!("`{text}` is not a duration such as 30m, 12h or 180d"))?;
    if amount == 0 {
        return Err("a duration must be positive".to_owned());
    }
    let seconds = match unit.trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(amount),
        "m" | "min" | "mins" | "minute" | "minutes" => amount.checked_mul(60),
        "h" | "hr" | "hrs" | "hour" | "hours" => amount.checked_mul(3_600),
        "d" | "day" | "days" => amount.checked_mul(86_400),
        _ => {
            return Err(format!(
                "`{text}` is not a duration such as 30m, 12h or 180d"
            ));
        }
    }
    .ok_or_else(|| format!("`{text}` is too long a duration"))?;
    Duration::try_seconds(seconds).ok_or_else(|| format!("`{text}` is too long a duration"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mirror is off unless turned on, names its projects, keeps its
    /// key in an absolute file and talks https (or http to this machine).
    #[test]
    fn the_memory_mirror_is_off_unless_turned_on_and_checked_when_it_is() {
        let key = std::env::temp_dir().join("supermemory.key");
        let parsed = |text: &str| toml::from_str::<DaemonConfig>(text).unwrap();
        assert_eq!(parsed("").supermemory(), Ok(None));
        let off = format!(
            "[memory.supermemory]\nkey_file = {:?}\nprojects = [\"p\"]\n",
            key.display().to_string()
        );
        assert_eq!(parsed(&off).supermemory(), Ok(None), "not enabled");
        let on = |extra: &str| {
            format!(
                "[memory.supermemory]\nenabled = true\nkey_file = {:?}\n{extra}",
                key.display().to_string()
            )
        };
        let config = parsed(&on("projects = [\"p\"]\n"));
        let mirror = config.supermemory().unwrap().unwrap();
        assert_eq!(mirror.base_url(), SUPERMEMORY_URL);
        assert_eq!(
            SupermemoryConfig::namespace(&crate::ProjectId::from("abc")),
            "agentdocker_abc"
        );
        let odd = SupermemoryConfig::namespace(&crate::ProjectId::from(&*"a/b c".repeat(40)));
        assert!(
            odd.len() <= 100
                && odd
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_:-".contains(c))
        );
        assert!(parsed(&on("")).supermemory().is_err(), "no projects");
        for url in [
            "http://example.com",
            "ftp://x",
            "http://localhost.evil.com",
            "http://localhost:80@collector.example",
            "https://user:pw@api.supermemory.ai",
            "https://api.supermemory.ai?x=1",
            "https://api.supermemory.ai#x",
            "http://127.0.0.1.example",
        ] {
            let config = parsed(&on(&format!("projects = [\"p\"]\nbase_url = {url:?}\n")));
            assert!(config.supermemory().is_err(), "{url}");
        }
        for url in [
            "https://memory.example.com/",
            "http://127.0.0.1:8787",
            "http://localhost",
        ] {
            let config = parsed(&on(&format!("projects = [\"p\"]\nbase_url = {url:?}\n")));
            assert!(config.supermemory().is_ok(), "{url}");
        }
        assert!(
            parsed(
                "[memory.supermemory]\nenabled = true\nkey_file = \"key\"\nprojects = [\"p\"]\n"
            )
            .supermemory()
            .is_err(),
            "a relative key file"
        );
    }

    /// `dreaming` and `supporting_context` are absent unless written; a
    /// mode is one Supermemory names, and the context is held to its limit
    /// in characters, not bytes.
    #[test]
    fn the_mirror_sends_a_dreaming_mode_and_context_only_when_written_and_within_limits() {
        let key = std::env::temp_dir().join("supermemory.key");
        let on = |extra: &str| {
            toml::from_str::<DaemonConfig>(&format!(
                "[memory.supermemory]\nenabled = true\nkey_file = {:?}\nprojects = [\"p\"]\n{extra}",
                key.display().to_string()
            ))
            .unwrap()
        };
        let plain = on("");
        let mirror = plain.supermemory().unwrap().unwrap();
        assert_eq!(mirror.dreaming, None);
        assert_eq!(mirror.supporting_context, None);

        for mode in DREAMING {
            let config = on(&format!("dreaming = {mode:?}\n"));
            let mirror = config.supermemory().unwrap().unwrap();
            assert_eq!(mirror.dreaming.as_deref(), Some(mode));
        }
        for mode in ["fast", "Dynamic", ""] {
            let error = on(&format!("dreaming = {mode:?}\n"))
                .supermemory()
                .unwrap_err();
            assert!(
                error.contains("dreaming") && error.contains("\"instant\""),
                "{error}"
            );
        }

        // Two bytes a character: the limit is counted in characters.
        let longest = "é".repeat(SUPPORTING_CONTEXT_CHARS);
        let config = on(&format!("supporting_context = {longest:?}\n"));
        let mirror = config.supermemory().unwrap().unwrap();
        assert_eq!(mirror.supporting_context.as_deref(), Some(&*longest));
        let over = "é".repeat(SUPPORTING_CONTEXT_CHARS + 1);
        let error = on(&format!("supporting_context = {over:?}\n"))
            .supermemory()
            .unwrap_err();
        assert!(
            error.contains("supporting_context") && error.contains("at most 1500"),
            "{error}"
        );
        assert!(!error.contains('é'), "the text itself is not repeated");
    }

    /// A sink is checked for the shape that keeps it safe: a sayable
    /// name, https (or http only to this machine), no credentials or
    /// fragment in the address, an absolute private secret path, kinds
    /// that are kinds and never a webhook's own, distinct names, a bound
    /// on the count.
    #[test]
    fn webhook_sinks_are_checked_for_shape() {
        // An absolute path on this platform: `/Users/…` is relative on
        // Windows, where the core tests also run.
        let secret = std::env::temp_dir().join("slack.secret");
        let good = |url: &str| WebhookConfig {
            name: "team-slack".into(),
            url: url.into(),
            secret_file: secret.clone(),
            events: vec!["question_opened".into(), "lease_deadlock".into()],
            project: None,
            format: WebhookFormat::Slack,
        };
        assert!(
            good("https://hooks.slack.com/services/T/B/x")
                .check()
                .is_ok()
        );
        assert!(good("http://127.0.0.1:8080/hook").check().is_ok());
        assert!(good("http://localhost/hook").check().is_ok());
        assert!(good("http://[::1]:9/hook").check().is_ok());
        for bad in [
            "http://example.com/hook",
            "ftp://example.com/hook",
            "https://user:pw@example.com/hook",
            "https://example.com/hook#frag",
            "https:///nohost",
            "nothing",
        ] {
            assert!(good(bad).check().is_err(), "{bad}");
        }
        let mut sink = good("https://example.com/hook");
        sink.name = "Team Slack".into();
        assert!(sink.check().is_err(), "the name");
        let mut sink = good("https://example.com/hook");
        sink.secret_file = "relative.secret".into();
        assert!(sink.check().is_err(), "the secret path");
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["webhook_failed".into()];
        assert!(sink.check().is_err(), "its own event");
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["Question Asked".into()];
        assert!(sink.check().is_err(), "not a kind");
        // A misspelt or renamed wire name used to pass on its characters
        // alone and that kind then never posted; it is refused by name,
        // with the kind meant.
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["question_asked".into()];
        let error = sink.check().unwrap_err();
        assert!(
            error.contains("`question_asked` is not an event kind")
                && error.contains("did you mean `question_closed` or `question_opened`?"),
            "{error}"
        );
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["lease_dedlock".into()];
        assert!(
            sink.check()
                .unwrap_err()
                .contains("did you mean `lease_deadlock`?")
        );
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["unknown".into()];
        assert!(
            sink.check().is_err(),
            "the reader's catch-all is never emitted"
        );
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["zzzzzzzzzzzzzzzzzzzz".into()];
        let error = sink.check().unwrap_err();
        assert!(
            error.contains("is not an event kind") && error.contains("docs/ARCHITECTURE.md"),
            "no guess for a name near nothing: {error}"
        );
        let mut sink = good("https://example.com/hook");
        sink.events = vec!["x".repeat(400)];
        let error = sink.check().unwrap_err();
        assert!(
            error.len() < 300,
            "a long name is not echoed whole: {error}"
        );
        for name in EventKind::WIRE_NAMES {
            if name.starts_with("webhook_") {
                continue;
            }
            let mut sink = good("https://example.com/hook");
            sink.events = vec![(*name).into()];
            assert!(sink.check().is_ok(), "{name}");
        }
        let text = format!(
            r#"
[[webhooks]]
name = "a"
url = "https://example.com/a"
secret_file = {secret:?}
events = ["agent_exited"]

[[webhooks]]
name = "a"
url = "https://example.com/b"
secret_file = {secret:?}
"#,
            secret = secret.display().to_string()
        );
        let config: DaemonConfig = toml::from_str(&text).unwrap();
        assert!(config.webhooks().unwrap_err().contains("named twice"));
        let many: Vec<WebhookConfig> = (0..=WEBHOOKS)
            .map(|i| {
                let mut sink = good("https://example.com/hook");
                sink.name = format!("sink-{i}");
                sink
            })
            .collect();
        let config = DaemonConfig {
            webhooks: many,
            ..DaemonConfig::default()
        };
        assert!(config.webhooks().is_err());
        let none: DaemonConfig = toml::from_str("").unwrap();
        assert!(none.webhooks().unwrap().is_empty());
    }

    /// The judge reads and sends nothing unless judgments are named, and
    /// when they are it needs an absolute key file, a model, an address
    /// of the safe shape, a budget and a threshold in range.
    #[test]
    fn the_judge_is_off_until_named_and_checked_when_it_is() {
        use crate::judgment::Judgment;
        let parse = |text: &str| toml::from_str::<DaemonConfig>(text).unwrap();
        assert!(parse("").judge.enabled().unwrap().is_empty(), "no section");
        assert!(
            parse("[judge]\nkey_file = \"relative\"\n")
                .judge
                .enabled()
                .unwrap()
                .is_empty(),
            "a key with nothing named runs nothing"
        );
        let key = std::env::temp_dir().join("typesafe.key");
        let config = parse(&format!(
            "[judge]\njudgments = [\"screens\"]\nkey_file = {:?}\n",
            key.display().to_string()
        ));
        assert_eq!(
            config
                .judge
                .enabled()
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            [Judgment::Screens]
        );
        assert_eq!(config.judge.model, "jev-1.13.0");
        assert_eq!(config.judge.url, "https://api.typesafe.ai/v1/systemone");
        assert_eq!(config.judge.calls_per_minute, 6);
        assert_eq!(config.judge.threshold_percent, 75);

        let with = |edit: &dyn Fn(&mut JudgeConfig)| {
            let mut judge = config.judge.clone();
            edit(&mut judge);
            judge.enabled()
        };
        assert!(with(&|j| j.key_file = None).is_err(), "no key");
        let relative = with(&|j| j.key_file = Some("typesafe.key".into())).unwrap_err();
        assert!(relative.contains("absolute"), "{relative}");
        assert!(
            with(&|j| j.judgments.push("screens".into())).is_err(),
            "twice"
        );
        assert!(
            with(&|j| j.judgments.push("everything".into())).is_err(),
            "unknown"
        );
        assert!(with(&|j| j.model = "jev latest".into()).is_err(), "model");
        assert!(with(&|j| j.model = String::new()).is_err(), "no model");
        for url in [
            "http://api.typesafe.ai/v1/systemone",
            "http://localhost.evil.com/v1",
            "http://127.0.0.1.example/v1",
            "ftp://api.typesafe.ai/v1",
            "https://user:key@api.typesafe.ai/v1",
            "https://api.typesafe.ai/v1#x",
            "api.typesafe.ai/v1/systemone",
        ] {
            assert!(with(&|j| j.url = url.into()).is_err(), "{url}");
        }
        for url in [
            "https://api.typesafe.ai/v1/systemone",
            "http://127.0.0.1:9/v1/systemone",
            "http://[::1]:9/v1/systemone",
            "http://localhost:9/v1/systemone",
        ] {
            assert!(with(&|j| j.url = url.into()).is_ok(), "{url}");
        }
        assert!(with(&|j| j.calls_per_minute = 0).is_err());
        assert!(with(&|j| j.calls_per_minute = 61).is_err());
        assert!(with(&|j| j.calls_per_minute = 60).is_ok());
        assert!(with(&|j| j.threshold_percent = 49).is_err());
        assert!(with(&|j| j.threshold_percent = 100).is_err());
        assert!(with(&|j| j.threshold_percent = 95).is_ok());
        assert!(toml::from_str::<DaemonConfig>("[judge]\nkey = \"x\"\n").is_err());
    }

    #[test]
    fn durations_parse_with_units_and_plain_seconds() {
        assert_eq!(parse_duration("45").unwrap(), Duration::seconds(45));
        assert_eq!(parse_duration("45s").unwrap(), Duration::seconds(45));
        assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_duration(" 12h ").unwrap(), Duration::hours(12));
        assert_eq!(parse_duration("180d").unwrap(), Duration::days(180));
        assert_eq!(parse_duration("2 days").unwrap(), Duration::days(2));
    }

    #[test]
    fn durations_refuse_zero_negative_junk_and_overflow() {
        for bad in [
            "",
            "0",
            "0d",
            "-1d",
            "d",
            "1w",
            "1.5h",
            "soon",
            "99999999999999999d",
            // Fits an i64 of seconds, not a chrono duration: an error, not a panic.
            "9223372036854775807",
            "9223372036854775807s",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn finished_agents_are_kept_thirty_days_unless_the_file_says_otherwise() {
        let parse = |text: &str| toml::from_str::<DaemonConfig>(text);
        assert_eq!(
            DaemonConfig::default().agents_retention().unwrap(),
            Some(Duration::days(30))
        );
        assert_eq!(
            parse("[journal]\nretention = \"180d\"\n")
                .unwrap()
                .agents_retention()
                .unwrap(),
            Some(Duration::days(30))
        );
        assert_eq!(
            parse("[agents]\nretention = \"7d\"\n")
                .unwrap()
                .agents_retention()
                .unwrap(),
            Some(Duration::days(7))
        );
        assert_eq!(
            parse("[agents]\nretention = \"off\"\n")
                .unwrap()
                .agents_retention()
                .unwrap(),
            None
        );
        assert!(
            parse("[agents]\nretention = \"soon\"\n")
                .unwrap()
                .agents_retention()
                .unwrap_err()
                .starts_with("agents.retention:")
        );
        assert!(parse("[agents]\nkeep = \"7d\"\n").is_err());
    }

    #[test]
    fn experimental_switches_default_off_except_native_codex_and_the_environment_only_turns_on() {
        let defaults = DaemonConfig::default().experimental;
        assert_eq!(
            defaults,
            ExperimentalConfig {
                reload: false,
                secret_input: false,
                native_codex: true
            }
        );
        let file: DaemonConfig =
            toml::from_str("[experimental]\nreload = true\nnative_codex = false\n").unwrap();
        assert_eq!(
            file.experimental,
            ExperimentalConfig {
                reload: true,
                secret_input: false,
                native_codex: false
            }
        );
        assert!(
            toml::from_str::<DaemonConfig>("[experimental]\nreplay = true\n").is_err(),
            "an unknown switch is refused, not ignored"
        );
        let env = |name: &str| match name {
            ExperimentalConfig::SECRET_INPUT_ENV => Some("1".to_owned()),
            ExperimentalConfig::RELOAD_ENV => Some("0".to_owned()),
            _ => None,
        };
        let applied = file.experimental.with_env(env);
        assert_eq!(
            applied,
            ExperimentalConfig {
                reload: true,
                secret_input: true,
                native_codex: false
            },
            "exactly `1` turns a switch on; nothing in the environment turns one off"
        );
        assert_eq!(
            applied.summary(),
            "reload on, secret input on, native codex off"
        );
        assert_eq!(
            serde_json::from_str::<ExperimentalConfig>("{}").unwrap(),
            defaults,
            "an older daemon's empty report reads as the defaults"
        );
    }

    #[test]
    fn config_parses_retention_and_refuses_unknown_keys() {
        let parse = |text: &str| toml::from_str::<DaemonConfig>(text);
        let config = parse("[journal]\nretention = \"180d\"\n").unwrap();
        assert_eq!(
            config.journal_retention().unwrap(),
            Some(Duration::days(180))
        );
        assert_eq!(parse("").unwrap(), DaemonConfig::default());
        let unreadable = parse("[journal]\nretention = \"forever\"\n").unwrap();
        assert!(unreadable.journal_retention().is_err());
        assert!(parse("[journal]\nretain = \"1d\"\n").is_err());
        assert!(parse("[jurnal]\nretention = \"1d\"\n").is_err());
    }

    /// The capacity is the default unless written, the file's word within
    /// its range, and refused — not clamped — outside it; a value of the
    /// wrong type or an unknown key fails the file as any other would.
    #[test]
    fn read_set_capacity_defaults_takes_its_range_and_refuses_the_rest() {
        let parse = |text: &str| toml::from_str::<DaemonConfig>(text);
        assert_eq!(
            parse("").unwrap().read_set_capacity(),
            Ok(READ_SET_CAPACITY)
        );
        assert_eq!(
            parse("[read_set]\n").unwrap().read_set_capacity(),
            Ok(READ_SET_CAPACITY)
        );
        for capacity in [READ_SET_CAPACITY_MIN, 2_500, READ_SET_CAPACITY_MAX] {
            let config = parse(&format!("[read_set]\ncapacity = {capacity}\n")).unwrap();
            assert_eq!(config.read_set_capacity(), Ok(capacity));
        }
        for capacity in [0, READ_SET_CAPACITY_MIN - 1, READ_SET_CAPACITY_MAX + 1] {
            let error = parse(&format!("[read_set]\ncapacity = {capacity}\n"))
                .unwrap()
                .read_set_capacity()
                .unwrap_err();
            assert_eq!(
                error,
                format!(
                    "read_set.capacity must be {READ_SET_CAPACITY_MIN}–{READ_SET_CAPACITY_MAX}, not {capacity}"
                )
            );
        }
        assert!(parse("[read_set]\ncapacity = -1\n").is_err(), "negative");
        assert!(
            parse("[read_set]\ncapacity = \"1000\"\n").is_err(),
            "a string"
        );
        assert!(parse("[read_set]\nmarks = 1000\n").is_err(), "unknown key");
    }

    /// What the documents tell a person is what the daemon applies: the
    /// default and the range, in the architecture's capacity rule and in
    /// the guide's example.
    #[test]
    fn the_documented_read_set_capacity_is_the_default_and_range() {
        fn grouped(n: usize) -> String {
            let digits = n.to_string();
            let mut out = String::new();
            for (i, digit) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(digit);
            }
            out
        }
        let architecture = include_str!("../../../docs/ARCHITECTURE.md");
        let rule = format!(
            "{} unless `agentd.toml` sets it, {}–{}",
            grouped(READ_SET_CAPACITY),
            grouped(READ_SET_CAPACITY_MIN),
            grouped(READ_SET_CAPACITY_MAX)
        );
        assert!(architecture.contains(&rule), "ARCHITECTURE.md: {rule}");
        let guide = include_str!("../../../docs/GUIDE.md");
        let example = format!(
            "capacity = {READ_SET_CAPACITY}  # the default; {READ_SET_CAPACITY_MIN}–{READ_SET_CAPACITY_MAX}"
        );
        assert!(guide.contains(&example), "GUIDE.md: {example}");
        assert_eq!(grouped(10_000), "10,000");
        assert_eq!(grouped(100), "100");
    }

    /// A topic keeps a thousand messages for seven days unless the file
    /// says otherwise; either value may be given alone, and one that
    /// cannot be read is refused rather than taken as unbounded.
    #[test]
    fn topic_retention_has_bounded_defaults_and_reads_either_value() {
        let parse = |text: &str| toml::from_str::<DaemonConfig>(text).unwrap();
        assert_eq!(
            DaemonConfig::default().topic_retention().unwrap(),
            TopicRetention {
                limit: 1_000,
                window: Duration::days(7)
            }
        );
        assert_eq!(
            parse("[messages]\ntopic_limit = 50\n")
                .topic_retention()
                .unwrap(),
            TopicRetention {
                limit: 50,
                window: Duration::days(7)
            }
        );
        assert_eq!(
            parse("[messages]\ntopic_retention = \"12h\"\nretention = \"30d\"\n")
                .topic_retention()
                .unwrap(),
            TopicRetention {
                limit: 1_000,
                window: Duration::hours(12)
            }
        );
        for broken in [
            "[messages]\ntopic_limit = 0\n",
            "[messages]\ntopic_limit = 100001\n",
            "[messages]\ntopic_retention = \"forever\"\n",
            "[messages]\ntopic_retention = \"0\"\n",
        ] {
            assert!(parse(broken).topic_retention().is_err(), "{broken}");
        }
        assert!(toml::from_str::<DaemonConfig>("[messages]\ntopic_keep = 5\n").is_err());
    }
}
