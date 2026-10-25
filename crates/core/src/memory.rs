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
/// Roles a memory names at most.
pub const MEMORY_ROLES: usize = 8;

/// Who reads a project's memory. The person reads all of it; anybody else
/// reads the memories that name no role and those that name a role it
/// holds, so one holding none (a copy of the memory elsewhere, a client
/// that names nobody) reads only what is for everyone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader<'a> {
    Person,
    Holding(&'a [String]),
}

impl Reader<'_> {
    /// A reader holding no role: what is for everyone.
    pub const EVERYONE: Reader<'static> = Reader::Holding(&[]);
}

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
    /// Said first, before the recent ones, for as long as it stands: a
    /// convention or a decision rather than news.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub standing: bool,
    /// After this moment it is no longer said, as if forgotten; it is kept
    /// until somebody forgets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forget_after: Option<DateTime<Utc>>,
    /// Said only to the agents holding one of these roles, and to the
    /// person; to everyone when it names none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// When its fact became true, where that is before it was kept: a
    /// decision taken last week and written down today. Absent, it became
    /// true when it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<DateTime<Utc>>,
}

/// What replaced a superseded memory, as a reader sees it: the memory that
/// supersedes it and when that one became true.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replaced {
    pub by: String,
    pub at: DateTime<Utc>,
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
            standing: false,
            forget_after: None,
            roles: Vec::new(),
            valid_from: None,
        };
        memory.check()?;
        Ok(memory)
    }

    /// Said first in the profile while it stands.
    pub fn standing(mut self, standing: bool) -> Self {
        self.standing = standing;
        self
    }

    /// No longer said after `when`, which must come after it was kept.
    pub fn forget_after(mut self, when: Option<DateTime<Utc>>) -> Result<Self, &'static str> {
        if when.is_some_and(|when| when <= self.created_at) {
            return Err("a memory is forgotten after it is kept, not before");
        }
        self.forget_after = when;
        Ok(self)
    }

    /// Said only to the holders of these roles, and to the person.
    pub fn roles(mut self, roles: Vec<String>) -> Result<Self, &'static str> {
        self.roles = roles;
        self.roles.sort();
        self.roles.dedup();
        self.check()?;
        Ok(self)
    }

    /// True since `when`, which may come before it is kept, never after.
    pub fn valid_from(mut self, when: Option<DateTime<Utc>>) -> Result<Self, &'static str> {
        if when.is_some_and(|when| when > self.created_at) {
            return Err("a memory's fact became true before it was kept, or when, not after");
        }
        self.valid_from = when.filter(|when| *when != self.created_at);
        Ok(self)
    }

    /// When its fact became true: its `valid_from`, else when it was kept.
    pub fn true_since(&self) -> DateTime<Utc> {
        self.valid_from.unwrap_or(self.created_at)
    }

    /// Whether it is still said at `now`: not past its `forget_after`.
    pub fn said_at(&self, now: DateTime<Utc>) -> bool {
        self.forget_after.is_none_or(|when| now < when)
    }

    /// Whether `reader` may read it: the person always; anybody else when
    /// it names no role or one the reader holds.
    pub fn seen_by(&self, reader: Reader<'_>) -> bool {
        match reader {
            Reader::Person => true,
            Reader::Holding(held) => {
                self.roles.is_empty() || self.roles.iter().any(|role| held.contains(role))
            }
        }
    }

    pub fn check(&self) -> Result<(), &'static str> {
        if self.text.is_empty() || self.text.chars().count() > MEMORY_CHARS {
            return Err("a memory is one to 2048 characters");
        }
        if self.links.len() > MEMORY_LINKS {
            return Err("a memory carries at most 8 links");
        }
        if self.roles.len() > MEMORY_ROLES {
            return Err("a memory names at most 8 roles");
        }
        for role in &self.roles {
            crate::agent::check_role(role)?;
        }
        for link in &self.links {
            link.check()?;
        }
        // Every word that is kept is checked, a link's too.
        if std::iter::once(self.text.as_str())
            .chain(
                self.links.iter().flat_map(|link| {
                    std::iter::once(link.target.as_str()).chain(link.note.as_deref())
                }),
            )
            .any(secret_like)
        {
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
    // Words as written, so a base64 key with `+ / =` is one; and the same
    // text cut at every character a token never holds, so one inside
    // `token=…`, a URL or a path is a word of its own.
    let written = text.split(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')')
    });
    let cut = text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')));
    written.chain(cut).any(|word| {
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

/// The memories said to `reader` at `at` (now, or a moment asked about):
/// those it may read and true by then, none superseded by one it may read
/// that was true by then, none past its `forget_after` by then; newest
/// first. Supersession is the reader's: a memory replaced by one only
/// another role's holders read is still said to everybody else.
pub fn current<'a>(
    memories: &'a [Memory],
    reader: Reader<'_>,
    at: DateTime<Utc>,
) -> Vec<&'a Memory> {
    let replaced = superseded(memories, reader, at);
    let mut kept: Vec<&Memory> = known(memories, reader, at)
        .filter(|m| !replaced.contains_key(m.id.as_str()) && m.said_at(at))
        .collect();
    kept.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    kept
}

/// The memories `reader` may read whose facts were true by `at`.
fn known<'a, 'r>(
    memories: &'a [Memory],
    reader: Reader<'r>,
    at: DateTime<Utc>,
) -> impl Iterator<Item = &'a Memory> + use<'a, 'r> {
    memories
        .iter()
        .filter(move |m| m.seen_by(reader) && m.true_since() <= at)
}

/// The memories superseded for `reader` at `at`, each with the one that
/// replaced it: the latest such, by when it became true.
pub fn superseded<'a>(
    memories: &'a [Memory],
    reader: Reader<'_>,
    at: DateTime<Utc>,
) -> std::collections::BTreeMap<&'a str, Replaced> {
    let mut replaced: std::collections::BTreeMap<&str, Replaced> = Default::default();
    for newer in known(memories, reader, at) {
        if let Some(old) = newer.supersedes.as_deref() {
            let by = Replaced {
                by: newer.id.clone(),
                at: newer.true_since(),
            };
            replaced
                .entry(old)
                .and_modify(|kept| {
                    if (by.at, &by.by) > (kept.at, &kept.by) {
                        *kept = by.clone();
                    }
                })
                .or_insert(by);
        }
    }
    replaced
}

/// The words of a text as search reads them: lower case, split at
/// anything but a letter, a digit or `_`, so an identifier such as
/// `task_id` is one word, and `latest` holds no `test`.
fn words(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The current memories `reader` may read that answer `query`, the best
/// first. A memory
/// answers a word it holds whole (`test` is not in `latest`), and a word
/// few memories hold counts for more than one most of them hold; a newer
/// memory wins a tie, and between two kept at the same moment the lower
/// id comes first, so equal evidence always comes back in one order. An
/// empty query answers the newest.
pub fn search<'a>(
    memories: &'a [Memory],
    query: &str,
    limit: usize,
    reader: Reader<'_>,
    at: DateTime<Utc>,
) -> Vec<&'a Memory> {
    rank(current(memories, reader, at), query, limit)
}

/// [`search`] over the history as well: the superseded memories that
/// answer too, each with what replaced it, so what was once held true is
/// read without picking a moment. Those past their `forget_after` stay
/// out.
pub fn search_history<'a>(
    memories: &'a [Memory],
    query: &str,
    limit: usize,
    reader: Reader<'_>,
    at: DateTime<Utc>,
) -> (
    Vec<&'a Memory>,
    std::collections::BTreeMap<String, Replaced>,
) {
    let mut pool: Vec<&Memory> = known(memories, reader, at)
        .filter(|m| m.said_at(at))
        .collect();
    pool.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    let found = rank(pool, query, limit);
    let replaced = superseded(memories, reader, at);
    let marks = found
        .iter()
        .filter_map(|m| Some((m.id.clone(), replaced.get(m.id.as_str())?.clone())))
        .collect();
    (found, marks)
}

/// The memories of `said` that answer `query`, the best first, as
/// [`search`] ranks them.
fn rank<'a>(said: Vec<&'a Memory>, query: &str, limit: usize) -> Vec<&'a Memory> {
    let asked = words(query);
    let held: Vec<std::collections::HashSet<String>> =
        said.iter().map(|m| words(&m.text)).collect();
    let total = said.len() as f64;
    // Inverse document frequency, as BM25 has it: never negative, highest
    // for a word one memory holds.
    let weight = |word: &String| {
        let holding = held.iter().filter(|words| words.contains(word)).count() as f64;
        (1.0 + (total - holding + 0.5) / (holding + 0.5)).ln()
    };
    let weights: Vec<(&String, f64)> = asked.iter().map(|w| (w, weight(w))).collect();
    let mut scored: Vec<(f64, &Memory)> = said
        .into_iter()
        .zip(&held)
        .map(|(memory, words)| {
            // Summed in one fixed order, so equal evidence is an equal
            // score and the newer memory wins the tie.
            let mut held: Vec<f64> = weights
                .iter()
                .filter(|(word, _)| words.contains(*word))
                .map(|(_, weight)| *weight)
                .collect();
            held.sort_by(f64::total_cmp);
            (held.into_iter().sum::<f64>(), memory)
        })
        .filter(|(score, _)| asked.is_empty() || *score > 0.0)
        .collect();
    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(b.1.created_at.cmp(&a.1.created_at))
            .then(a.1.id.cmp(&b.1.id))
    });
    scored.into_iter().take(limit).map(|(_, m)| m).collect()
}

/// What `reader` should know first of a project's memory, as many as fit
/// in `max_chars`: its standing memories, the oldest first, then the
/// others, newest first.
pub fn profile<'a>(
    memories: &'a [Memory],
    max_chars: usize,
    reader: Reader<'_>,
    at: DateTime<Utc>,
) -> Vec<&'a Memory> {
    let said = current(memories, reader, at);
    let (mut standing, recent): (Vec<&Memory>, Vec<&Memory>) =
        said.into_iter().partition(|m| m.standing);
    // Oldest first; two kept at the same moment by id, as everywhere else.
    standing.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let mut used = 0;
    standing
        .into_iter()
        .chain(recent)
        // One too long for what is left is passed over, not the end: the
        // shorter ones after it still fit.
        .filter(|memory| {
            let size = memory.text.chars().count() + 1;
            let fits = used + size <= max_chars;
            if fits {
                used += size;
            }
            fits
        })
        .collect()
}

/// What a copy of a project's memory elsewhere must do to hold exactly its
/// current memories for everyone, given the ids it holds: the memories to
/// add, and the ids to take out — forgotten, past their `forget_after`,
/// superseded by a newer one, or said only to some roles, which never
/// leave the machine. A memory never changes once kept, so its id says
/// everything.
pub fn mirror_plan<'a>(
    memories: &'a [Memory],
    mirrored: &std::collections::BTreeSet<String>,
    now: DateTime<Utc>,
) -> (Vec<&'a Memory>, Vec<String>) {
    let said = current(memories, Reader::EVERYONE, now);
    let wanted: std::collections::BTreeSet<&str> = said.iter().map(|m| m.id.as_str()).collect();
    let add = said
        .into_iter()
        .filter(|m| !mirrored.contains(&m.id))
        .collect();
    let remove = mirrored
        .iter()
        .filter(|id| !wanted.contains(id.as_str()))
        .cloned()
        .collect();
    (add, remove)
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
            standing: false,
            forget_after: None,
            roles: vec![],
            valid_from: None,
        }
    }

    /// A memory naming roles is said to their holders and to the person,
    /// one naming none to everyone. Supersession is each reader's own: a
    /// memory replaced by one only reviewers read is still said to everybody
    /// else, and a copy elsewhere holds only what is for everyone.
    #[test]
    fn a_memory_with_roles_is_said_to_their_holders_and_the_person() {
        let roles = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let mut for_reviewers = memory("rev", "review against the full diff", 2, None);
        for_reviewers.roles = roles(&["reviewer"]);
        let mut replaced_for_reviewers =
            memory("new", "the build runs with nextest", 3, Some("old"));
        replaced_for_reviewers.roles = roles(&["reviewer"]);
        let all = vec![
            memory("old", "the build runs with cargo test", 1, None),
            for_reviewers,
            replaced_for_reviewers,
        ];
        let ids = |reader: Reader<'_>| {
            let mut ids: Vec<&str> = current(&all, reader, now())
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            ids.sort();
            ids
        };
        let reviewer = roles(&["reviewer"]);
        let tester = roles(&["tester"]);
        assert_eq!(ids(Reader::Holding(&reviewer)), ["new", "rev"]);
        assert_eq!(
            ids(Reader::Holding(&tester)),
            ["old"],
            "the old one stands for them"
        );
        assert_eq!(ids(Reader::EVERYONE), ["old"]);
        assert_eq!(ids(Reader::Person), ["new", "rev"]);
        assert!(search(&all, "diff", 10, Reader::Holding(&tester), now()).is_empty());
        assert_eq!(
            search(&all, "diff", 10, Reader::Holding(&reviewer), now())[0].id,
            "rev"
        );
        assert_eq!(
            profile(&all, 4000, Reader::Holding(&tester), now()).len(),
            1
        );
        let (add, _) = mirror_plan(&all, &Default::default(), now());
        assert_eq!(
            add.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["old"]
        );

        let kept = Memory::new(ProjectId::from("p"), "a", "fact", vec![], None, now())
            .unwrap()
            .roles(roles(&["tester", "reviewer", "tester"]))
            .unwrap();
        assert_eq!(kept.roles, ["reviewer", "tester"], "sorted, once each");
        assert!(
            Memory::new(ProjectId::from("p"), "a", "fact", vec![], None, now())
                .unwrap()
                .roles(roles(&["not a role!"]))
                .is_err()
        );
    }

    /// Read as of a moment, a fact replaced over time is the one that held
    /// then: before anything was true nothing is said, then the first fact,
    /// then its replacement. A backdated memory counts from when it became
    /// true, and one past its `forget_after` by then is not said. A history
    /// search lists the replaced one too, with what replaced it and when.
    #[test]
    fn memory_reads_as_it_stood_at_a_moment() {
        let at = |minutes: i64| {
            DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minutes)
        };
        let first = memory("first", "releases go out on fridays", 10, None);
        let mut second = memory("second", "releases go out on mondays", 30, Some("first"));
        // Decided at 20, written down at 30.
        second.valid_from = Some(at(20));
        let mut brief = memory("brief", "the freeze lasts a week", 12, None);
        brief.forget_after = Some(at(40));
        let all = vec![first, second, brief];
        let ids = |moment| {
            let mut ids: Vec<&str> = current(&all, Reader::Person, moment)
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            ids.sort();
            ids
        };
        assert!(ids(at(5)).is_empty(), "before anything was true");
        assert_eq!(ids(at(15)), ["brief", "first"]);
        assert_eq!(ids(at(25)), ["brief", "second"], "true from 20, kept at 30");
        assert_eq!(ids(at(50)), ["second"], "the brief one forgotten at 40");
        assert_eq!(
            search(&all, "releases", 10, Reader::Person, at(15))[0].id,
            "first"
        );
        let (found, replaced) = search_history(&all, "releases", 10, Reader::Person, at(50));
        let mut found: Vec<&str> = found.iter().map(|m| m.id.as_str()).collect();
        found.sort();
        assert_eq!(found, ["first", "second"]);
        assert_eq!(
            replaced.get("first"),
            Some(&Replaced {
                by: "second".into(),
                at: at(20)
            })
        );
        assert!(!replaced.contains_key("second"));
        // A memory is true from when it was kept, or before; never after.
        let kept = Memory::new(ProjectId::from("p"), "a", "fact", vec![], None, at(30)).unwrap();
        assert!(kept.clone().valid_from(Some(at(31))).is_err());
        assert_eq!(
            kept.clone().valid_from(Some(at(30))).unwrap().valid_from,
            None
        );
        assert_eq!(kept.valid_from(Some(at(1))).unwrap().true_since(), at(1));
    }

    /// A moment after every memory these tests keep.
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::days(1)
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
    fn a_secret_inside_an_assignment_a_url_or_a_link_is_refused() {
        let now = Utc::now();
        let project = ProjectId::from("p");
        for text in [
            "token=ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "see https://example.invalid/?token=ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "path /keys/sk-abcdefghijklmnopqrstuvwx/x",
        ] {
            assert!(
                Memory::new(project.clone(), "a", text, vec![], None, now).is_err(),
                "{text}"
            );
        }
        let mut link = Link::new(crate::LinkKind::Memory, "sk-abcdefghijklmnopqrstuvwx");
        assert!(
            Memory::new(project.clone(), "a", "fine", vec![link.clone()], None, now).is_err(),
            "a link's target"
        );
        link.target = "notes".into();
        link.note = Some("ghp_abcdefghijklmnopqrstuvwxyz0123456789".into());
        assert!(
            Memory::new(project.clone(), "a", "fine", vec![link], None, now).is_err(),
            "a link's note"
        );
        assert!(!secret_like(
            "set RUST_LOG=debug and see https://docs.rs/tokio"
        ));
    }

    #[test]
    fn a_superseded_memory_is_kept_but_no_longer_said() {
        let all = vec![
            memory("old", "deploys go out on Fridays", 0, None),
            memory("new", "deploys go out on Mondays", 5, Some("old")),
            memory("other", "the reviewer is Codex", 3, None),
        ];
        let ids: Vec<&str> = current(&all, Reader::Person, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["new", "other"], "newest first, the replaced one gone");
    }

    #[test]
    fn search_ranks_by_words_held_then_by_recency() {
        let all = vec![
            memory("a", "the reviewer reviews every PR", 0, None),
            memory("b", "the release reviewer signs the build", 5, None),
            memory("c", "nothing about it", 9, None),
        ];
        let ids = |q: &str| -> Vec<String> {
            search(&all, q, 10, Reader::Person, now())
                .iter()
                .map(|m| m.id.clone())
                .collect()
        };
        assert_eq!(ids("release reviewer"), ["b", "a"]);
        assert_eq!(ids("REVIEWER"), ["b", "a"], "case is not a difference");
        assert_eq!(ids("absent"), Vec::<String>::new());
        assert_eq!(ids(""), ["c", "b", "a"], "no words, the newest");
        assert_eq!(search(&all, "", 1, Reader::Person, now()).len(), 1);
    }

    #[test]
    fn a_mirror_adds_what_it_lacks_and_drops_what_is_no_longer_said() {
        let all = vec![
            memory("old", "deploys go out on Fridays", 0, None),
            memory("new", "deploys go out on Mondays", 5, Some("old")),
            memory("kept", "the reviewer is a role", 3, None),
        ];
        let mirrored: std::collections::BTreeSet<String> =
            ["old", "kept", "forgotten"].map(str::to_owned).into();
        let (add, remove) = mirror_plan(&all, &mirrored, now());
        let added: Vec<&str> = add.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(added, ["new"]);
        assert_eq!(remove, ["forgotten", "old"]);
        let (add, remove) = mirror_plan(&all, &["new", "kept"].map(str::to_owned).into(), now());
        assert!(add.is_empty() && remove.is_empty(), "in step");
    }

    #[test]
    fn a_profile_is_the_newest_that_fit() {
        let all = vec![
            memory("a", &"a".repeat(10), 0, None),
            memory("b", &"b".repeat(10), 1, None),
            memory("c", &"c".repeat(10), 2, None),
        ];
        let ids: Vec<&str> = profile(&all, 22, Reader::Person, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["c", "b"]);
        assert!(profile(&all, 5, Reader::Person, now()).is_empty());
        let mut long = all.clone();
        long.push(memory("big", &"z".repeat(900), 3, None));
        let ids: Vec<&str> = profile(&long, 22, Reader::Person, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["c", "b"],
            "a long newest one is passed over, not the end"
        );
    }

    #[test]
    fn search_matches_whole_words_and_weighs_the_rare_ones() {
        let all = vec![
            memory("both", "alpha beta", 0, None),
            memory("rare", "gamma beta", 1, None),
            memory("older", "alpha", 2, None),
            memory("newer", "alpha", 3, None),
            memory("latest", "the latest build", 4, None),
        ];
        let ids = |q: &str| -> Vec<String> {
            search(&all, q, 10, Reader::Person, now())
                .iter()
                .map(|m| m.id.clone())
                .collect()
        };
        assert_eq!(
            ids("alpha beta"),
            ["both", "rare", "newer", "older"],
            "beta, which fewer hold, outweighs alpha; a tie goes to the newer"
        );
        assert_eq!(ids("test"), Vec::<String>::new(), "not inside another word");
        assert_eq!(ids("latest"), ["latest"]);
        assert_eq!(
            ids("Beta, ALPHA!"),
            ids("alpha beta"),
            "case and marks aside"
        );
    }

    #[test]
    fn equal_evidence_is_an_equal_score_and_the_newer_wins() {
        let all = vec![
            memory("older", "a b common", 0, None),
            memory("newer", "c d common", 1, None),
            memory("x", "unrelated one", 2, None),
            memory("y", "unrelated two", 3, None),
        ];
        // Whatever order the query's words are held in, the sums match.
        for query in ["a b c d common", "common d c b a", "b common a d c"] {
            for _ in 0..20 {
                let ids: Vec<&str> = search(&all, query, 10, Reader::Person, now())
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect();
                assert_eq!(ids, ["newer", "older"], "{query}");
            }
        }
    }

    #[test]
    fn standing_memories_come_first_the_oldest_of_them_first() {
        let mut all = vec![
            memory("rule", "reviews go through the gate", 0, None),
            memory("news", "the build is slow today", 1, None),
            memory("later rule", "commits carry no trailers", 2, None),
            memory("latest", "the cache was cleared", 3, None),
        ];
        all[0].standing = true;
        all[2].standing = true;
        let ids: Vec<&str> = profile(&all, 4000, Reader::Person, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["rule", "later rule", "latest", "news"]);
        let ids: Vec<&str> = profile(&all, 30, Reader::Person, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["rule"], "the standing ones claim the room first");
    }

    #[test]
    fn a_word_is_cut_at_marks_but_an_identifier_stays_whole() {
        let all = vec![
            memory("field", "the task_id field names the card", 0, None),
            memory("cargo", "hold task:local-cargo-campaign first", 1, None),
            memory("tests", "testing happens in CI", 2, None),
            memory("accent", "L'ÉTÉ est long", 3, None),
        ];
        let ids = |q: &str| -> Vec<String> {
            search(&all, q, 10, Reader::Person, now())
                .iter()
                .map(|m| m.id.clone())
                .collect()
        };
        assert_eq!(ids("task"), ["cargo"], "`:` and `-` cut, `_` does not");
        assert_eq!(ids("id"), Vec::<String>::new(), "not inside `task_id`");
        assert_eq!(ids("task_id"), ["field"], "an identifier is one word");
        assert_eq!(ids("TASK_ID"), ["field"]);
        assert_eq!(ids("campaign"), ["cargo"]);
        assert_eq!(ids("test"), Vec::<String>::new(), "not inside `testing`");
        assert_eq!(ids("ask"), Vec::<String>::new(), "not inside `task`");
        assert_eq!(ids("été"), ["accent"], "case folds beyond ASCII");
    }

    #[test]
    fn equal_scores_kept_at_the_same_moment_go_by_id() {
        let same = |id: &str, text: &str| memory(id, text, 7, None);
        let forward = vec![
            same("b2", "deploy on monday"),
            same("a1", "deploy on monday"),
            same("c3", "deploy on monday"),
            memory("newer", "deploy on friday", 8, None),
            memory("other", "unrelated", 0, None),
        ];
        let mut backward = forward.clone();
        backward.reverse();
        for all in [&forward, &backward] {
            let ids: Vec<&str> = search(all, "deploy", 10, Reader::Person, now())
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(ids, ["newer", "a1", "b2", "c3"], "newer, then by id");
            let ids: Vec<&str> = search(all, "monday deploy", 10, Reader::Person, now())
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(ids, ["a1", "b2", "c3", "newer"], "score, then by id");
        }
    }

    #[test]
    fn a_profile_says_standing_then_recent_in_one_order_whatever_was_stored() {
        let mut all = vec![
            memory("s-b", "standing kept with another", 2, None),
            memory("r-old", "older news", 1, None),
            memory("s-a", "standing kept with another", 2, None),
            memory("s-first", "the first convention", 0, None),
            memory("r-b", "news at one moment", 5, None),
            memory("r-a", "news at one moment", 5, None),
        ];
        for m in all.iter_mut().filter(|m| m.id.starts_with("s-")) {
            m.standing = true;
        }
        let mut backward = all.clone();
        backward.reverse();
        for stored in [&all, &backward] {
            let ids: Vec<&str> = profile(stored, 4000, Reader::Person, now())
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            assert_eq!(ids, ["s-first", "s-a", "s-b", "r-a", "r-b", "r-old"]);
        }
    }

    #[test]
    fn a_memory_past_its_forget_after_is_no_longer_said() {
        let mut all = vec![
            memory("kept", "the reviewer is a role", 0, None),
            memory("brief", "the main branch is frozen", 1, None),
        ];
        all[1].forget_after = Some(now() - Duration::minutes(1));
        let said: Vec<&str> = current(&all, Reader::Person, now())
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(said, ["kept"]);
        assert!(search(&all, "frozen", 10, Reader::Person, now()).is_empty());
        assert_eq!(profile(&all, 4000, Reader::Person, now()).len(), 1);
        let (_, remove) = mirror_plan(&all, &["kept", "brief"].map(str::to_owned).into(), now());
        assert_eq!(remove, ["brief"], "a mirror lets it go too");
        let before = now() - Duration::minutes(2);
        assert_eq!(
            current(&all, Reader::Person, before).len(),
            2,
            "said until then"
        );

        let kept = Memory::new(ProjectId::from("p"), "a", "frozen", vec![], None, now()).unwrap();
        assert!(
            kept.clone().forget_after(Some(now())).is_err(),
            "not when it is kept"
        );
        let later = kept.forget_after(Some(now() + Duration::hours(1))).unwrap();
        assert!(later.said_at(now()) && !later.said_at(now() + Duration::hours(1)));
    }
}
