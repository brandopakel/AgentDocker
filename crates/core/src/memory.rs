//! A project's memory: short facts its agents and the person keep for each
//! other, kept on this machine. A memory is added on purpose, never taken
//! from a journal, a message or a transcript, so nothing reaches it that
//! nobody chose to keep. One that is wrong is superseded by a newer one,
//! or forgotten for good.
//!
//! The shape follows Supermemory's (add, search, profile, forget, scoped to
//! one project), so a mirror to it can come later without a second model.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Link, ProjectId};

/// Characters a memory holds at most: a fact, not a document.
pub const MEMORY_CHARS: usize = 2048;
/// Memories a project keeps at most.
pub const MEMORIES_PER_PROJECT: usize = 1000;
/// Links a memory carries at most.
pub const MEMORY_LINKS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub project: ProjectId,
    /// The agent or person that kept it.
    pub author: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// The memory this one replaces: that one is kept, but no longer said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Memory {
    pub fn new(
        project: ProjectId,
        author: &str,
        text: &str,
        links: Vec<Link>,
        supersedes: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, &'static str> {
        let memory = Self {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
            project,
            author: author.to_owned(),
            text: text.trim().to_owned(),
            links,
            supersedes,
            created_at: now,
        };
        memory.check()?;
        Ok(memory)
    }

    pub fn check(&self) -> Result<(), &'static str> {
        if self.text.is_empty() || self.text.chars().count() > MEMORY_CHARS {
            return Err("a memory is one to 2048 characters");
        }
        if self.links.len() > MEMORY_LINKS {
            return Err("a memory carries at most 8 links");
        }
        for link in &self.links {
            link.check()?;
        }
        if secret_like(&self.text) {
            return Err(
                "that reads like a secret (a key, a token, a private key); a memory is kept in plain text, so keep secrets out of it",
            );
        }
        Ok(())
    }
}

/// Whether `text` holds what looks like a credential: a private key, a
/// well-known token shape, or a long run of mixed letters and digits.
/// A guard, not a guarantee: it catches the common shapes only.
pub fn secret_like(text: &str) -> bool {
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return true;
    }
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')'))
        .any(|word| {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let token = |prefix: &str, min: usize| {
                word.strip_prefix(prefix).is_some_and(|rest| {
                    rest.len() >= min
                        && rest
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                })
            };
            token("sk-", 20)
                || token("sk_live_", 16)
                || token("ghp_", 30)
                || token("gho_", 30)
                || token("github_pat_", 30)
                || token("xoxb-", 20)
                || token("xoxp-", 20)
                || token("glpat-", 20)
                || (word.len() == 20
                    && word.starts_with("AKIA")
                    && word
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
                || high_entropy(word)
        })
}

/// A run of 32 or more letters and digits that mixes upper case, lower
/// case and digits, as generated keys do and words never do.
fn high_entropy(word: &str) -> bool {
    word.len() >= 32
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '='))
        && word.chars().any(|c| c.is_ascii_uppercase())
        && word.chars().any(|c| c.is_ascii_lowercase())
        && word.chars().filter(|c| c.is_ascii_digit()).count() >= 4
}

/// The memories still said: none another memory supersedes.
pub fn current(memories: &[Memory]) -> Vec<&Memory> {
    let replaced: std::collections::HashSet<&str> = memories
        .iter()
        .filter_map(|m| m.supersedes.as_deref())
        .collect();
    let mut kept: Vec<&Memory> = memories
        .iter()
        .filter(|m| !replaced.contains(m.id.as_str()))
        .collect();
    kept.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    kept
}

/// The current memories that answer `query`, the best first: each word of
/// the query a memory holds counts, a newer memory wins a tie. An empty
/// query answers the newest.
pub fn search<'a>(memories: &'a [Memory], query: &str, limit: usize) -> Vec<&'a Memory> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    let mut scored: Vec<(usize, &Memory)> = current(memories)
        .into_iter()
        .map(|memory| {
            let text = memory.text.to_lowercase();
            (
                words.iter().filter(|w| text.contains(w.as_str())).count(),
                memory,
            )
        })
        .filter(|(score, _)| words.is_empty() || *score > 0)
        .collect();
    // `current` is newest first and the sort is stable: ties stay newest first.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().take(limit).map(|(_, m)| m).collect()
}

/// What a project's agents should know first: its current memories,
/// newest first, as many as fit in `max_chars`.
pub fn profile(memories: &[Memory], max_chars: usize) -> Vec<&Memory> {
    let mut used = 0;
    current(memories)
        .into_iter()
        .take_while(|memory| {
            used += memory.text.chars().count() + 1;
            used <= max_chars
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn memory(id: &str, text: &str, minutes: i64, supersedes: Option<&str>) -> Memory {
        Memory {
            id: id.into(),
            project: ProjectId::from("p"),
            author: "a".into(),
            text: text.into(),
            links: vec![],
            supersedes: supersedes.map(str::to_owned),
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap()
                + Duration::minutes(minutes),
        }
    }

    #[test]
    fn a_memory_is_a_short_fact_and_never_a_secret() {
        let now = Utc::now();
        let project = ProjectId::from("p");
        assert!(
            Memory::new(
                project.clone(),
                "a",
                "  Tests run with nextest  ",
                vec![],
                None,
                now
            )
            .is_ok()
        );
        assert!(Memory::new(project.clone(), "a", "   ", vec![], None, now).is_err());
        assert!(Memory::new(project.clone(), "a", &"x".repeat(2049), vec![], None, now).is_err());
        for secret in [
            "the key is sk-abcdefghijklmnopqrstuvwx",
            "token ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "AKIAABCDEFGHIJKLMNOP is the access key",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc",
            "x9Fk2LmQ7vR3tY8wZ1aB4cD6eG0hJ5nP",
        ] {
            assert!(
                Memory::new(project.clone(), "a", secret, vec![], None, now).is_err(),
                "{secret}"
            );
        }
        assert!(!secret_like(
            "Use the reviewer role for every PR; Codex reviews, Claude implements"
        ));
        assert!(!secret_like(
            "the build cache lives in target/debug and is 4 GiB"
        ));
    }

    #[test]
    fn a_superseded_memory_is_kept_but_no_longer_said() {
        let all = vec![
            memory("old", "deploys go out on Fridays", 0, None),
            memory("new", "deploys go out on Mondays", 5, Some("old")),
            memory("other", "the reviewer is Codex", 3, None),
        ];
        let ids: Vec<&str> = current(&all).iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["new", "other"], "newest first, the replaced one gone");
    }

    #[test]
    fn search_ranks_by_words_held_then_by_recency() {
        let all = vec![
            memory("a", "the reviewer reviews every PR", 0, None),
            memory("b", "the release reviewer signs the build", 5, None),
            memory("c", "nothing about it", 9, None),
        ];
        let ids =
            |q: &str| -> Vec<String> { search(&all, q, 10).iter().map(|m| m.id.clone()).collect() };
        assert_eq!(ids("release reviewer"), ["b", "a"]);
        assert_eq!(ids("REVIEWER"), ["b", "a"], "case is not a difference");
        assert_eq!(ids("absent"), Vec::<String>::new());
        assert_eq!(ids(""), ["c", "b", "a"], "no words, the newest");
        assert_eq!(search(&all, "", 1).len(), 1);
    }

    #[test]
    fn a_profile_is_the_newest_that_fit() {
        let all = vec![
            memory("a", &"a".repeat(10), 0, None),
            memory("b", &"b".repeat(10), 1, None),
            memory("c", &"c".repeat(10), 2, None),
        ];
        let ids: Vec<&str> = profile(&all, 22).iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["c", "b"]);
        assert!(profile(&all, 5).is_empty());
    }
}
