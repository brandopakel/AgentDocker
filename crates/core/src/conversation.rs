//! Conversations: what people read, on top of the queues agents consume.
//!
//! A queue holds what one recipient has not yet taken; a conversation is
//! every message ever said in one place, bounded by retention, for a
//! reader who wants the room rather than the pile. Every archived message
//! belongs to exactly one conversation, decided by its destination, and a
//! reader keeps one cursor per conversation. Nothing here changes what an
//! agent is delivered.
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::channel::ChannelId;
use crate::{AgentId, Destination, Envelope, ProjectId};

/// The daemon's own name as a sender.
pub const DAEMON: &str = "agentd";

/// One place people read: `everyone:<project>`, `all`, `channel:<id>`,
/// `dm:<a>:<b>` (the sorted pair of ids, whoever the parties are) or
/// `notices:<agent>` (what the daemon itself told one agent). Topics are
/// streams, not conversations, and have none.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConversationId(String);

impl ConversationId {
    pub fn everyone(project: &ProjectId) -> Self {
        Self(format!("everyone:{}", project.as_str()))
    }

    pub fn all() -> Self {
        Self("all".to_owned())
    }

    pub fn channel(channel: &ChannelId) -> Self {
        Self(format!("channel:{channel}"))
    }

    /// The pair is unordered: `dm(a, b)` and `dm(b, a)` are one conversation.
    pub fn dm(a: &str, b: &str) -> Self {
        let (first, second) = if a <= b { (a, b) } else { (b, a) };
        Self(format!("dm:{first}:{second}"))
    }

    pub fn notices(agent: &AgentId) -> Self {
        Self(format!("notices:{}", agent.as_str()))
    }

    /// The conversation a message belongs to, or none for a topic.
    pub fn of(envelope: &Envelope) -> Option<Self> {
        Some(match &envelope.to {
            Destination::Agent(to) if envelope.from == DAEMON => Self::notices(to),
            Destination::Agent(to) => Self::dm(&envelope.from, to.as_str()),
            Destination::Project(project) => Self::everyone(project),
            Destination::Channel(channel) => Self::channel(channel),
            Destination::Broadcast => Self::all(),
            Destination::Topic(_) => return None,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn kind(&self) -> Option<ConversationKind> {
        let (head, _) = self.0.split_once(':').unwrap_or((&self.0, ""));
        Some(match head {
            "everyone" => ConversationKind::Everyone,
            "all" => ConversationKind::All,
            "channel" => ConversationKind::Channel,
            "dm" => ConversationKind::Dm,
            "notices" => ConversationKind::Notices,
            _ => return None,
        })
    }

    /// The channel a `channel:` conversation names.
    pub fn channel_id(&self) -> Option<ChannelId> {
        self.0
            .strip_prefix("channel:")
            .map(|id| ChannelId::from(id.to_owned()))
    }

    /// The two parties of a `dm:` conversation.
    pub fn dm_parties(&self) -> Option<(&str, &str)> {
        self.0.strip_prefix("dm:")?.split_once(':')
    }

    /// The agent a `notices:` conversation is addressed to.
    pub fn notices_agent(&self) -> Option<AgentId> {
        self.0
            .strip_prefix("notices:")
            .map(|id| AgentId::from(id.to_owned()))
    }

    /// The project an `everyone:` conversation is the broadcast of.
    pub fn everyone_project(&self) -> Option<ProjectId> {
        self.0.strip_prefix("everyone:").map(ProjectId::from)
    }

    /// Whether `who` is one of the parties, for a direct conversation, or
    /// the addressee of a notices conversation. Rooms and broadcasts answer
    /// by membership, which the daemon knows and this type does not.
    pub fn is_party(&self, who: &str) -> bool {
        match self.dm_parties() {
            Some((a, b)) => a == who || b == who,
            None => self.notices_agent().is_some_and(|id| id.as_str() == who),
        }
    }
}

impl From<String> for ConversationId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ConversationId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Display for ConversationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    /// A project's broadcast, `#everyone`.
    Everyone,
    /// Every project at once, `Destination::Broadcast`.
    All,
    /// A channel somebody or the daemon opened.
    Channel,
    /// A channel the daemon opened because two checkouts changed one path.
    Collision,
    /// Two parties.
    Dm,
    /// What the daemon itself told one agent.
    Notices,
}

/// One row of the archive: the message, where it belongs, and its place
/// in the order everything was said.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedMessage {
    pub seq: u64,
    pub conversation: ConversationId,
    #[serde(flatten)]
    pub envelope: Envelope,
    /// How many replies this message has as a thread root, when asked for.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub replies: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// What a reader sees of one conversation in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub conversation: ConversationId,
    pub kind: ConversationKind,
    /// The `#name` of a named channel, `everyone` or `all`; none for a
    /// direct conversation, a collision room or notices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What to show: the channel's task or paths, the other party's name,
    /// or the daemon's.
    pub title: String,
    pub members: Vec<AgentId>,
    /// Archived messages past the reader's cursor.
    pub unread: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_line: Option<String>,
}

/// Where a reader has read to in one conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadCursor {
    pub reader: AgentId,
    pub conversation: ConversationId,
    pub through: u64,
    pub updated_at: DateTime<Utc>,
}

/// The text of a message as a line: the payload's `text`, or the JSON.
pub fn line_of(envelope: &Envelope) -> String {
    match envelope.payload.get("text").and_then(|t| t.as_str()) {
        Some(text) => text.to_owned(),
        None => envelope.payload.to_string(),
    }
}

/// Whether `reply_to` threads under a root in the same conversation. A
/// reply whose root is elsewhere is a plain message that keeps the id as
/// data; a reply whose root is gone shows as such.
pub fn threads_under(reply: &ArchivedMessage, root: Option<&ArchivedMessage>) -> bool {
    match (&reply.envelope.reply_to, root) {
        (Some(id), Some(root)) => {
            root.envelope.id == *id && root.conversation == reply.conversation
        }
        _ => false,
    }
}

/// A channel name: lowercase letters, digits and hyphens, at most 40
/// characters, neither empty nor starting or ending with a hyphen.
pub fn valid_channel_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The name a task gets when nobody chose one: its words, lowercased and
/// hyphenated, cut to fit.
pub fn channel_name_from(task: &str) -> Option<String> {
    let mut name = String::new();
    for word in task
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let word = word.to_ascii_lowercase();
        if name.len() + word.len() + usize::from(!name.is_empty()) > 40 {
            break;
        }
        if !name.is_empty() {
            name.push('-');
        }
        name.push_str(&word);
    }
    valid_channel_name(&name).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(from: &str, to: Destination) -> Envelope {
        Envelope::new(
            from,
            to,
            "chat",
            json!({ "text": "hello" }),
            None,
            Utc::now(),
        )
    }

    #[test]
    fn every_destination_but_a_topic_names_one_conversation() {
        let a = AgentId::from("aaa".to_owned());
        let b = AgentId::from("bbb".to_owned());
        let project = ProjectId::from("ppp");
        let channel = ChannelId::from("ccc".to_owned());
        assert_eq!(
            ConversationId::of(&envelope("bbb", Destination::Agent(a.clone()))),
            Some(ConversationId::from("dm:aaa:bbb"))
        );
        assert_eq!(
            ConversationId::of(&envelope("aaa", Destination::Agent(b.clone()))),
            Some(ConversationId::from("dm:aaa:bbb")),
            "one conversation whichever way it goes"
        );
        assert_eq!(
            ConversationId::of(&envelope(DAEMON, Destination::Agent(a.clone()))),
            Some(ConversationId::from("notices:aaa"))
        );
        assert_eq!(
            ConversationId::of(&envelope("aaa", Destination::Project(project.clone()))),
            Some(ConversationId::from("everyone:ppp"))
        );
        assert_eq!(
            ConversationId::of(&envelope("aaa", Destination::Channel(channel.clone()))),
            Some(ConversationId::from("channel:ccc"))
        );
        assert_eq!(
            ConversationId::of(&envelope("aaa", Destination::Broadcast)),
            Some(ConversationId::all())
        );
        assert_eq!(
            ConversationId::of(&envelope("aaa", Destination::Topic("t/x".into()))),
            None
        );
        let dm = ConversationId::dm("bbb", "aaa");
        assert_eq!(dm.kind(), Some(ConversationKind::Dm));
        assert_eq!(dm.dm_parties(), Some(("aaa", "bbb")));
        assert!(dm.is_party("aaa") && dm.is_party("bbb") && !dm.is_party("ccc"));
        assert_eq!(
            ConversationId::channel(&channel).channel_id(),
            Some(channel)
        );
        assert_eq!(ConversationId::notices(&a).notices_agent(), Some(a.clone()));
        assert!(ConversationId::notices(&a).is_party("aaa"));
        assert_eq!(
            ConversationId::everyone(&project).everyone_project(),
            Some(project)
        );
        assert_eq!(ConversationId::from("what:ever").kind(), None);
    }

    #[test]
    fn a_reply_threads_only_under_a_root_in_its_own_conversation() {
        let root = ArchivedMessage {
            seq: 1,
            conversation: ConversationId::from("everyone:p"),
            envelope: envelope("aaa", Destination::Project(ProjectId::from("p"))),
            replies: 0,
        };
        let mut reply = ArchivedMessage {
            seq: 2,
            conversation: ConversationId::from("everyone:p"),
            envelope: envelope("bbb", Destination::Project(ProjectId::from("p"))),
            replies: 0,
        };
        reply.envelope.reply_to = Some(root.envelope.id.clone());
        assert!(threads_under(&reply, Some(&root)));
        assert!(
            !threads_under(&reply, None),
            "a pruned root threads nothing"
        );
        let mut elsewhere = root.clone();
        elsewhere.conversation = ConversationId::from("channel:c");
        assert!(
            !threads_under(&reply, Some(&elsewhere)),
            "a root in another conversation is not this thread"
        );
        reply.envelope.reply_to = None;
        assert!(!threads_under(&reply, Some(&root)));
    }

    #[test]
    fn channel_names_are_slugs_and_come_from_tasks_when_nobody_chose() {
        assert!(valid_channel_name("planning"));
        assert!(valid_channel_name("release-2"));
        assert!(!valid_channel_name(""));
        assert!(!valid_channel_name("Planning"));
        assert!(!valid_channel_name("-x"));
        assert!(!valid_channel_name("x-"));
        assert!(!valid_channel_name("a b"));
        assert!(!valid_channel_name(&"a".repeat(41)));
        assert_eq!(
            channel_name_from("Settle the parser, then ship!"),
            Some("settle-the-parser-then-ship".into())
        );
        assert_eq!(
            channel_name_from(&"word ".repeat(20)).map(|n| n.len() <= 40),
            Some(true)
        );
        assert_eq!(channel_name_from("!!!"), None);
    }

    #[test]
    fn a_line_is_the_text_or_the_json() {
        let e = envelope("aaa", Destination::Broadcast);
        assert_eq!(line_of(&e), "hello");
        let raw = Envelope::new(
            "aaa",
            Destination::Broadcast,
            "chat",
            json!({ "n": 1 }),
            None,
            Utc::now(),
        );
        assert_eq!(line_of(&raw), "{\"n\":1}");
    }
}
