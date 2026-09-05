//! The change journal: a per-project, append-only narrative of what changed
//! and why.
//!
//! Coarse where the ledger is fine-grained: one entry per release request,
//! not one per file, plus notes, commits, and agents joining and leaving.
//! Readable by models and humans, cheap to read incrementally, and what a
//! newcomer is handed instead of the event stream. This module holds the
//! pure half — the entry, how it renders, and how a summary is synthesised
//! from paths when nobody wrote one.

use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, AgentRecord, ProjectId, ResourceKey};

/// Paths inlined per entry; the rest is counted in `paths_total`.
pub const PATH_CAP: usize = 200;
/// Paths named in a rendered line before "(+N more)".
const LINE_PATHS: usize = 3;
/// File names a synthesised summary lists before "and N more".
const SUMMARY_FILES: usize = 5;
/// A never-seen reader starts at the newer of this many entries back and
/// [`INITIAL_AGE`] ago.
pub const INITIAL_ENTRIES: usize = 20;
/// See [`INITIAL_ENTRIES`].
pub const INITIAL_AGE: Duration = Duration::hours(24);
/// A finished agent older than this no longer lends its cursor to a
/// newcomer of the same name.
pub const DONOR_MAX_AGE: Duration = Duration::days(7);
/// A transcript-tail summary is trimmed to this many characters.
pub const SUMMARY_CHARS: usize = 280;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalKind {
    /// Leases were released; the entry says what changed under them.
    Release,
    /// Free text an agent wrote.
    Note,
    /// The checkout's HEAD moved: a commit or a branch switch.
    Commit,
    Join,
    Leave,
    /// One agent handed work to another (Phase 4).
    Handoff,
}

impl JournalKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "release" => Self::Release,
            "note" => Self::Note,
            "commit" => Self::Commit,
            "join" => Self::Join,
            "leave" => Self::Leave,
            "handoff" => Self::Handoff,
            _ => return None,
        })
    }
}

impl fmt::Display for JournalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Release => "release",
            Self::Note => "note",
            Self::Commit => "commit",
            Self::Join => "join",
            Self::Leave => "leave",
            Self::Handoff => "handoff",
        })
    }
}

/// Where an entry's summary came from, so renderers can quote a transcript
/// rather than assert it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySource {
    #[default]
    Explicit,
    /// The tail of the agent's own transcript, quoted rather than asserted.
    Transcript,
    Synthesised,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub project: ProjectId,
    /// Per project, assigned by the daemon; `0` until stored.
    #[serde(default)]
    pub seq: u64,
    pub at: DateTime<Utc>,
    /// `None` when the daemon attributes the entry to nobody, such as a
    /// commit made outside AgentDocker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    pub agent_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The physical checkout the entry concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
    /// Set when that checkout is a linked worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    pub kind: JournalKind,
    pub summary: String,
    pub summary_source: SummarySource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceKey>,
    /// Checkout-relative, deduplicated, sorted, capped at [`PATH_CAP`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathBuf>,
    /// The real count when the cap bit.
    #[serde(default)]
    pub paths_total: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_after: Option<String>,
    /// Ledger seq range for drill-down while those rows exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<(u64, u64)>,
}

impl JournalEntry {
    /// One line without a timestamp: `codex-1 [feat/x] released src/a.rs,
    /// src/b.rs (+3 more): "rewrote the tokenizer"`.
    pub fn line(&self) -> String {
        let who = match &self.branch {
            Some(branch) => format!("{} [{branch}]", self.agent_name),
            None => self.agent_name.clone(),
        };
        match self.kind {
            JournalKind::Release => {
                let named: Vec<String> = self
                    .paths
                    .iter()
                    .take(LINE_PATHS)
                    .map(|p| p.display().to_string())
                    .collect();
                let more = self.paths_total.saturating_sub(named.len());
                let mut text = format!("{who} released {}", named.join(", "));
                if more > 0 {
                    text.push_str(&format!(" (+{more} more)"));
                }
                if named.is_empty() {
                    text = format!("{who} released");
                }
                match self.summary_source {
                    SummarySource::Synthesised => text,
                    _ => format!("{text}: \"{}\"", self.summary),
                }
            }
            JournalKind::Note => format!("{who} noted: \"{}\"", self.summary),
            JournalKind::Handoff => format!("{who} handed off: \"{}\"", self.summary),
            JournalKind::Commit | JournalKind::Join | JournalKind::Leave => {
                format!("{who} {}", self.summary)
            }
        }
    }

    /// `line()` with a relative time in front: `- 4m ago   codex-1 …`.
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        format!("- {:<8} {}", ago(now, self.at), self.line())
    }
}

/// The filters a listing applies, for use where the daemon's query cannot
/// be: entries arriving on a followed stream are matched with this so
/// `journal --kind note --follow` keeps showing only notes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalFilter {
    /// An agent id, id prefix, or name.
    pub agent: Option<String>,
    pub branch: Option<String>,
    pub kind: Option<JournalKind>,
    /// Checkout-relative, or absolute when the entry's checkout is known;
    /// a directory matches anything beneath it.
    pub path: Option<PathBuf>,
    /// Case-insensitive substring of the summary.
    pub grep: Option<String>,
    pub until_seq: Option<u64>,
}

impl JournalFilter {
    pub fn matches(&self, entry: &JournalEntry) -> bool {
        if self.until_seq.is_some_and(|until| entry.seq > until) {
            return false;
        }
        if let Some(agent) = &self.agent {
            let by_id = entry
                .agent
                .as_ref()
                .is_some_and(|id| id.as_str().starts_with(agent.as_str()));
            if !by_id && entry.agent_name != *agent {
                return false;
            }
        }
        if let Some(branch) = &self.branch {
            if entry.branch.as_deref() != Some(branch.as_str()) {
                return false;
            }
        }
        if self.kind.is_some_and(|kind| kind != entry.kind) {
            return false;
        }
        if let Some(wanted) = &self.path {
            let relative: &Path = match (wanted.is_absolute(), &entry.checkout) {
                (true, Some(checkout)) => match wanted.strip_prefix(checkout) {
                    Ok(relative) => relative,
                    Err(_) => return false,
                },
                _ => wanted.as_path(),
            };
            if !entry.paths.iter().any(|p| p.starts_with(relative)) {
                return false;
            }
        }
        if let Some(grep) = &self.grep {
            if !entry.summary.to_lowercase().contains(&grep.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

// ----- digests ---------------------------------------------------------------

/// How much of the journal a digest may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestBudget {
    pub max_entries: usize,
    pub max_chars: usize,
}

impl DigestBudget {
    /// What a starting session is handed: about 500 tokens.
    pub const SESSION_START: Self = Self {
        max_entries: 20,
        max_chars: 2000,
    };
    /// What a prompt may carry: only what is new, and little of it.
    pub const PROMPT: Self = Self {
        max_entries: 5,
        max_chars: 500,
    };
}

/// What a reader is handed: the rendered text and what it accounts for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digest {
    /// Empty when nothing is new.
    pub text: String,
    /// The newest seq the digest accounts for; an advancing cursor moves
    /// here. Entries the branch filter hid count as seen too.
    pub head_seq: u64,
    /// Entries rendered verbatim.
    pub shown: usize,
    /// Older entries folded into the leading count line.
    pub collapsed: usize,
    /// Entries left out because they are on another branch.
    pub other_branches: usize,
}

/// Who a digest is for: the branch filter keeps the reader's own branch
/// verbatim, and the reader's own `join` and `leave` lines are not news.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reader<'a> {
    pub name: &'a str,
    pub branch: Option<&'a str>,
    /// Show every branch instead of counting the others.
    pub all_branches: bool,
}

impl Reader<'_> {
    /// Whether an entry is shown to this reader or only counted.
    fn keeps(&self, entry: &JournalEntry) -> bool {
        if self.all_branches
            || matches!(
                entry.kind,
                JournalKind::Join | JournalKind::Leave | JournalKind::Handoff | JournalKind::Commit
            )
        {
            return true;
        }
        match (self.branch, entry.branch.as_deref()) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => true,
        }
    }

    fn is_own_arrival(&self, entry: &JournalEntry) -> bool {
        matches!(entry.kind, JournalKind::Join | JournalKind::Leave)
            && entry.agent_name == self.name
    }
}

/// Render what happened after `cursor` for a reader, within a budget:
/// the newest entries verbatim, older ones folded into one leading count,
/// other branches folded into one trailing count. `entries` are oldest
/// first; anything at or before the cursor is ignored.
pub fn digest(
    entries: &[JournalEntry],
    cursor: u64,
    reader: &Reader<'_>,
    budget: DigestBudget,
    now: DateTime<Utc>,
) -> Digest {
    let fresh: Vec<&JournalEntry> = entries.iter().filter(|e| e.seq > cursor).collect();
    let head_seq = fresh.iter().map(|e| e.seq).max().unwrap_or(cursor);
    let mut matched: Vec<&JournalEntry> = Vec::new();
    let mut other_branches = 0;
    for entry in fresh {
        if reader.is_own_arrival(entry) {
            continue;
        }
        if reader.keeps(entry) {
            matched.push(entry);
        } else {
            other_branches += 1;
        }
    }
    if matched.is_empty() && other_branches == 0 {
        return Digest {
            head_seq,
            ..Digest::default()
        };
    }
    let trailer = match other_branches {
        0 => None,
        n => Some(format!(
            "{n} {} on other branches: agentdocker journal --all-branches",
            plural(n, "entry", "entries")
        )),
    };
    let render = |shown: usize| -> String {
        let mut text = String::new();
        if !matched.is_empty() {
            text.push_str(&format!(
                "Since you last looked ({} {}):\n",
                matched.len(),
                plural(matched.len(), "entry", "entries")
            ));
            let collapsed = matched.len() - shown;
            if collapsed > 0 {
                text.push_str(&format!(
                    "… {collapsed} earlier {} (agentdocker journal --since {cursor})\n",
                    plural(collapsed, "entry", "entries")
                ));
            }
            for entry in &matched[collapsed..] {
                text.push_str(&entry.describe(now));
                text.push('\n');
            }
        }
        if let Some(trailer) = &trailer {
            text.push_str(trailer);
            text.push('\n');
        }
        text
    };
    // The newest entries stay; when the budget bites, the oldest fold
    // first. One entry is always shown when any matched.
    let mut shown = matched.len().min(budget.max_entries.max(1));
    let mut text = render(shown);
    while text.chars().count() > budget.max_chars && shown > 1 {
        shown -= 1;
        text = render(shown);
    }
    Digest {
        text,
        head_seq,
        shown,
        collapsed: matched.len() - shown,
        other_branches,
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

// ----- cursors ---------------------------------------------------------------

/// Where a never-seen reader starts: the newer of [`INITIAL_AGE`] ago and
/// [`INITIAL_ENTRIES`] back, so a newcomer is told recent history, not all
/// of it. `entries` are the project's newest, oldest first; the newest 256
/// are enough since both rules look back at most that far.
pub fn initial_cursor(entries: &[JournalEntry], now: DateTime<Utc>) -> u64 {
    let by_count = entries
        .len()
        .checked_sub(INITIAL_ENTRIES + 1)
        .map_or(0, |i| entries[i].seq);
    let by_age = entries
        .iter()
        .filter(|e| now - e.at > INITIAL_AGE)
        .map(|e| e.seq)
        .max()
        .unwrap_or(0);
    by_count.max(by_age)
}

/// The finished agent whose cursor a newcomer inherits: same name, same
/// project, finished within [`DONOR_MAX_AGE`]; the most recently finished
/// when several qualify. This is what lets a resumed session continue
/// where it left off instead of being told everything twice.
pub fn cursor_donor<'a>(
    records: impl IntoIterator<Item = &'a AgentRecord>,
    name: &str,
    project: &ProjectId,
    now: DateTime<Utc>,
) -> Option<&'a AgentRecord> {
    records
        .into_iter()
        .filter(|r| !r.status.is_live() && r.spec.name == name)
        .filter(|r| r.project.as_ref().is_some_and(|p| p.id() == *project))
        .filter(|r| r.finished_at.is_some_and(|at| now - at <= DONOR_MAX_AGE))
        .max_by_key(|r| r.finished_at)
}

// ----- transcript summaries ------------------------------------------------

/// The last thing the model said, as a journal summary, from the tail of
/// a Claude Code transcript (JSONL, one event per line): the last
/// assistant message with text, markdown stripped, its first paragraph,
/// trimmed to [`SUMMARY_CHARS`] at a word boundary. Lines that do not
/// parse — the cut first line of a tail, say — are skipped.
pub fn transcript_summary(tail: &str) -> Option<String> {
    tail.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event.get("type").and_then(|t| t.as_str()) == Some("assistant"))
        .filter_map(|event| {
            let content = event.get("message")?.get("content")?;
            let text = match content {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Array(blocks) => blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                _ => return None,
            };
            summarise_text(&text)
        })
        .next()
}

/// Markdown to one plain paragraph within [`SUMMARY_CHARS`]; `None` when
/// nothing readable is left.
pub fn summarise_text(text: &str) -> Option<String> {
    // Drop fenced code and headings wholesale, then take the first
    // paragraph: a heading names a section, it does not say what happened.
    let mut kept = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence && !line.trim_start().starts_with('#') {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    let paragraph = kept
        .split("\n\n")
        .map(|p| {
            p.lines()
                .map(strip_markdown_line)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .find(|p| !p.is_empty())?;
    let words: Vec<&str> = paragraph.split_whitespace().collect();
    let mut out = String::new();
    for word in words {
        let with = if out.is_empty() {
            word.chars().count()
        } else {
            out.chars().count() + 1 + word.chars().count()
        };
        if with > SUMMARY_CHARS - 1 && !out.is_empty() {
            out.push('…');
            return Some(out);
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.chars().count() > SUMMARY_CHARS {
        // One word longer than the budget: cut it.
        out = out.chars().take(SUMMARY_CHARS - 1).collect();
        out.push('…');
    }
    Some(out)
}

/// One line of markdown as plain text: no heading, quote, or list marker,
/// no emphasis or code ticks, links reduced to their text.
fn strip_markdown_line(line: &str) -> String {
    let mut s = line.trim();
    s = s.trim_start_matches('#').trim_start();
    s = s.trim_start_matches('>').trim_start();
    if let Some(rest) = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
        s = rest;
    } else if let Some(dot) = s.find(". ") {
        if s[..dot].chars().all(|c| c.is_ascii_digit()) && dot > 0 {
            s = &s[dot + 2..];
        }
    }
    // Images read as their alt text, links as their text.
    let unlinked = s.replace("![", "[");
    let mut out = String::with_capacity(unlinked.len());
    let mut rest = unlinked.as_str();
    while let Some(open) = rest.find('[') {
        let (before, after) = rest.split_at(open);
        out.push_str(before);
        match after.find("](").and_then(|close| {
            after[close..]
                .find(')')
                .map(|end| (&after[1..close], close + end + 1))
        }) {
            Some((text, consumed)) => {
                out.push_str(text);
                rest = &after[consumed..];
            }
            None => {
                out.push('[');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace('`', "")
        .trim()
        .to_owned()
}

/// "4m ago", "2h ago", "3d ago".
pub fn ago(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    match secs {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// Deduplicate, sort, and cap; returns the kept paths and the real count.
pub fn cap_paths(mut paths: Vec<PathBuf>) -> (Vec<PathBuf>, usize) {
    paths.sort();
    paths.dedup();
    let total = paths.len();
    paths.truncate(PATH_CAP);
    (paths, total)
}

/// What to say when nobody wrote a summary: "edited 3 files under src/:
/// parser.rs, lexer.rs, mod.rs".
pub fn synthesise_summary(paths: &[PathBuf], total: usize) -> String {
    if paths.is_empty() {
        return "released leases".to_owned();
    }
    let total = total.max(paths.len());
    let names: Vec<String> = paths
        .iter()
        .take(SUMMARY_FILES)
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        })
        .collect();
    let under = common_dir(paths)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| format!(" under {}/", dir.display()))
        .unwrap_or_default();
    let mut text = format!(
        "edited {total} file{}{under}: {}",
        if total == 1 { "" } else { "s" },
        names.join(", ")
    );
    if total > names.len() {
        text.push_str(&format!(" and {} more", total - names.len()));
    }
    text
}

fn common_dir(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut common: Option<PathBuf> = None;
    for path in paths {
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        common = Some(match common {
            None => dir,
            Some(current) => {
                let shared: PathBuf = current
                    .components()
                    .zip(dir.components())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a)
                    .collect();
                shared
            }
        });
    }
    common
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: JournalKind, summary: &str, source: SummarySource) -> JournalEntry {
        JournalEntry {
            project: ProjectId::from("p"),
            seq: 1,
            at: Utc::now(),
            agent: Some(AgentId::from("a1")),
            agent_name: "codex-1".into(),
            branch: Some("feat/x".into()),
            checkout: None,
            worktree: None,
            kind,
            summary: summary.into(),
            summary_source: source,
            resources: Vec::new(),
            paths: vec![
                "src/a.rs".into(),
                "src/b.rs".into(),
                "src/c.rs".into(),
                "src/d.rs".into(),
            ],
            paths_total: 5,
            head_before: None,
            head_after: None,
            changes: None,
        }
    }

    #[test]
    fn lines_per_kind() {
        let e = entry(
            JournalKind::Release,
            "rewrote the tokenizer",
            SummarySource::Explicit,
        );
        assert_eq!(
            e.line(),
            "codex-1 [feat/x] released src/a.rs, src/b.rs, src/c.rs (+2 more): \"rewrote the tokenizer\""
        );
        let e = entry(
            JournalKind::Release,
            "edited 5 files",
            SummarySource::Synthesised,
        );
        assert_eq!(
            e.line(),
            "codex-1 [feat/x] released src/a.rs, src/b.rs, src/c.rs (+2 more)"
        );
        let mut bare = entry(JournalKind::Release, "done", SummarySource::Explicit);
        bare.paths.clear();
        bare.paths_total = 0;
        bare.branch = None;
        assert_eq!(bare.line(), "codex-1 released: \"done\"");
        assert_eq!(
            entry(JournalKind::Note, "parser is next", SummarySource::Explicit).line(),
            "codex-1 [feat/x] noted: \"parser is next\""
        );
        assert_eq!(
            entry(
                JournalKind::Commit,
                "committed 3f9c1e0: Add lease transfer",
                SummarySource::Synthesised
            )
            .line(),
            "codex-1 [feat/x] committed 3f9c1e0: Add lease transfer"
        );
        assert_eq!(
            entry(
                JournalKind::Join,
                "joined (branch feat/x)",
                SummarySource::Synthesised
            )
            .line(),
            "codex-1 [feat/x] joined (branch feat/x)"
        );
        let described = entry(JournalKind::Leave, "left", SummarySource::Synthesised)
            .describe(Utc::now() + chrono::Duration::minutes(4));
        assert!(described.starts_with("- 4m ago   codex-1"), "{described}");
    }

    #[test]
    fn synthesised_summaries_name_the_directory_and_files() {
        let paths: Vec<PathBuf> = ["src/parser.rs", "src/lexer.rs", "src/mod.rs"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            synthesise_summary(&paths, 3),
            "edited 3 files under src/: parser.rs, lexer.rs, mod.rs"
        );
        let mixed: Vec<PathBuf> = ["src/a.rs", "docs/b.md"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(synthesise_summary(&mixed, 2), "edited 2 files: a.rs, b.md");
        let one = vec![PathBuf::from("README.md")];
        assert_eq!(synthesise_summary(&one, 1), "edited 1 file: README.md");
        let many: Vec<PathBuf> = (0..7)
            .map(|i| PathBuf::from(format!("src/f{i}.rs")))
            .collect();
        assert_eq!(
            synthesise_summary(&many, 7),
            "edited 7 files under src/: f0.rs, f1.rs, f2.rs, f3.rs, f4.rs and 2 more"
        );
        assert_eq!(synthesise_summary(&[], 0), "released leases");
    }

    #[test]
    fn filters_match_followed_entries_like_the_query_would() {
        let mut e = entry(JournalKind::Note, "Parser is next", SummarySource::Explicit);
        e.checkout = Some(PathBuf::from("/repo"));
        let f = |f: &dyn Fn(&mut JournalFilter)| {
            let mut filter = JournalFilter::default();
            f(&mut filter);
            filter
        };
        assert!(f(&|_| {}).matches(&e));
        assert!(f(&|x| x.until_seq = Some(1)).matches(&e));
        assert!(!f(&|x| x.until_seq = Some(0)).matches(&e));
        assert!(f(&|x| x.agent = Some("a1".into())).matches(&e), "by id");
        assert!(
            f(&|x| x.agent = Some("a".into())).matches(&e),
            "by id prefix"
        );
        assert!(
            f(&|x| x.agent = Some("codex-1".into())).matches(&e),
            "by name"
        );
        assert!(!f(&|x| x.agent = Some("codex-2".into())).matches(&e));
        assert!(f(&|x| x.branch = Some("feat/x".into())).matches(&e));
        assert!(!f(&|x| x.branch = Some("main".into())).matches(&e));
        assert!(f(&|x| x.kind = Some(JournalKind::Note)).matches(&e));
        assert!(!f(&|x| x.kind = Some(JournalKind::Release)).matches(&e));
        assert!(f(&|x| x.path = Some("src".into())).matches(&e), "directory");
        assert!(f(&|x| x.path = Some("src/b.rs".into())).matches(&e));
        assert!(!f(&|x| x.path = Some("docs".into())).matches(&e));
        assert!(
            f(&|x| x.path = Some("/repo/src/a.rs".into())).matches(&e),
            "absolute, under the entry's checkout"
        );
        assert!(!f(&|x| x.path = Some("/elsewhere/src/a.rs".into())).matches(&e));
        assert!(
            f(&|x| x.grep = Some("parser".into())).matches(&e),
            "case-insensitive"
        );
        assert!(!f(&|x| x.grep = Some("lexer".into())).matches(&e));
    }

    fn at(
        seq: u64,
        kind: JournalKind,
        name: &str,
        branch: Option<&str>,
        summary: &str,
    ) -> JournalEntry {
        let mut e = entry(kind, summary, SummarySource::Explicit);
        e.seq = seq;
        e.agent_name = name.into();
        e.agent = Some(AgentId::from(format!("id-{name}").as_str()));
        e.branch = branch.map(str::to_owned);
        e.at = Utc::now() - chrono::Duration::minutes(seq as i64);
        e
    }

    #[test]
    fn digest_filters_branches_skips_own_arrival_and_keeps_the_newest() {
        let now = Utc::now();
        let entries = vec![
            at(1, JournalKind::Join, "codex-1", Some("main"), "joined"),
            at(
                2,
                JournalKind::Note,
                "codex-1",
                Some("main"),
                "parser is next",
            ),
            at(
                3,
                JournalKind::Release,
                "gemini-2",
                Some("feat/y"),
                "edited lexer",
            ),
            at(
                4,
                JournalKind::Commit,
                "gemini-2",
                Some("feat/y"),
                "committed abc: Add lexer",
            ),
            at(
                5,
                JournalKind::Join,
                "me",
                Some("main"),
                "joined (branch main)",
            ),
            at(6, JournalKind::Note, "codex-1", Some("main"), "lexer done"),
            at(7, JournalKind::Leave, "me", Some("main"), "left (exited 0)"),
        ];
        let me = Reader {
            name: "me",
            branch: Some("main"),
            all_branches: false,
        };
        let d = digest(&entries, 0, &me, DigestBudget::SESSION_START, now);
        assert_eq!(
            (d.head_seq, d.shown, d.collapsed, d.other_branches),
            (7, 4, 0, 1)
        );
        let lines: Vec<&str> = d.text.lines().collect();
        assert_eq!(lines[0], "Since you last looked (4 entries):");
        assert!(lines[1].ends_with("codex-1 [main] joined"), "{}", lines[1]);
        assert!(lines[2].contains("noted: \"parser is next\""));
        assert!(
            lines[3].contains("gemini-2 [feat/y] committed abc"),
            "commits from every branch: {}",
            lines[3]
        );
        assert!(lines[4].contains("noted: \"lexer done\""));
        assert_eq!(
            lines[5],
            "1 entry on other branches: agentdocker journal --all-branches"
        );
        assert_eq!(lines.len(), 6, "own join and leave are not news");

        // The cursor: only what is after it, and the head is the newest seq
        // even when the filter hid it.
        let d = digest(&entries, 6, &me, DigestBudget::SESSION_START, now);
        assert_eq!((d.head_seq, d.shown, d.other_branches), (7, 0, 0));
        assert_eq!(d.text, "", "nothing new means no text");
        let d = digest(&entries[..3], 2, &me, DigestBudget::SESSION_START, now);
        assert_eq!((d.head_seq, d.shown, d.other_branches), (3, 0, 1));
        assert_eq!(
            d.text,
            "1 entry on other branches: agentdocker journal --all-branches\n"
        );

        // All branches, and a reader without a branch, see everything.
        let all = Reader {
            all_branches: true,
            ..me
        };
        assert_eq!(
            digest(&entries, 0, &all, DigestBudget::SESSION_START, now).shown,
            5
        );
        let detached = Reader { branch: None, ..me };
        assert_eq!(
            digest(&entries, 0, &detached, DigestBudget::SESSION_START, now).other_branches,
            0
        );

        // The entry budget folds the oldest into the count line.
        let d = digest(
            &entries,
            0,
            &me,
            DigestBudget {
                max_entries: 2,
                max_chars: 2000,
            },
            now,
        );
        assert_eq!((d.shown, d.collapsed), (2, 2));
        let lines: Vec<&str> = d.text.lines().collect();
        assert_eq!(
            lines[1],
            "… 2 earlier entries (agentdocker journal --since 0)"
        );
        assert!(lines[2].contains("committed abc"));
        assert!(lines[3].contains("lexer done"));

        // The character budget does the same, and always shows one.
        let d = digest(
            &entries,
            0,
            &me,
            DigestBudget {
                max_entries: 20,
                max_chars: 120,
            },
            now,
        );
        assert_eq!(d.shown, 1, "{}", d.text);
        assert_eq!(d.collapsed, 3);
        assert!(d.text.contains("lexer done"));
        let d = digest(
            &entries,
            0,
            &me,
            DigestBudget {
                max_entries: 20,
                max_chars: 1,
            },
            now,
        );
        assert_eq!(d.shown, 1, "one entry even when it does not fit");
    }

    #[test]
    fn initial_cursor_is_the_newer_of_twenty_back_and_a_day_ago() {
        let now = Utc::now();
        assert_eq!(initial_cursor(&[], now), 0);
        // Thirty young entries: twenty back wins.
        let young: Vec<JournalEntry> = (1..=30)
            .map(|seq| {
                let mut e = at(seq, JournalKind::Note, "a", None, "x");
                e.at = now - chrono::Duration::minutes(31 - seq as i64);
                e
            })
            .collect();
        assert_eq!(
            initial_cursor(&young, now),
            10,
            "entries 11..=30 are the newest twenty"
        );
        assert_eq!(
            initial_cursor(&young[..15], now),
            0,
            "fewer than twenty: all of them"
        );
        // The oldest fifteen are older than a day: the day wins.
        let mut mixed = young.clone();
        for e in &mut mixed[..15] {
            e.at = now - chrono::Duration::hours(25);
        }
        assert_eq!(initial_cursor(&mixed, now), 15);
    }

    #[test]
    fn cursor_donor_is_a_recently_finished_namesake_in_the_same_project() {
        use crate::{AgentSpec, AgentStatus, ProjectRef};
        let now = Utc::now();
        let project = ProjectRef::directory("/work/alpha");
        let record = |name: &str, dir: &str, finished: Option<chrono::Duration>| {
            let mut r = AgentRecord::new(
                AgentSpec {
                    name: name.to_owned(),
                    ..AgentSpec::default()
                },
                false,
                now - chrono::Duration::days(10),
            );
            r.project = Some(ProjectRef::directory(dir));
            if let Some(ago) = finished {
                r.status = AgentStatus::Exited { code: Some(0) };
                r.finished_at = Some(now - ago);
            } else {
                r.status = AgentStatus::Running;
            }
            r
        };
        let older = record("claude-abc", "/work/alpha", Some(chrono::Duration::days(2)));
        let newer = record(
            "claude-abc",
            "/work/alpha",
            Some(chrono::Duration::hours(1)),
        );
        let stale = record("claude-abc", "/work/alpha", Some(chrono::Duration::days(8)));
        let elsewhere = record("claude-abc", "/work/beta", Some(chrono::Duration::hours(1)));
        let live = record("claude-abc", "/work/alpha", None);
        let other = record(
            "claude-xyz",
            "/work/alpha",
            Some(chrono::Duration::hours(1)),
        );
        let all = [&older, &newer, &stale, &elsewhere, &live, &other];
        let donor = cursor_donor(all.iter().copied(), "claude-abc", &project.id(), now).unwrap();
        assert_eq!(donor.id, newer.id, "most recently finished namesake");
        assert!(
            cursor_donor(
                [&stale, &elsewhere, &live, &other],
                "claude-abc",
                &project.id(),
                now
            )
            .is_none()
        );
        assert!(
            cursor_donor(
                [&older],
                "claude-abc",
                &ProjectRef::directory("/work/beta").id(),
                now
            )
            .is_none()
        );
    }

    #[test]
    fn transcript_tail_yields_the_last_assistant_paragraph_as_plain_text() {
        let line = |v: serde_json::Value| serde_json::to_string(&v).unwrap();
        let tail = [
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"cut off".to_owned(),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"Earlier message."}]}})),
            line(serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[
                {"type":"thinking","thinking":"hmm"},
                {"type":"text","text":"## Done\n\n```rust\nfn x() {}\n```\n\n- Rewrote the **tokenizer** in `src/lexer.rs` to handle [unicode](https://example.com) escapes.\n  Second line of the paragraph.\n\nNext I will look at the parser."},
                {"type":"tool_use","name":"Edit"}]}})),
            line(serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}})),
            line(serde_json::json!({"type":"system","subtype":"stop"})),
        ]
        .join("\n");
        assert_eq!(
            transcript_summary(&tail).as_deref(),
            Some(
                "Rewrote the tokenizer in src/lexer.rs to handle unicode escapes. Second line of the paragraph."
            )
        );
        assert_eq!(transcript_summary("not json\n{\"type\":\"user\"}"), None);
        assert_eq!(
            transcript_summary(&line(
                serde_json::json!({"type":"assistant","message":{"content":"plain string"}})
            ))
            .as_deref(),
            Some("plain string")
        );

        // The word-boundary trim.
        let long = "word ".repeat(100);
        let short = summarise_text(&long).unwrap();
        assert!(
            short.chars().count() <= SUMMARY_CHARS,
            "{}",
            short.chars().count()
        );
        assert!(short.ends_with("word…"), "{short}");
        let giant = "x".repeat(400);
        assert_eq!(
            summarise_text(&giant).unwrap().chars().count(),
            SUMMARY_CHARS
        );
        assert_eq!(summarise_text("```\nonly code\n```"), None);
        assert_eq!(
            summarise_text("1. first item\n2. second"),
            Some("first item second".into())
        );
        assert_eq!(
            summarise_text("> quoted ![img](u.png) ~~gone~~"),
            Some("quoted img gone".into())
        );
    }

    #[test]
    fn paths_are_deduplicated_sorted_and_capped() {
        let mut paths: Vec<PathBuf> = (0..(PATH_CAP + 10))
            .map(|i| PathBuf::from(format!("f{i:04}")))
            .collect();
        paths.push(PathBuf::from("f0000"));
        let (kept, total) = cap_paths(paths);
        assert_eq!(total, PATH_CAP + 10);
        assert_eq!(kept.len(), PATH_CAP);
        assert_eq!(kept[0], PathBuf::from("f0000"));
        assert_eq!(JournalKind::parse("note"), Some(JournalKind::Note));
        assert_eq!(JournalKind::parse("nope"), None);
    }
}
