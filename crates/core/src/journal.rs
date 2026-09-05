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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, ProjectId, ResourceKey};

/// Paths inlined per entry; the rest is counted in `paths_total`.
pub const PATH_CAP: usize = 200;
/// Paths named in a rendered line before "(+N more)".
const LINE_PATHS: usize = 3;
/// File names a synthesised summary lists before "and N more".
const SUMMARY_FILES: usize = 5;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySource {
    Explicit,
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
