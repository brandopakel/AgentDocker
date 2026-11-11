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
/// well-known token shape, a bearer token or a JWT, a value given to a
/// name that says it is secret, or a long run of mixed letters and
/// digits. A guard, not a guarantee: it catches the common shapes only.
///
/// The token shapes beyond the first few follow the redaction list of
/// past.dev's MIT-licensed agent plugins (Copyright (c) Past Corp), which
/// redact where this refuses.
pub fn secret_like(text: &str) -> bool {
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return true;
    }
    if bearer_token(text) || secret_assignment(text) {
        return true;
    }
    // A JWT keeps its dots, so it is cut out only at what a JWT never
    // holds: from `session=…`, a URL's query or a sentence's last word.
    if text
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .any(|word| jwt(word.trim_matches('.')))
    {
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
            || ["ghp_", "gho_", "ghu_", "ghs_", "github_pat_"]
                .iter()
                .any(|prefix| token(prefix, 30))
            || ["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-", "glpat-"]
                .iter()
                .any(|prefix| token(prefix, 20))
            || (word.len() == 20
                && (word.starts_with("AKIA") || word.starts_with("ASIA"))
                && word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
            || jwt(word)
            || high_entropy(word)
    })
}

/// A JSON web token: three base64url parts, the first a JSON header
/// (`eyJ…`).
fn jwt(word: &str) -> bool {
    word.starts_with("eyJ")
        && word.len() >= 30
        && word.matches('.').count() == 2
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// `Bearer <token>`, as an authorization header carries one, whatever
/// wraps it: a header name with no space (`Authorization:Bearer …`), a
/// JSON field's quotes, a sentence's brackets. A token is generated: it
/// holds a digit, or upper and lower case together, so a placeholder
/// (`Bearer <token>`, `Bearer ${TOKEN}`, `Bearer YOUR_ACCESS_TOKEN_HERE`)
/// is not one.
fn bearer_token(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.windows(2).any(|pair| {
        // The scheme is the word's last run of letters, so what comes
        // before it (`Authorization:`, a quote, a bracket) is no matter.
        let scheme = pair[0]
            .rsplit(|c: char| !c.is_ascii_alphabetic())
            .next()
            .unwrap_or("");
        let token = pair[1].trim_matches(|c| {
            matches!(
                c,
                '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        });
        scheme.eq_ignore_ascii_case("bearer")
            && token.len() >= 20
            && token.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '~' | '+' | '/' | '=')
            })
            && (token.chars().any(|c| c.is_ascii_digit())
                || (token.chars().any(|c| c.is_ascii_uppercase())
                    && token.chars().any(|c| c.is_ascii_lowercase())))
    })
}

/// A value given to a name that says it is secret (`DB_PASSWORD=…`,
/// `api_key: …`, `"api_key": "…"`, `{"DB_PASSWORD":"…"}`, in brackets or
/// not), when the value looks like one: a placeholder, a word of prose
/// after the name ("TOKEN: see the docs", "TOKEN: generation.") or a value
/// that only says where the secret is ([`locator`]) is not.
fn secret_assignment(text: &str) -> bool {
    const SECRET_NAMES: [&str; 7] = [
        "SECRET",
        "PASSWORD",
        "TOKEN",
        "API_KEY",
        "APIKEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
    ];
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(at, b)| {
        if !matches!(b, b'=' | b':') {
            return false;
        }
        // The name: the identifier right before, spaces allowed between,
        // and the quote that closes a JSON or YAML key (`"api_key":`).
        let before = text[..at].trim_end();
        let before = before.strip_suffix(['"', '\'', '`']).unwrap_or(before);
        let name: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let upper = name.to_ascii_uppercase();
        if name.is_empty() || !SECRET_NAMES.iter().any(|secret| upper.contains(secret)) {
            return false;
        }
        // The value: the next word, past the quote or bracket that opens
        // it and up to the one that closes it, without the full stop or
        // question mark that ends a sentence. A `<placeholder>` or a
        // `${VARIABLE}` keeps its brackets, so it is never one.
        let value = text[at + 1..]
            .trim_start()
            .trim_start_matches(['"', '\'', '`', '(', '['])
            .split(|c: char| {
                c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'' | '`' | ')' | ']' | '}')
            })
            .next()
            .unwrap_or("")
            .trim_end_matches(['.', '?']);
        !locator(&upper, value) && token_like(value)
    })
}

/// Whether an assignment's value only says where a secret is: a path
/// given to a name for a file or a directory
/// (`TOKEN_FILE=/run/secrets/ci_token`), or a CI secret or variable named
/// by reference (`token: secrets.RELEASE_TOKEN`). Only the assignment rule
/// reads a value so; a secret written into one is still found by the
/// token, JWT and entropy checks over the whole text.
fn locator(name: &str, value: &str) -> bool {
    let file_name = ["_FILE", "_PATH", "_DIR"]
        .iter()
        .any(|end| name.ends_with(end));
    let path = ["/", "~/", "./", "../"]
        .iter()
        .any(|start| value.starts_with(start));
    let reference = ["secrets.", "vars.", "env."].iter().any(|context| {
        value.strip_prefix(context).is_some_and(|named| {
            named.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && named.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    });
    (file_name && path) || reference
}

/// A value as a generated secret or a password is written: at least eight
/// characters of no space or bracket, holding a digit, or upper and lower
/// case letters together, or a symbol. A plain word, a `<placeholder>` or
/// a `${VARIABLE}` is not.
fn token_like(value: &str) -> bool {
    value.chars().count() >= 8
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '_' | '-'
                        | '+'
                        | '/'
                        | '='
                        | '.'
                        | '!'
                        | '@'
                        | '#'
                        | '%'
                        | '^'
                        | '&'
                        | '*'
                        | '~'
                )
        })
        && (value.chars().any(|c| c.is_ascii_digit())
            || (value.chars().any(|c| c.is_ascii_uppercase())
                && value.chars().any(|c| c.is_ascii_lowercase()))
            || value
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-'))
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
    rank(current(memories, reader, at), query, limit, 0.0)
}

/// [`search`], keeping only the memories whose words score at least
/// `min_score`: a word few memories hold counts for more than one most of
/// them hold, so a threshold keeps out a memory that shares only common
/// words with the query.
pub fn search_above<'a>(
    memories: &'a [Memory],
    query: &str,
    limit: usize,
    min_score: f64,
    reader: Reader<'_>,
    at: DateTime<Utc>,
) -> Vec<&'a Memory> {
    rank(current(memories, reader, at), query, limit, min_score)
}

/// [`search_above`] over the history as well: the superseded memories
/// that answer too, each with what replaced it, so what was once held true
/// is read without picking a moment. Those past their `forget_after` stay
/// out.
pub fn search_history<'a>(
    memories: &'a [Memory],
    query: &str,
    limit: usize,
    min_score: f64,
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
    let found = rank(pool, query, limit, min_score);
    let replaced = superseded(memories, reader, at);
    let marks = found
        .iter()
        .filter_map(|m| Some((m.id.clone(), replaced.get(m.id.as_str())?.clone())))
        .collect();
    (found, marks)
}

/// What came of asking which memories a new one may replace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suggestion {
    /// Asked, or nothing was alike enough to ask about.
    Made,
    /// The `memories` judgment is off.
    Off,
    /// The judge could not be asked or did not answer.
    Unavailable,
    /// The judge's calls for this minute are spent.
    OverBudget,
}

/// A memory a new one may replace, and how likely the judge thinks that is.
/// Only a suggestion: the author supersedes on purpose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaySupersede {
    pub memory: String,
    pub probability: f64,
}

/// The current memories `new` may be compared with, the most alike first,
/// at most `limit`: those sharing a word with it that its author (`reader`)
/// may read and every reader of `new` may too, so an answer shows nobody a
/// memory they could not read. One `new` already replaces is left out.
pub fn comparable<'a>(
    memories: &'a [Memory],
    new: &Memory,
    limit: usize,
    reader: Reader<'_>,
    at: DateTime<Utc>,
) -> Vec<&'a Memory> {
    // A memory with no words (punctuation, an emoji) shares none, so
    // nothing is alike; `rank` would list every memory for it.
    if words(&new.text).is_empty() {
        return Vec::new();
    }
    let pool = current(memories, reader, at)
        .into_iter()
        .filter(|m| m.id != new.id && new.supersedes.as_deref() != Some(m.id.as_str()))
        .filter(|m| {
            m.roles.is_empty()
                || (!new.roles.is_empty() && new.roles.iter().all(|r| m.roles.contains(r)))
        })
        .collect();
    rank(pool, &new.text, limit, 0.0)
}

/// The memories of `said` that answer `query`, the best first, as
/// [`search`] ranks them. A query with no words lists them all, unless a
/// `min_score` asks for matches: then nothing scores.
fn rank<'a>(said: Vec<&'a Memory>, query: &str, limit: usize, min_score: f64) -> Vec<&'a Memory> {
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
        .filter(|(score, _)| {
            if asked.is_empty() {
                min_score <= 0.0
            } else {
                *score > 0.0 && *score >= min_score
            }
        })
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
        let (found, replaced) = search_history(&all, "releases", 10, 0.0, Reader::Person, at(50));
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

    /// A threshold keeps out a memory that shares only words most memories
    /// hold, and keeps one that holds a word few do.
    #[test]
    fn a_search_above_a_score_keeps_only_strong_matches() {
        let all: Vec<Memory> = (0..10_i64)
            .map(|i| {
                let text = if i == 0 {
                    "the deploy needs the staging check".to_owned()
                } else {
                    format!("the note number {i}")
                };
                memory(&format!("m{i}"), &text, i, None)
            })
            .collect();
        let ids = |query: &str, min: f64| {
            search_above(&all, query, 10, min, Reader::Person, now())
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("staging", 1.0), ["m0"]);
        assert!(ids("the", 1.0).is_empty(), "a word every memory holds");
        assert_eq!(ids("the", 0.0).len(), 10);
        // A query of no words asks for nothing a threshold can match.
        assert!(ids("????????????", 1.0).is_empty());
        assert_eq!(
            ids("????????????", 0.0).len(),
            10,
            "unthresholded, it lists"
        );
        // The history is held to the same threshold.
        let history = |query: &str, min: f64| {
            search_history(&all, query, 10, min, Reader::Person, now())
                .0
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(history("staging", 1.0), ["m0"]);
        assert!(history("the", 1.0).is_empty());
        assert!(history("????????????", 1.0).is_empty());
    }

    /// A new memory is compared with the current ones sharing a word with
    /// it, the most alike first, within the limit; never with itself, one it
    /// already replaces, one its author cannot read, or one some reader of
    /// it cannot read.
    #[test]
    fn a_new_memory_is_compared_only_with_what_all_its_readers_can_read() {
        let role = |id: &str, text: &str, roles: &[&str]| Memory {
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            ..memory(id, text, 1, None)
        };
        let all = vec![
            role("open", "deploys go out on Mondays", &[]),
            role("reviewers", "deploys need a reviewer", &["reviewer"]),
            role("both", "deploys need both roles", &["reviewer", "lead"]),
            role("elsewhere", "the cache lives in target", &[]),
        ];
        let ids = |new: &Memory, reader: Reader<'_>, limit: usize| {
            let mut found: Vec<String> = comparable(&all, new, limit, reader, now())
                .iter()
                .map(|m| m.id.clone())
                .collect();
            found.sort();
            found
        };
        // For everyone: only what everyone can read.
        let new = memory("new", "deploys go out on Fridays now", 2, None);
        assert_eq!(ids(&new, Reader::Person, 8), ["open"]);
        // For reviewers: also what every reviewer can read.
        let held = ["reviewer".to_owned()];
        let new = role("new", "deploys go out on Fridays now", &["reviewer"]);
        assert_eq!(
            ids(&new, Reader::Holding(&held), 8),
            ["both", "open", "reviewers"]
        );
        // The author reads less than the memory's readers do: it decides.
        assert_eq!(ids(&new, Reader::EVERYONE, 8), ["open"]);
        // For two roles: what holders of either can read, not of just one.
        let new = role(
            "new",
            "deploys go out on Fridays now",
            &["reviewer", "lead"],
        );
        assert_eq!(ids(&new, Reader::Person, 8), ["both", "open"]);
        assert_eq!(ids(&new, Reader::Person, 1).len(), 1);
        // Itself, and what it already replaces, are out.
        let mut new = memory("open", "deploys go out on Fridays now", 2, None);
        assert!(ids(&new, Reader::Person, 8).is_empty());
        new.id = "new".into();
        new.supersedes = Some("open".into());
        assert!(ids(&new, Reader::Person, 8).is_empty());
        // One with no words shares none, so nothing is alike: not every
        // memory there is.
        for wordless in ["!!!", "🚀", "-- ?"] {
            let new = memory("new", wordless, 2, None);
            assert!(ids(&new, Reader::Person, 8).is_empty(), "{wordless:?}");
        }
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

    /// The shapes past.dev's plugins redact are refused too, each in a
    /// sentence; prose that names a secret without holding one is kept.
    #[test]
    fn more_credential_shapes_are_refused_and_prose_about_them_is_kept() {
        for secret in [
            "auth with Bearer abcdefghijklmnopqrstuv0123",
            "the header is \"authorization: bearer abc.def-ghi_jkl.mno123456\"",
            "session eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcdefghij_klm",
            "session=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcdefghij_klm",
            "see https://example.invalid/cb?jwt=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcdefghij_klm&next=1",
            "the token was eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcdefghij_klm.",
            "ASIAABCDEFGHIJKLMNOP is a session key",
            "ghu_abcdefghijklmnopqrstuvwxyz0123456789 for the app",
            "ghs_abcdefghijklmnopqrstuvwxyz0123456789",
            "xoxa-1234567890-abcdefghij and xoxr-1234567890-abcdefghij",
            "xoxs-1234567890-abcdefghij",
            "sk-ant-api03-abcdefghijklmnopqrstu",
            "glpat-abcdefghijklmnopqrst",
            "DB_PASSWORD=hunter2hunter2",
            "set api_key: \"Zx81-pq77-Lm00\" in the env",
            "GITHUB_TOKEN = ghx9Ab7Cd5Ef3Gh1",
            "CREDENTIALS=p@ssw0rd!",
        ] {
            assert!(secret_like(secret), "{secret}");
        }
        for prose in [
            "TOKEN: see the docs for how to make one",
            "the PASSWORD= field is left empty on purpose",
            "API_KEY=<your key> goes in the env file",
            "API_KEY=${API_KEY} is read from the shell",
            "SECRET: changeme is the default, never ship it",
            "bearer tokens expire after an hour",
            "a JWT starts with eyJ and has three parts",
            "the token budget is 4000 characters: token: limit",
        ] {
            assert!(!secret_like(prose), "{prose}");
        }
    }

    /// The wrappers a secret is written in (brackets, a JSON or YAML key's
    /// quotes, a header with no space) do not hide it, in a memory's text,
    /// a link's target or a link's note. A placeholder, prose and a value
    /// that only says where a secret lives are kept, and a secret written
    /// into one of those is still found.
    #[test]
    fn a_wrapped_secret_is_refused_and_a_placeholder_or_a_locator_is_kept() {
        let refused = [
            "(DB_PASSWORD=hunter2hunter2)",
            "DB_PASSWORD=\"hunter2hunter2\"",
            "(DB_PASSWORD=\"hunter2hunter2\")",
            "PASSWORD=[hunter2hunter2]",
            "{\"DB_PASSWORD\":\"hunter2hunter2\"}",
            "\"api_key\": \"Zx81-pq77-Lm00\"",
            "PASSWORD=Correct.Horse",
            "PASSWORD=abcd/efgh",
            "(Bearer abcdefghijklmnopqrstuv0123)",
            "Authorization:Bearer abcdefghijklmnopqrstuv0123",
            "{\"Authorization\":\"Bearer abcdefghijklmnopqrstuv0123\"}",
            "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuv0123' https://example.invalid",
            // Shaped like a locator but not one, or a secret inside one.
            "TOKEN_FILE=hunter2hunter2",
            "PASSWORD_FILE=abcd/efgh",
            "api_key: settings.ApiKey",
            "TOKEN_FILE=/run/secrets/aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3z",
            "TOKEN: secrets.ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "PASSWORD_PATH=/vault/sk-abcdefghijklmnopqrstuvwx",
        ];
        let kept = [
            "TOKEN: generation.",
            "TOKEN=${TOKEN}",
            "{\"api_key\": \"<your key>\"}",
            "Authorization: Bearer <token>",
            "Authorization: Bearer YOUR_ACCESS_TOKEN_HERE",
            "TOKEN_FILE=/run/secrets/ci_token",
            "PASSWORD_FILE=/etc/app/db_pass",
            "SECRET_PATH: ~/.config/app/secret.json",
            "SECRETS_DIR=./certs/staging",
            "the release job's token: secrets.RELEASE_TOKEN",
        ];
        let now = Utc::now();
        let project = ProjectId::from("p");
        let refuses = |text: &str, links: Vec<Link>| {
            matches!(
                Memory::new(project.clone(), "a", text, links, None, now),
                Err(reason) if reason.contains("reads like a secret")
            )
        };
        for secret in refused {
            assert!(secret_like(secret), "{secret}");
            assert!(refuses(secret, vec![]), "text: {secret}");
            let mut link = Link::new(crate::LinkKind::Memory, secret);
            assert!(refuses("fine", vec![link.clone()]), "target: {secret}");
            link.target = "notes".into();
            link.note = Some(secret.into());
            assert!(refuses("fine", vec![link]), "note: {secret}");
        }
        for text in kept {
            assert!(!secret_like(text), "{text}");
            assert!(
                Memory::new(project.clone(), "a", text, vec![], None, now).is_ok(),
                "{text}"
            );
        }
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
