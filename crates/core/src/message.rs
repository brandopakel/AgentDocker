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
    /// Every live agent.
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
    },
    CodexFiles {
        cwd: String,
        reason: String,
        changes: Vec<QuestionFileChange>,
    },
    CodexPermissions {
        cwd: String,
        reason: String,
        permissions: QuestionPermissions,
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
            } => format!(
                "Allow Codex this access for the current turn?\n\nDirectory: {cwd}\nReason: {reason}\n\n{}\n\nReply Allow or Deny.",
                permissions.lines().join("\n")
            ),
            Self::CodexCommand {
                command,
                cwd,
                reason,
            } => format!(
                "Allow Codex to run this command once?\n\nDirectory: {cwd}\nCommand:\n{command}\n\nReason: {reason}\n\nReply Allow or Deny."
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
            } => {
                let mut text = format!(
                    "Allow Codex to apply these file changes once?\n\nDirectory: {cwd}\nReason: {reason}\n"
                );
                for change in changes {
                    text.push_str(&format!("\n{}\n{}\n", change.label(), change.diff));
                }
                text.push_str("\nReply Allow or Deny.");
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
            }
            Self::Choices { options, .. } => options.iter().any(|o| o.label == value),
        }
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

    /// Whether this question was put to `agent` — directly, or as one of
    /// the recipients of a wider destination it belongs to.
    pub fn addressed_to(&self, agent: &AgentId) -> bool {
        match &self.to {
            Destination::Agent(id) => id == agent,
            Destination::Broadcast => true,
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
        };
        assert!(command.valid_for(&command.text()));
        assert!(!command.valid_for("Run a different command"));
        assert!(command.permits_choice("Allow"));
        assert!(command.permits_choice("Deny"));
        assert!(!command.permits_choice("Allow for this session"));
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
