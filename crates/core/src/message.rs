//! Messages exchanged between agents.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, ChannelId, ProjectId, QuestionPermissions};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(String);

impl MessageId {
    pub fn generate() -> Self {
        let raw = uuid::Uuid::new_v4().simple().to_string();
        Self(raw[..16].to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for MessageId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a message goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Destination {
    /// One agent. Clients may address by name; the daemon resolves to an id
    /// before publishing.
    Agent(AgentId),
    /// Every live subscriber of a topic such as `repo/backend/reviews`.
    /// Subscribers use MQTT-style patterns (`+` one level, `#` the rest).
    Topic(String),
    /// Every live agent working in one project, except the sender. Clients
    /// may give an id prefix or an absolute path inside the project; the
    /// daemon resolves to the id before publishing.
    Project(ProjectId),
    /// Every member of a channel except the sender. Unlike a topic, the
    /// membership is the channel's, not a subscription: an agent put in a
    /// channel hears it without asking.
    Channel(ChannelId),
    /// Every live agent. Only the person, or the daemon, says it to every
    /// project; an agent's `all` is published to its own project.
    Broadcast,
}

impl Destination {
    /// Parse the shorthand accepted on the command line: `all` / `*` →
    /// broadcast, `topic:x/y` → topic, `project:<id or path>` → project,
    /// anything else → agent.
    pub fn parse(s: &str) -> Self {
        if let "all" | "*" = s {
            return Self::Broadcast;
        }
        if let Some(topic) = s.strip_prefix("topic:") {
            return Self::Topic(topic.to_owned());
        }
        if let Some(project) = s.strip_prefix("project:") {
            return Self::Project(ProjectId::from(project));
        }
        if let Some(channel) = s.strip_prefix("channel:") {
            return Self::Channel(ChannelId::from(channel));
        }
        Self::Agent(AgentId::from(s))
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent(id) => f.write_str(id.short()),
            Self::Topic(topic) => write!(f, "topic:{topic}"),
            Self::Project(id) => write!(f, "project:{}", id.short()),
            Self::Channel(id) => write!(f, "channel:{id}"),
            Self::Broadcast => f.write_str("all"),
        }
    }
}

/// The unit of communication between agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: MessageId,
    /// Sender: an agent id, or `user` for messages injected from the CLI.
    pub from: String,
    pub to: Destination,
    /// Application-level type: `chat`, `task`, `handoff`, `question`,
    /// `answer`, `notice`... Agents agree on kinds; the daemon just routes.
    pub kind: String,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<MessageId>,
    pub sent_at: DateTime<Utc>,
    /// What the message points at, typed, beside its text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<crate::Link>,
}

impl Envelope {
    pub fn new(
        from: impl Into<String>,
        to: Destination,
        kind: impl Into<String>,
        payload: serde_json::Value,
        reply_to: Option<MessageId>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: MessageId::generate(),
            from: from.into(),
            to,
            kind: kind.into(),
            payload,
            reply_to,
            sent_at: now,
            links: Vec::new(),
        }
    }
}

/// The runtime of the agent that is a person rather than a program, and
/// the name that agent is always registered under. Orchestration needs an
/// escalation path, and it works best inside the same model as everything
/// else: the human is an agent you can address, queue messages for, and
/// ask.
pub const HUMAN: &str = "user";
pub const HUMAN_RUNTIME: &str = "human";

/// A question waiting for an answer.
///
/// `ask` blocks its caller until the answer comes back, so the daemon has
/// to keep the outstanding questions: an answer names the question by id,
/// and only the question knows who to send the answer to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: MessageId,
    /// Who is waiting, as an agent id.
    pub from: String,
    /// Who was asked.
    pub to: Destination,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<QuestionPresentation>,
    pub asked_at: DateTime<Utc>,
    /// When the asker gives up. An answer after this is still delivered as
    /// an ordinary message; it just has nobody blocked on it.
    pub expires_at: DateTime<Utc>,
    /// What the asker asked for beyond the words: the kind of decision,
    /// the choices, and what happens when nobody answers in time. Every
    /// field has a default, so a question with none of them is the free
    /// text it always was.
    #[serde(flatten)]
    pub form: QuestionForm,
    /// When the question went stale: its `stale_after_secs` passed with
    /// no answer. Set once, by the daemon, with the `question_stale`
    /// event; `None` while the question is fresh or when it never set a
    /// stale window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_since: Option<DateTime<Utc>>,
}

/// The kind of decision a question asks for. Each kind gets its own chip
/// in the desktop and its own word in a terminal; the daemon treats them
/// alike otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// A choice or a piece of information only the recipient has.
    #[default]
    Question,
    /// Leave to do one operation once: run a command, change these files.
    Permission,
    /// Standing access to a resource: a credential, a service, a path.
    Access,
}

impl QuestionKind {
    pub const ALL: [QuestionKind; 3] = [Self::Question, Self::Permission, Self::Access];

    /// The wire name, which is also the word a chip shows in lower case.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Permission => "permission",
            Self::Access => "access",
        }
    }

    /// The chip's word.
    pub fn label(self) -> &'static str {
        match self {
            Self::Question => "Question",
            Self::Permission => "Permission",
            Self::Access => "Access",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str().eq_ignore_ascii_case(value.trim()))
    }

    fn is_question(&self) -> bool {
        *self == Self::Question
    }
}

impl std::fmt::Display for QuestionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the daemon does when a question's stale window passes with no
/// answer. The default keeps waiting: the question stays open until it is
/// answered, cancelled or expires, exactly as a question with no window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum StaleAction {
    /// Mark it stale and keep it open.
    #[default]
    KeepWaiting,
    /// Answer it with this option, attributed to the daemon as the default.
    AnswerDefault { label: String },
    /// Close it unanswered; a waiting `ask` gets `error(timeout)`.
    Withdraw,
}

impl StaleAction {
    /// Parse the one-line spelling the CLI and the MCP tools take:
    /// `keep_waiting`, `withdraw`, or `answer:<label>`.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("keep_waiting") || value.eq_ignore_ascii_case("keep-waiting")
        {
            return Some(Self::KeepWaiting);
        }
        if value.eq_ignore_ascii_case("withdraw") {
            return Some(Self::Withdraw);
        }
        let label = value
            .strip_prefix("answer:")
            .or_else(|| value.strip_prefix("answer="))?
            .trim();
        (!label.is_empty()).then(|| Self::AnswerDefault {
            label: label.to_owned(),
        })
    }

    fn is_keep_waiting(&self) -> bool {
        *self == Self::KeepWaiting
    }
}

impl std::fmt::Display for StaleAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeepWaiting => f.write_str("keep_waiting"),
            Self::Withdraw => f.write_str("withdraw"),
            Self::AnswerDefault { label } => write!(f, "answer:{label}"),
        }
    }
}

/// The typed part of a question: its kind, its options and its stale
/// rule. Shared by the `ask` and `post_question` requests, which carry it
/// flattened beside the text, and by the stored [`Question`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionForm {
    #[serde(default, skip_serializing_if = "QuestionKind::is_question")]
    pub kind: QuestionKind,
    /// The choices offered; empty means free text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOption>,
    /// Several options may be chosen at once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
    /// Besides the options, an answer in the recipient's own words is
    /// taken. On by default; meaningless without options.
    #[serde(default = "true_by_default", skip_serializing_if = "Clone::clone")]
    pub allow_other: bool,
    /// How long after it is asked the question goes stale, if ever. Must
    /// be shorter than the timeout: stale comes before expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_after_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "StaleAction::is_keep_waiting")]
    pub stale_action: StaleAction,
}

fn true_by_default() -> bool {
    true
}

impl Default for QuestionForm {
    fn default() -> Self {
        Self {
            kind: QuestionKind::Question,
            options: Vec::new(),
            multi_select: false,
            allow_other: true,
            stale_after_secs: None,
            stale_action: StaleAction::KeepWaiting,
        }
    }
}

impl QuestionForm {
    /// As many options as a person can take in at once, and as a Codex
    /// choice presentation allows.
    pub const MAX_OPTIONS: usize = 16;

    /// Whether this form asks for anything beyond free text.
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }

    /// The form is sound for a question that expires `timeout_secs` after
    /// it is asked: every option label is distinct and trimmed, a choice
    /// needs options, the default answer is one of the options (or some
    /// words, for a free-text question), and the stale window, when there
    /// is one, is shorter than the timeout.
    pub fn validate(&self, timeout_secs: u64) -> Result<(), String> {
        let bounded = |s: &str| !s.trim().is_empty() && s.len() <= 16_000;
        if self.options.len() > Self::MAX_OPTIONS {
            return Err(format!("at most {} options", Self::MAX_OPTIONS));
        }
        let mut labels = std::collections::HashSet::new();
        for option in &self.options {
            if option.label != option.label.trim() || !bounded(&option.label) {
                return Err("an option label needs words, without surrounding space".to_owned());
            }
            if option.description.len() > 16_000 {
                return Err("an option description is too long".to_owned());
            }
            if !labels.insert(option.label.as_str()) {
                return Err(format!("option {:?} is offered twice", option.label));
            }
        }
        if self.multi_select && self.options.is_empty() {
            return Err("multi_select needs options to choose from".to_owned());
        }
        match self.stale_after_secs {
            Some(0) => return Err("stale_after_secs must be at least 1".to_owned()),
            Some(secs) if secs >= timeout_secs => {
                return Err("a question goes stale before it expires: stale_after_secs must be shorter than the timeout".to_owned());
            }
            None if self.stale_action != StaleAction::KeepWaiting => {
                return Err("a stale action needs stale_after_secs".to_owned());
            }
            _ => {}
        }
        if let StaleAction::AnswerDefault { label } = &self.stale_action {
            if self.options.is_empty() {
                if label != label.trim() || !bounded(label) {
                    return Err("the default answer needs words".to_owned());
                }
            } else if !labels.contains(label.as_str()) {
                return Err(format!(
                    "the default answer {label:?} is not one of the options"
                ));
            }
        }
        Ok(())
    }

    /// The options as a numbered list, with the Other line when the
    /// recipient may answer in their own words. What a terminal, the
    /// connector and a text-only reader show under the question; `None`
    /// without options.
    pub fn options_text(&self) -> Option<String> {
        if self.options.is_empty() {
            return None;
        }
        let mut text = String::from(if self.multi_select {
            "Choose any that apply, by number or label:"
        } else {
            "Choose one, by number or label:"
        });
        for (index, option) in self.options.iter().enumerate() {
            text.push_str(&format!("\n{}. {}", index + 1, option.label));
            if !option.description.trim().is_empty() {
                text.push_str(&format!(" — {}", option.description.trim()));
            }
        }
        if self.allow_other {
            text.push_str("\nOther: answer in your own words.");
        }
        Some(text)
    }

    /// The option `value` names: its 1-based number, or its label (case
    /// does not matter, surrounding space does not either).
    pub fn resolve_choice(&self, value: &str) -> Option<&QuestionOption> {
        let value = value.trim();
        if let Ok(number) = value.parse::<usize>()
            && number >= 1
            && let Some(option) = self.options.get(number - 1)
        {
            return Some(option);
        }
        self.options
            .iter()
            .find(|option| option.label.eq_ignore_ascii_case(value))
    }
}

/// An answer as it was given: the options chosen, by label, a note in the
/// answerer's words, and the text a reader that knows neither sees. The
/// daemon builds one from an `answer` request against the question it
/// answers, so a number becomes its label and a choice outside the options
/// is refused unless the question allows Other.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Nobody answered: the daemon applied the question's default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

impl QuestionAnswer {
    /// Resolve what was given against `form`. `choices` are labels or
    /// numbers; with none, `text` is read as one choice when the form has
    /// options, and as the answer itself otherwise.
    pub fn resolve(
        form: &QuestionForm,
        text: &str,
        choices: &[String],
        note: Option<&str>,
    ) -> Result<Self, String> {
        let note = note
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned);
        let text = text.trim();
        if form.options.is_empty() {
            if !choices.is_empty() {
                return Err("this question offers no options; answer in words".to_owned());
            }
            let text = match (text.is_empty(), &note) {
                (false, _) => text.to_owned(),
                (true, Some(note)) => note.clone(),
                (true, None) => return Err("an answer needs some text".to_owned()),
            };
            return Ok(Self {
                text,
                choices: Vec::new(),
                note,
                default: false,
            });
        }
        let given: Vec<&str> = if choices.is_empty() {
            if text.is_empty() {
                return Err("choose an option, or answer in words".to_owned());
            }
            vec![text]
        } else {
            choices.iter().map(|c| c.trim()).collect()
        };
        let mut resolved: Vec<String> = Vec::new();
        for value in given {
            let label = match form.resolve_choice(value) {
                Some(option) => option.label.clone(),
                None if form.allow_other && !value.is_empty() => value.to_owned(),
                None if value.is_empty() => return Err("an empty choice".to_owned()),
                None => {
                    return Err(format!(
                        "{value:?} is not one of the options; this question takes no other answer"
                    ));
                }
            };
            if !resolved.contains(&label) {
                resolved.push(label);
            }
        }
        if !form.multi_select && resolved.len() > 1 {
            return Err("this question takes one choice".to_owned());
        }
        let mut text = resolved.join(", ");
        if let Some(note) = &note {
            text.push_str(" — ");
            text.push_str(note);
        }
        Ok(Self {
            text,
            choices: resolved,
            note,
            default: false,
        })
    }

    /// The payload of the `answer` message that carries this.
    pub fn payload(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({"text": self.text}))
    }

    /// Read an answer back from a message payload; a payload with only
    /// text is a text-only answer.
    pub fn from_payload(payload: &serde_json::Value) -> Self {
        serde_json::from_value(payload.clone()).unwrap_or_else(|_| Self {
            text: payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| payload.to_string()),
            ..Self::default()
        })
    }
}

/// Which way an answer reached its asker. A synchronous `ask` waiting on
/// its connection is handed the answer and the queue never shows it; with
/// no `ask` waiting, or one that ended first, the queue delivers it. One
/// answer travels one way, never both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerRoute {
    ToolResult,
    Queue,
}

/// The answer that approves a Codex request and lets Codex do the same again
/// without asking until its session ends: Codex's own `acceptForSession`, or
/// a permission granted for the session rather than the turn. Offered only
/// where Codex offers it (`for_session`).
pub const ALLOW_FOR_SESSION: &str = "Allow for session";

/// Explicit controls for a human question. The fallback text must describe the
/// same choice, so native and terminal clients review the same request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuestionPresentation {
    McpForm {
        server: String,
        message: String,
        schema: serde_json::Value,
    },
    McpUrl {
        server: String,
        elicitation_id: String,
        message: String,
        url: String,
    },
    CodexCommand {
        command: String,
        cwd: String,
        reason: String,
        /// Codex offers to allow it for the rest of its session too.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        for_session: bool,
    },
    CodexFiles {
        cwd: String,
        reason: String,
        changes: Vec<QuestionFileChange>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        for_session: bool,
    },
    CodexPermissions {
        cwd: String,
        reason: String,
        permissions: QuestionPermissions,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        for_session: bool,
    },
    Choices {
        question: String,
        options: Vec<QuestionOption>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionFileChange {
    pub path: String,
    pub kind: QuestionFileChangeKind,
    pub diff: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuestionFileChangeKind {
    Add,
    Delete,
    Update {
        #[serde(default)]
        move_path: Option<String>,
    },
}

impl QuestionFileChange {
    pub fn label(&self) -> String {
        match &self.kind {
            QuestionFileChangeKind::Add => format!("Create {}", self.path),
            QuestionFileChangeKind::Delete => format!("Delete {}", self.path),
            QuestionFileChangeKind::Update { move_path: None } => format!("Update {}", self.path),
            QuestionFileChangeKind::Update {
                move_path: Some(to),
            } => format!("Move {} → {to}", self.path),
        }
    }
}

impl QuestionPresentation {
    pub fn text(&self) -> String {
        match self {
            Self::McpForm {
                server,
                message,
                schema,
            } => format!(
                "Provide information to {server}?\n\n{message}\n\nReview the fields before submitting. Do not enter passwords, API keys, access tokens or payment credentials. Form answers are retained in this conversation.\n\nSchema:\n{}\n\nUse the form in the desktop app, or reply with a JSON object containing the field values. Reply Decline or Cancel to dismiss without sharing values.",
                serde_json::to_string_pretty(schema).unwrap_or_default()
            ),
            Self::McpUrl {
                server,
                message,
                url,
                ..
            } => format!(
                "Continue on the website requested by {server}?\n\n{message}\n\nFull URL:\n{url}\n\nCopy the link and open it in your browser if you consent. Enter any private information only on that website. Accept records your consent; it does not confirm the website interaction finished. Reply Accept, Decline or Cancel."
            ),
            Self::CodexPermissions {
                cwd,
                reason,
                permissions,
                for_session,
            } => format!(
                "Allow Codex this access{}?\n\nDirectory: {cwd}\nReason: {reason}\n\n{}\n\n{}",
                if *for_session {
                    ""
                } else {
                    " for the current turn"
                },
                permissions.lines().join("\n"),
                reply(*for_session, "for the current turn")
            ),
            Self::CodexCommand {
                command,
                cwd,
                reason,
                for_session,
            } => format!(
                "Allow Codex to run this command{}?\n\nDirectory: {cwd}\nCommand:\n{command}\n\nReason: {reason}\n\n{}",
                if *for_session { "" } else { " once" },
                reply(*for_session, "once")
            ),
            Self::Choices { question, options } => {
                let mut text = question.clone();
                for option in options {
                    text.push_str(&format!("\n- {}: {}", option.label, option.description));
                }
                text
            }
            Self::CodexFiles {
                cwd,
                reason,
                changes,
                for_session,
            } => {
                let mut text = format!(
                    "Allow Codex to apply these file changes{}?\n\nDirectory: {cwd}\nReason: {reason}\n",
                    if *for_session { "" } else { " once" }
                );
                for change in changes {
                    text.push_str(&format!("\n{}\n{}\n", change.label(), change.diff));
                }
                text.push('\n');
                text.push_str(&reply(*for_session, "once"));
                text
            }
        }
    }

    pub fn valid_for(&self, text: &str) -> bool {
        let bounded = |s: &str| !s.trim().is_empty() && s.len() <= 16_000;
        let valid = match self {
            Self::McpForm {
                server,
                message,
                schema,
            } => {
                bounded(server)
                    && server.len() <= 256
                    && !server.chars().any(display_control)
                    && bounded(message)
                    && !message.chars().any(display_control)
                    && crate::McpForm::parse(schema).is_ok()
            }
            Self::McpUrl {
                server,
                elicitation_id,
                message,
                url,
            } => {
                let identifier =
                    |s: &str| bounded(s) && s.len() <= 256 && !s.chars().any(display_control);
                identifier(server)
                    && identifier(elicitation_id)
                    && bounded(message)
                    && !message.chars().any(display_control)
                    && mcp_url_authority(url).is_some()
            }
            Self::CodexPermissions {
                cwd,
                reason,
                permissions,
                ..
            } => {
                bounded(cwd)
                    && !cwd.chars().any(char::is_control)
                    && reason.len() <= 16_000
                    && permissions.valid()
            }
            Self::CodexCommand {
                command,
                cwd,
                reason,
                ..
            } => bounded(command) && bounded(cwd) && reason.len() <= 16_000,
            Self::Choices { question, options } => {
                let mut labels = std::collections::HashSet::new();
                bounded(question)
                    && !options.is_empty()
                    && options.len() <= 16
                    && options.iter().all(|o| {
                        o.label == o.label.trim()
                            && bounded(&o.label)
                            && o.description.len() <= 16_000
                            && labels.insert(&o.label)
                    })
            }
            Self::CodexFiles {
                cwd,
                reason,
                changes,
                ..
            } => {
                let path = |s: &str| bounded(s) && !s.chars().any(char::is_control);
                let mut paths = std::collections::HashSet::new();
                path(cwd)
                    && reason.len() <= 16_000
                    && !changes.is_empty()
                    && changes.len() <= 16
                    && changes.iter().all(|change| {
                        path(&change.path)
                            && paths.insert(&change.path)
                            && bounded(&change.diff)
                            && match &change.kind {
                                QuestionFileChangeKind::Update {
                                    move_path: Some(to),
                                } => path(to),
                                _ => true,
                            }
                    })
            }
        };
        valid && text.len() <= 16_000 && self.text() == text
    }

    pub fn permits_answer(&self, value: &str) -> bool {
        match self {
            Self::McpForm { schema, .. } => crate::McpForm::parse(schema)
                .ok()
                .and_then(|form| form.response(value))
                .is_some(),
            _ => self.permits_choice(value),
        }
    }

    pub fn permits_choice(&self, value: &str) -> bool {
        match self {
            Self::McpForm { .. } => matches!(value, "Decline" | "Cancel"),
            Self::McpUrl { .. } => matches!(value, "Accept" | "Decline" | "Cancel"),
            Self::CodexCommand { .. } | Self::CodexFiles { .. } | Self::CodexPermissions { .. } => {
                matches!(value, "Allow" | "Deny")
                    || (value == ALLOW_FOR_SESSION && self.for_session())
            }
            Self::Choices { options, .. } => options.iter().any(|o| o.label == value),
        }
    }

    /// Whether Codex offered to allow this for the rest of its session.
    pub fn for_session(&self) -> bool {
        match self {
            Self::CodexCommand { for_session, .. }
            | Self::CodexFiles { for_session, .. }
            | Self::CodexPermissions { for_session, .. } => *for_session,
            _ => false,
        }
    }
}

/// How a Codex approval says it is answered: Allow `once` or Deny, and
/// Allow for session where Codex offers it.
fn reply(for_session: bool, once: &str) -> String {
    if for_session {
        format!(
            "Reply Allow ({once}), {ALLOW_FOR_SESSION} (this and the same again without asking until this Codex session ends) or Deny."
        )
    } else {
        "Reply Allow or Deny.".to_owned()
    }
}

fn display_control(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// A deliberately narrow, printable web URL. No fetching, rewriting or decoding
/// takes place here; clients show/copy the original string. HTTP is limited to
/// canonical loopback addresses for local development. The authority parser is
/// shared with HTTP transports; additional checks exclude browser-normalized
/// userinfo, encoded hosts, legacy IP spellings and ambiguous DNS labels.
pub fn mcp_url_authority(value: &str) -> Option<&str> {
    if value.len() > 8_192
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b))
    {
        return None;
    }
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !bytes
                .get(index + 1..index + 3)
                .is_some_and(|pair| pair.iter().all(u8::is_ascii_hexdigit))
        {
            return None;
        }
    }
    let (scheme, rest) = value.split_once("://")?;
    if !matches!(scheme, "https" | "http") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains(['@', '%']) || authority.ends_with(':') {
        return None;
    }
    let parsed: http::uri::Authority = authority.parse().ok()?;
    let host = parsed.host();
    let port = authority.strip_prefix(host)?;
    if !port.is_empty()
        && !port.strip_prefix(':').is_some_and(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|port| port > 0)
        })
    {
        return None;
    }
    if let Some(ipv6) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        ipv6.parse::<std::net::Ipv6Addr>().ok()?;
    } else if host.rsplit('.').next().is_some_and(|last| {
        last.bytes().all(|b| b.is_ascii_digit())
            || last
                .to_ascii_lowercase()
                .strip_prefix("0x")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit()))
    }) {
        let ip: std::net::Ipv4Addr = host.parse().ok()?;
        if ip.to_string() != host {
            return None;
        }
    } else if host.len() > 253
        || !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return None;
    }
    if scheme == "http" && !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return None;
    }
    Some(authority)
}

impl Question {
    pub fn expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    pub fn kind(&self) -> QuestionKind {
        self.form.kind
    }

    /// When the stale window closes, if the question set one.
    pub fn stale_at(&self) -> Option<DateTime<Utc>> {
        self.form
            .stale_after_secs
            .and_then(|secs| i64::try_from(secs).ok())
            .map(|secs| self.asked_at + chrono::Duration::seconds(secs))
    }

    /// Whether the question has gone stale: marked so by the daemon, or
    /// past its window and not yet swept.
    pub fn stale(&self, now: DateTime<Utc>) -> bool {
        self.stale_since.is_some() || self.stale_at().is_some_and(|at| now >= at)
    }

    /// The question as a text-only reader sees it: the words, then the
    /// numbered options when there are any.
    pub fn prompt(&self) -> String {
        match self.form.options_text() {
            Some(options) => format!("{}\n\n{options}", self.text.trim_end()),
            None => self.text.clone(),
        }
    }

    /// Whether `value` is one of this question's offered choices: an
    /// option label, or a choice its presentation permits.
    pub fn permits_choice(&self, value: &str) -> bool {
        self.form.options.iter().any(|o| o.label == value)
            || self
                .presentation
                .as_ref()
                .is_some_and(|p| p.valid_for(&self.text) && p.permits_choice(value))
    }

    /// Whether this question was put to `agent` — directly, or as one of
    /// the recipients of a wider destination it belongs to. `project` is
    /// the one `agent` works in: a question put to a project reaches its
    /// agents.
    pub fn addressed_to(&self, agent: &AgentId, project: Option<&ProjectId>) -> bool {
        match &self.to {
            Destination::Agent(id) => id == agent,
            Destination::Broadcast => true,
            Destination::Project(to) => project == Some(to),
            _ => false,
        }
    }
}

/// MQTT-style topic matching: `+` matches exactly one level, `#` matches the
/// remainder (including nothing).
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    let mut pattern = pattern.split('/');
    let mut topic = topic.split('/');
    loop {
        match (pattern.next(), topic.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => continue,
            (Some(a), Some(b)) if a == b => continue,
            (None, None) => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A question put to a project reaches the agents working in it, and
    /// no agent elsewhere; one put to everyone reaches anyone.
    #[test]
    fn a_question_put_to_a_project_reaches_only_its_agents() {
        let now = Utc::now();
        let alpha = ProjectId::from("alpha");
        let question = |to| Question {
            id: MessageId::generate(),
            from: "asker".to_owned(),
            to,
            text: "?".to_owned(),
            presentation: None,
            asked_at: now,
            expires_at: now,
            form: Default::default(),
            stale_since: None,
        };
        let agent = AgentId::from("agent");
        let to_alpha = question(Destination::Project(alpha.clone()));
        assert!(to_alpha.addressed_to(&agent, Some(&alpha)));
        assert!(!to_alpha.addressed_to(&agent, Some(&ProjectId::from("beta"))));
        assert!(!to_alpha.addressed_to(&agent, None));
        assert!(question(Destination::Broadcast).addressed_to(&agent, None));
    }

    #[test]
    fn mcp_url_review_keeps_the_destination_and_only_three_decisions() {
        let presentation = QuestionPresentation::McpUrl {
            server: "example-tools".into(),
            elicitation_id: "opaque-id".into(),
            message: "Connect your example account.".into(),
            url: "https://accounts.example.com/consent?state=a%20b#confirm".into(),
        };
        assert!(presentation.valid_for(&presentation.text()));
        assert!(
            presentation
                .text()
                .contains("https://accounts.example.com/consent?state=a%20b#confirm")
        );
        assert!(!presentation.valid_for("A different website"));
        for answer in ["Accept", "Decline", "Cancel"] {
            assert!(presentation.permits_choice(answer));
        }
        for answer in ["accept", " Accept", "Allow", "a password"] {
            assert!(!presentation.permits_choice(answer));
        }
        let decoded: QuestionPresentation =
            serde_json::from_value(serde_json::to_value(&presentation).unwrap()).unwrap();
        assert_eq!(decoded, presentation);
    }

    #[test]
    fn mcp_urls_exclude_ambiguous_hosts_credentials_and_non_web_actions() {
        for url in [
            "https://example.com/path?scope=a%20b",
            "https://example.com:8443/#state",
            "https://[2001:db8::1]/",
            "http://127.0.0.1:1234/",
            "http://[::1]:1234/",
            "http://localhost:1234/",
            "https://xn--bcher-kva.example/",
        ] {
            assert!(mcp_url_authority(url).is_some(), "{url}");
        }
        for url in [
            "https:///path",
            "https://",
            "http://remote.example/",
            "http://127.1/",
            "https://127.1/",
            "https://0177.0.0.1/",
            "https://0x7f.0.0.1/",
            "https://0x7f000001/",
            "javascript:alert(1)",
            "file:///tmp/x",
            "https://user:pass@example.com/",
            "https://@example.com/",
            "https://good.example\\@bad.example/",
            "https://examp\nle.com/",
            " https://example.com/",
            "https://example.com/a b",
            "https://%65xample.com/",
            "https://example.com:999999/",
            "https://example.com:no/",
            "https://example.com:/",
            "https://example.com:0/",
            "https://-example.com/",
            "https://example..com/",
            "https://[nonsense]/",
            "https://bücher.example/",
            "https://example.com/%",
            "https://example.com/%0g",
            "https://example.com/\u{202e}abc",
            "https://example.com/\"onclick=bad",
        ] {
            assert!(mcp_url_authority(url).is_none(), "{url}");
        }
    }

    #[test]
    fn structured_questions_match_fallback_text_and_bound_unambiguous_choices() {
        let command = QuestionPresentation::CodexCommand {
            command: "printf hello".into(),
            cwd: "/owned".into(),
            reason: "Print the fixture token".into(),
            for_session: false,
        };
        assert!(command.valid_for(&command.text()));
        assert!(!command.valid_for("Run a different command"));
        assert!(command.permits_choice("Allow"));
        assert!(command.permits_choice("Deny"));
        assert!(!command.permits_choice("Allow for this session"));
        assert!(
            !command.permits_choice(ALLOW_FOR_SESSION),
            "only where Codex offers it"
        );
        assert!(command.text().contains("run this command once?"));
        // Where Codex offers it, the session is a third answer, and the
        // text names it as the buttons do.
        let session = QuestionPresentation::CodexCommand {
            command: "printf hello".into(),
            cwd: "/owned".into(),
            reason: "Print the fixture token".into(),
            for_session: true,
        };
        assert!(session.valid_for(&session.text()));
        assert!(!session.valid_for(&command.text()), "the text says which");
        assert!(session.permits_choice(ALLOW_FOR_SESSION));
        assert!(session.permits_choice("Allow") && session.permits_choice("Deny"));
        assert!(session.text().contains(ALLOW_FOR_SESSION));
        // Stored without the flag, as every question before it was, and
        // read back from that.
        let stored = serde_json::to_value(&command).unwrap();
        assert!(stored.get("for_session").is_none());
        assert_eq!(
            serde_json::from_value::<QuestionPresentation>(stored).unwrap(),
            command
        );
        assert_eq!(serde_json::to_value(&session).unwrap()["for_session"], true);
        let option = QuestionOption {
            label: "Blue".into(),
            description: "Use the blue theme".into(),
        };
        let choice = QuestionPresentation::Choices {
            question: "Which color?".into(),
            options: vec![option.clone()],
        };
        assert!(choice.valid_for(&choice.text()));
        assert!(choice.permits_choice("Blue"));
        assert!(!choice.permits_choice("Red"));
        let duplicate = QuestionPresentation::Choices {
            question: "Which color?".into(),
            options: vec![option.clone(), option],
        };
        assert!(!duplicate.valid_for(&duplicate.text()));
        for label in [" Blue", "Blue ", "Blue\t", "Blue\n", "Blue\u{00a0}"] {
            let ambiguous = QuestionPresentation::Choices {
                question: "Which color?".into(),
                options: vec![
                    QuestionOption {
                        label: "Blue".into(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: label.into(),
                        description: String::new(),
                    },
                ],
            };
            assert!(
                !ambiguous.valid_for(&ambiguous.text()),
                "surrounding whitespace must not create a second visible Blue choice"
            );
        }
        let huge = QuestionPresentation::CodexCommand {
            command: "x".repeat(16_001),
            cwd: "/owned".into(),
            reason: String::new(),
            for_session: false,
        };
        assert!(!huge.valid_for(&huge.text()));
    }

    #[test]
    fn topic_patterns() {
        assert!(topic_matches("a/b/c", "a/b/c"));
        assert!(!topic_matches("a/b/c", "a/b"));
        assert!(!topic_matches("a/b", "a/b/c"));
        assert!(topic_matches("a/+/c", "a/x/c"));
        assert!(!topic_matches("a/+/c", "a/x/y/c"));
        assert!(topic_matches("a/#", "a/x/y/c"));
        assert!(topic_matches("a/#", "a"));
        assert!(topic_matches("#", "anything/at/all"));
        assert!(!topic_matches("b/#", "a/b"));
    }

    #[test]
    fn destination_shorthand() {
        assert_eq!(Destination::parse("all"), Destination::Broadcast);
        assert_eq!(Destination::parse("*"), Destination::Broadcast);
        assert_eq!(
            Destination::parse("topic:repo/reviews"),
            Destination::Topic("repo/reviews".into())
        );
        assert_eq!(
            Destination::parse("reviewer"),
            Destination::Agent(AgentId::from("reviewer"))
        );
        assert_eq!(
            Destination::parse("project:/repo"),
            Destination::Project(ProjectId::from("/repo"))
        );
        assert_eq!(
            Destination::parse("project:3f9c").to_string(),
            "project:3f9c"
        );
    }

    #[test]
    fn destination_serialises_tagged() {
        let json = serde_json::to_string(&Destination::Topic("x".into())).unwrap();
        assert_eq!(json, r#"{"kind":"topic","value":"x"}"#);
        let json = serde_json::to_string(&Destination::Broadcast).unwrap();
        assert_eq!(json, r#"{"kind":"broadcast"}"#);
    }
}

#[cfg(test)]
mod form_tests {
    use super::*;

    fn option(label: &str, description: &str) -> QuestionOption {
        QuestionOption {
            label: label.into(),
            description: description.into(),
        }
    }

    fn colours() -> QuestionForm {
        QuestionForm {
            options: vec![option("Blue", "the current theme"), option("Green", "")],
            ..QuestionForm::default()
        }
    }

    fn question(form: QuestionForm) -> Question {
        let now = Utc::now();
        Question {
            id: MessageId::generate(),
            from: "asker".into(),
            to: Destination::Agent(AgentId::from("user")),
            text: "Which theme?".into(),
            presentation: None,
            asked_at: now,
            expires_at: now + chrono::Duration::seconds(300),
            form,
            stale_since: None,
        }
    }

    /// A question that says nothing beyond its words is on the wire what
    /// it always was, and one stored before forms existed reads back as a
    /// plain question that keeps waiting.
    #[test]
    fn a_plain_form_adds_nothing_to_the_wire_and_an_old_question_reads_back_plain() {
        let plain = question(QuestionForm::default());
        let value = serde_json::to_value(&plain).unwrap();
        for key in [
            "kind",
            "options",
            "multi_select",
            "allow_other",
            "stale_after_secs",
            "stale_action",
            "stale_since",
        ] {
            assert!(
                value.get(key).is_none(),
                "{key} must be absent on a plain question"
            );
        }
        // What a daemon stored before questions had a form: the same
        // seven fields, nothing else.
        assert_eq!(value.as_object().map(|o| o.len()), Some(6), "{value}");
        let read: Question = serde_json::from_value(value).unwrap();
        assert!(read.form.is_plain());
        assert!(read.form.allow_other);
        assert_eq!(read.kind(), QuestionKind::Question);
        assert_eq!(read.stale_at(), None);
        assert!(!read.stale(Utc::now()));
        assert_eq!(read.prompt(), "Which theme?");
    }

    /// The typed fields survive a round trip, flattened beside the text.
    #[test]
    fn a_typed_form_round_trips_flattened() {
        let mut form = colours();
        form.kind = QuestionKind::Permission;
        form.multi_select = true;
        form.allow_other = false;
        form.stale_after_secs = Some(60);
        form.stale_action = StaleAction::AnswerDefault {
            label: "Blue".into(),
        };
        let asked = question(form.clone());
        let value = serde_json::to_value(&asked).unwrap();
        assert_eq!(value["kind"], "permission");
        assert_eq!(value["options"][1]["label"], "Green");
        assert_eq!(value["multi_select"], true);
        assert_eq!(value["allow_other"], false);
        assert_eq!(value["stale_after_secs"], 60);
        assert_eq!(value["stale_action"]["action"], "answer_default");
        assert_eq!(value["stale_action"]["label"], "Blue");
        let read: Question = serde_json::from_value(value).unwrap();
        assert_eq!(read.form, form);
        assert_eq!(
            read.stale_at(),
            Some(asked.asked_at + chrono::Duration::seconds(60))
        );
        assert!(read.stale(asked.asked_at + chrono::Duration::seconds(60)));
        assert!(!read.stale(asked.asked_at + chrono::Duration::seconds(59)));
    }

    #[test]
    fn validation_holds_labels_choices_and_the_stale_window_to_their_rules() {
        assert_eq!(QuestionForm::default().validate(300), Ok(()));
        assert_eq!(colours().validate(300), Ok(()));
        let twice = QuestionForm {
            options: vec![option("Blue", ""), option("Blue", "")],
            ..QuestionForm::default()
        };
        assert!(twice.validate(300).is_err(), "a repeated label");
        let padded = QuestionForm {
            options: vec![option(" Blue", "")],
            ..QuestionForm::default()
        };
        assert!(padded.validate(300).is_err(), "surrounding space");
        let blank = QuestionForm {
            options: vec![option("", "")],
            ..QuestionForm::default()
        };
        assert!(blank.validate(300).is_err(), "a blank label");
        let many = QuestionForm {
            options: (0..=QuestionForm::MAX_OPTIONS)
                .map(|n| option(&n.to_string(), ""))
                .collect(),
            ..QuestionForm::default()
        };
        assert!(many.validate(300).is_err(), "too many options");
        let multi_without = QuestionForm {
            multi_select: true,
            ..QuestionForm::default()
        };
        assert!(
            multi_without.validate(300).is_err(),
            "multi_select needs options"
        );
        let zero = QuestionForm {
            stale_after_secs: Some(0),
            ..QuestionForm::default()
        };
        assert!(zero.validate(300).is_err(), "a zero window");
        let late = QuestionForm {
            stale_after_secs: Some(300),
            ..QuestionForm::default()
        };
        assert!(late.validate(300).is_err(), "stale must come before expiry");
        let early = QuestionForm {
            stale_after_secs: Some(299),
            ..QuestionForm::default()
        };
        assert_eq!(early.validate(300), Ok(()));
        let action_without_window = QuestionForm {
            stale_action: StaleAction::Withdraw,
            ..QuestionForm::default()
        };
        assert!(
            action_without_window.validate(300).is_err(),
            "an action needs a window"
        );
        let default_outside = QuestionForm {
            stale_after_secs: Some(10),
            stale_action: StaleAction::AnswerDefault {
                label: "Red".into(),
            },
            ..colours()
        };
        assert!(
            default_outside.validate(300).is_err(),
            "the default must be an option"
        );
        let default_inside = QuestionForm {
            stale_after_secs: Some(10),
            stale_action: StaleAction::AnswerDefault {
                label: "Green".into(),
            },
            ..colours()
        };
        assert_eq!(default_inside.validate(300), Ok(()));
        let free_default = QuestionForm {
            stale_after_secs: Some(10),
            stale_action: StaleAction::AnswerDefault {
                label: "carry on".into(),
            },
            ..QuestionForm::default()
        };
        assert_eq!(
            free_default.validate(300),
            Ok(()),
            "a free-text question may default to words"
        );
        let blank_default = QuestionForm {
            stale_after_secs: Some(10),
            stale_action: StaleAction::AnswerDefault { label: " ".into() },
            ..QuestionForm::default()
        };
        assert!(blank_default.validate(300).is_err());
    }

    /// The text a terminal shows numbers the options and offers Other
    /// exactly when the form does; the prompt puts it under the words.
    #[test]
    fn options_render_as_a_numbered_list_with_an_other_line() {
        assert_eq!(QuestionForm::default().options_text(), None);
        let asked = question(colours());
        assert_eq!(
            asked.prompt(),
            "Which theme?\n\nChoose one, by number or label:\n1. Blue — the current theme\n2. Green\nOther: answer in your own words."
        );
        let strict = question(QuestionForm {
            multi_select: true,
            allow_other: false,
            ..colours()
        });
        assert_eq!(
            strict.form.options_text().unwrap(),
            "Choose any that apply, by number or label:\n1. Blue — the current theme\n2. Green"
        );
    }

    #[test]
    fn a_choice_is_found_by_number_or_label() {
        let form = colours();
        assert_eq!(
            form.resolve_choice("1").map(|o| o.label.as_str()),
            Some("Blue")
        );
        assert_eq!(
            form.resolve_choice(" green ").map(|o| o.label.as_str()),
            Some("Green")
        );
        assert_eq!(form.resolve_choice("3"), None);
        assert_eq!(form.resolve_choice("0"), None);
        assert_eq!(form.resolve_choice("Red"), None);
        let asked = question(form);
        assert!(asked.permits_choice("Blue"));
        assert!(!asked.permits_choice("Red"));
    }

    #[test]
    fn an_answer_resolves_against_the_form() {
        let plain = QuestionForm::default();
        assert_eq!(
            QuestionAnswer::resolve(&plain, "yes", &[], None)
                .unwrap()
                .text,
            "yes"
        );
        assert!(QuestionAnswer::resolve(&plain, " ", &[], None).is_err());
        assert!(
            QuestionAnswer::resolve(&plain, "", &["1".into()], None).is_err(),
            "no options to choose from"
        );
        let colours = colours();
        let by_number = QuestionAnswer::resolve(&colours, "2", &[], None).unwrap();
        assert_eq!(by_number.choices, vec!["Green".to_owned()]);
        assert_eq!(by_number.text, "Green");
        let by_label =
            QuestionAnswer::resolve(&colours, "", &["blue".into()], Some(" please ")).unwrap();
        assert_eq!(by_label.choices, vec!["Blue".to_owned()]);
        assert_eq!(by_label.note.as_deref(), Some("please"));
        assert_eq!(by_label.text, "Blue — please");
        let other = QuestionAnswer::resolve(&colours, "Red", &[], None).unwrap();
        assert_eq!(
            other.choices,
            vec!["Red".to_owned()],
            "Other is allowed by default"
        );
        assert!(
            QuestionAnswer::resolve(&colours, "", &["1".into(), "2".into()], None).is_err(),
            "one choice unless multi_select"
        );
        let strict = QuestionForm {
            allow_other: false,
            multi_select: true,
            ..colours.clone()
        };
        assert!(QuestionAnswer::resolve(&strict, "Red", &[], None).is_err());
        let both =
            QuestionAnswer::resolve(&strict, "", &["2".into(), "Blue".into(), "2".into()], None)
                .unwrap();
        assert_eq!(
            both.choices,
            vec!["Green".to_owned(), "Blue".to_owned()],
            "deduplicated, in order"
        );
        assert_eq!(both.text, "Green, Blue");
        assert!(QuestionAnswer::resolve(&strict, "", &[" ".into()], None).is_err());
        let payload = both.payload();
        assert_eq!(QuestionAnswer::from_payload(&payload), both);
        let bare = QuestionAnswer::from_payload(&serde_json::json!({"text": "ok"}));
        assert_eq!(bare.text, "ok");
        assert!(bare.choices.is_empty() && bare.note.is_none() && !bare.default);
    }

    #[test]
    fn kinds_and_stale_actions_parse_their_one_line_spellings() {
        assert_eq!(
            QuestionKind::parse(" Permission "),
            Some(QuestionKind::Permission)
        );
        assert_eq!(QuestionKind::parse("access"), Some(QuestionKind::Access));
        assert_eq!(QuestionKind::parse("prompt"), None);
        assert_eq!(QuestionKind::Access.label(), "Access");
        assert_eq!(QuestionKind::Access.to_string(), "access");
        assert_eq!(
            StaleAction::parse("keep_waiting"),
            Some(StaleAction::KeepWaiting)
        );
        assert_eq!(StaleAction::parse("Withdraw"), Some(StaleAction::Withdraw));
        assert_eq!(
            StaleAction::parse("answer: Deny"),
            Some(StaleAction::AnswerDefault {
                label: "Deny".into()
            })
        );
        assert_eq!(StaleAction::parse("answer:"), None);
        assert_eq!(StaleAction::parse("drop"), None);
        assert_eq!(
            StaleAction::AnswerDefault {
                label: "Deny".into()
            }
            .to_string(),
            "answer:Deny"
        );
    }
}
