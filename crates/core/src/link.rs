//! A typed reference beside a card, a message or a hand-off: what kind
//! of thing it points at, where that is, and a word about it. The journal
//! note and the commit attribution are the data; the link is the
//! affordance — a reader, human or agent, knows what to open without
//! parsing prose. The daemon checks the shape of a link and nothing
//! more: whether a path exists or a pull request is open is the reader's
//! to find out.
use serde::{Deserialize, Serialize};

/// How many links one card, message or hand-off carries at most.
pub const LINKS: usize = 16;
/// The longest target: a URL, a path, or the text of a memory note.
pub const TARGET_CHARS: usize = 2_048;
/// The longest word about a link.
pub const NOTE_CHARS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// A file or directory, absolute or relative to the checkout.
    Path,
    /// A commit, by its hash (seven to sixty-four hex digits).
    Commit,
    /// A pull request: a URL, `#123`, or `owner/repo#123`.
    Pr,
    /// Any `http` or `https` address.
    Url,
    /// A card on the board, by id or a unique prefix of it.
    Task,
    /// An archived message, by id.
    Message,
    /// A memory for the next agent: the target is the text itself.
    Memory,
}

impl LinkKind {
    pub const ALL: [LinkKind; 7] = [
        LinkKind::Path,
        LinkKind::Commit,
        LinkKind::Pr,
        LinkKind::Url,
        LinkKind::Task,
        LinkKind::Message,
        LinkKind::Memory,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            LinkKind::Path => "path",
            LinkKind::Commit => "commit",
            LinkKind::Pr => "pr",
            LinkKind::Url => "url",
            LinkKind::Task => "task",
            LinkKind::Message => "message",
            LinkKind::Memory => "memory",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text.trim().to_ascii_lowercase())
    }
}

impl std::fmt::Display for LinkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Link {
    pub kind: LinkKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Link {
    pub fn new(kind: LinkKind, target: impl Into<String>) -> Self {
        Self {
            kind,
            target: target.into(),
            note: None,
        }
    }

    /// `kind:target`, as the command line writes a link; a `memory` link
    /// is the rest of the text after the colon.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let (kind, target) = text
            .split_once(':')
            .ok_or("a link is kind:target — path, commit, pr, url, task, message or memory")?;
        let kind = LinkKind::parse(kind)
            .ok_or("a link's kind is path, commit, pr, url, task, message or memory")?;
        let link = Link::new(kind, target.trim());
        link.check()?;
        Ok(link)
    }

    /// The shape a link of its kind must have.
    pub fn check(&self) -> Result<(), &'static str> {
        let target = self.target.trim();
        if target.is_empty() {
            return Err("a link needs a target");
        }
        if target.chars().count() > TARGET_CHARS || target.contains(['\0', '\n', '\r']) {
            return Err("a link's target is one line of at most 2,048 characters");
        }
        if self
            .note
            .as_ref()
            .is_some_and(|note| note.chars().count() > NOTE_CHARS || note.contains(['\0', '\n']))
        {
            return Err("a link's note is one line of at most 200 characters");
        }
        let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let ok = match self.kind {
            LinkKind::Path => !target.contains(['\t']),
            LinkKind::Commit => (7..=64).contains(&target.len()) && hex(target),
            LinkKind::Pr => {
                target.starts_with("https://")
                    || target.starts_with("http://")
                    || target.strip_prefix('#').is_some_and(digits)
                    || target
                        .split_once('#')
                        .is_some_and(|(repo, number)| repo.contains('/') && digits(number))
            }
            LinkKind::Url => {
                (target.starts_with("https://") || target.starts_with("http://"))
                    && !target.contains(char::is_whitespace)
            }
            LinkKind::Task => (4..=12).contains(&target.len()) && hex(target),
            LinkKind::Message => (8..=32).contains(&target.len()) && hex(target),
            LinkKind::Memory => true,
        };
        if ok {
            Ok(())
        } else {
            Err(match self.kind {
                LinkKind::Path => "a path link has no tabs",
                LinkKind::Commit => "a commit link is seven to sixty-four hex digits",
                LinkKind::Pr => "a pr link is a URL, #123 or owner/repo#123",
                LinkKind::Url => "a url link starts with http:// or https:// and has no spaces",
                LinkKind::Task => {
                    "a task link is a card id or a prefix of at least four hex digits"
                }
                LinkKind::Message => "a message link is a message id",
                LinkKind::Memory => "a memory link is its text",
            })
        }
    }
}

impl std::fmt::Display for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind, self.target)
    }
}

/// Every link well-formed and not too many of them.
pub fn check(links: &[Link]) -> Result<(), &'static str> {
    if links.len() > LINKS {
        return Err("at most sixteen links");
    }
    links.iter().try_for_each(Link::check)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each kind has a shape; the command line's `kind:target` reads into
    /// it; the bounds hold.
    #[test]
    fn links_have_a_kind_and_a_shape() {
        for good in [
            "path:crates/core/src/link.rs",
            "path:/abs/dir",
            "commit:3fb678f",
            "pr:https://github.com/brandopakel/AgentDocker/pull/176",
            "pr:#176",
            "pr:brandopakel/AgentDocker#176",
            "url:https://paprika.ai/",
            "task:5fd54785dd77",
            "task:5fd5",
            "message:26beba01be15456f",
            "memory:the parser keeps a tail; read reader.rs before touching quarantine",
            "MEMORY:case does not matter for the kind",
        ] {
            assert!(Link::parse(good).is_ok(), "{good}: {:?}", Link::parse(good));
        }
        for bad in [
            "nokind",
            "sticker:x",
            "path:",
            "commit:xyz",
            "commit:12345",
            "pr:176",
            "url:ftp://x",
            "url:https://x y",
            "task:zz",
            "message:short",
        ] {
            assert!(Link::parse(bad).is_err(), "{bad}");
        }
        let long = format!("url:https://x/{}", "a".repeat(TARGET_CHARS));
        assert!(Link::parse(&long).is_err());
        let mut noted = Link::parse("pr:#1").unwrap();
        noted.note = Some("n".repeat(NOTE_CHARS + 1));
        assert!(noted.check().is_err());
        assert!(check(&vec![Link::parse("pr:#1").unwrap(); LINKS]).is_ok());
        assert!(check(&vec![Link::parse("pr:#1").unwrap(); LINKS + 1]).is_err());
        assert_eq!(
            Link::parse("commit:3fb678f").unwrap().to_string(),
            "commit:3fb678f"
        );
    }
}
