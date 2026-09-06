//! Handoff bundles: what one agent hands another so the recipient does not
//! have to be told what the daemon already knows.
//!
//! A handoff is a checkpoint addressed to someone. The bundle assembled
//! around it carries the sender's task and note, its checkout and content
//! identity, the leases it holds, what it read and at which versions, the
//! ledger rows it caused, its uncommitted diff when it worked in a
//! worktree, the messages it never read, and its own journal entries. The
//! recipient reviews and accepts through `resume`, which is what commits
//! ownership: leases move (when the sender asked for it), the read set is
//! seeded so staleness carries over, and the journal cursor follows. A
//! bundle is a plain document with a schema version of its own, so it can
//! be exported, carried to another host by hand, and imported there.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgentId, Change, Envelope, JournalEntry, Lease, ProjectId, ReadMark, VcsState};

/// Bumped when a stored bundle's meaning changes.
pub const HANDOFF_SCHEMA: u32 = 2;
/// Maximum serialized size accepted by import, including escaping and nested payloads.
pub const IMPORT_BYTES: usize = 8 * 1024 * 1024;
/// An uncommitted diff is carried up to this many bytes; the rest stays in
/// the worktree the bundle points at.
pub const DIFF_CAP: usize = 64 * 1024;
/// Ledger rows, journal entries, and unread messages carried at most.
pub const BUNDLE_ROWS: usize = 1000;

/// The sender's uncommitted work, for a sender that had its own worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffDiff {
    pub patch: String,
    /// The patch was cut at [`DIFF_CAP`]; the worktree has all of it.
    pub truncated: bool,
    pub worktree: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffBundle {
    pub schema: u32,
    /// The checkpoint's id; `resume` accepts it under the same id.
    pub id: String,
    pub from: AgentId,
    pub from_name: String,
    /// The recipient; `None` for an export nobody is addressed to yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
    /// The physical checkout the sender worked in.
    pub checkout: PathBuf,
    /// Its ignore-aware content identity when the bundle was made; what
    /// acceptance checks against.
    pub version: String,
    #[serde(default)]
    pub environment: Option<crate::container::ContainerEnvironment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs: Option<VcsState>,
    /// Leases the sender held when it handed off; they move to the
    /// recipient at acceptance when `transfer_leases` is set, else they
    /// were released when the bundle was made.
    #[serde(default)]
    pub leases: Vec<Lease>,
    #[serde(default)]
    pub transfer_leases: bool,
    /// Paths the sender read and the versions it read them at.
    #[serde(default)]
    pub read_set: Vec<ReadMark>,
    /// The sender's ledger rows since it joined, newest last.
    #[serde(default)]
    pub changes: Vec<Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<HandoffDiff>,
    #[serde(default)]
    pub unread_inbox: Vec<Envelope>,
    /// The sender's own journal entries, oldest first.
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
    /// Where the sender had read the project journal to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_cursor: Option<u64>,
    pub created_at: DateTime<Utc>,
    /// Set when the bundle came from another host through `import`; the
    /// checkout was re-homed and leases cannot follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_at: Option<DateTime<Utc>>,
}

impl HandoffBundle {
    /// Count serialized bytes without allocating another copy of a large bundle.
    pub fn fits_import_limit(&self) -> bool {
        struct Count(usize);
        impl std::io::Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len());
                if self.0 > IMPORT_BYTES {
                    return Err(std::io::Error::other("handoff import is too large"));
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Count(0), self).is_ok()
    }

    /// One line for the journal and the message: "handed off to gemini-2:
    /// finish the parser".
    pub fn headline(&self, to_name: Option<&str>) -> String {
        match to_name {
            Some(name) => format!("handed off to {name}: {}", self.task),
            None => format!("exported a handoff: {}", self.task),
        }
    }

    /// Cut a patch at [`DIFF_CAP`] on a line boundary.
    pub fn cap_diff(patch: String, worktree: PathBuf) -> HandoffDiff {
        if patch.len() <= DIFF_CAP {
            return HandoffDiff {
                patch,
                truncated: false,
                worktree,
            };
        }
        // Room for the marker, then back to a character boundary and a
        // line boundary, so the kept text fits the cap and never splits a
        // character.
        const MARKER: &str = "… (truncated)\n";
        let mut cut = DIFF_CAP.saturating_sub(MARKER.len());
        while cut > 0 && !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        let cut = patch[..cut].rfind('\n').map_or(cut, |i| i + 1);
        let mut kept = patch[..cut].to_owned();
        kept.push_str(MARKER);
        HandoffDiff {
            patch: kept,
            truncated: true,
            worktree,
        }
    }

    /// Re-home a bundle that came from elsewhere: the checkout becomes the
    /// importer's and read marks move with it, the recipient is the
    /// importer, and leases and the journal cursor stay behind on the host
    /// they belong to. A read mark that does not sit plainly below the
    /// sender's checkout — outside it, or reaching out of it with `..` —
    /// makes the bundle unacceptable rather than a path to go and check.
    pub fn imported(
        mut self,
        to: AgentId,
        checkout: PathBuf,
        now: DateTime<Utc>,
    ) -> Result<Self, String> {
        let old = std::mem::replace(&mut self.checkout, checkout);
        let mut read_set = Vec::with_capacity(self.read_set.len());
        for mut mark in std::mem::take(&mut self.read_set) {
            let relative = mark.path.strip_prefix(&old).map_err(|_| {
                format!(
                    "read mark {} lies outside the checkout",
                    mark.path.display()
                )
            })?;
            if !relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            {
                return Err(format!(
                    "read mark {} reaches outside the checkout",
                    mark.path.display()
                ));
            }
            mark.path = self.checkout.join(relative);
            read_set.push(mark);
        }
        self.read_set = read_set;
        self.to = Some(to);
        self.transfer_leases = false;
        self.journal_cursor = None;
        self.imported_at = Some(now);
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> HandoffBundle {
        HandoffBundle {
            schema: HANDOFF_SCHEMA,
            id: "a1:handoff-x".into(),
            from: AgentId::from("a1"),
            from_name: "codex-1".into(),
            to: Some(AgentId::from("b2")),
            project: None,
            task: "finish the parser".into(),
            note: None,
            assumptions: Vec::new(),
            next_steps: Vec::new(),
            checkout: PathBuf::from("/work/alpha"),
            version: "v1".into(),
            environment: None,
            vcs: None,
            leases: Vec::new(),
            transfer_leases: true,
            read_set: Vec::new(),
            changes: Vec::new(),
            diff: None,
            unread_inbox: Vec::new(),
            journal: Vec::new(),
            journal_cursor: Some(7),
            created_at: Utc::now(),
            imported_at: None,
        }
    }

    #[test]
    fn headline_names_the_recipient_or_the_export() {
        let b = bundle();
        assert_eq!(
            b.headline(Some("gemini-2")),
            "handed off to gemini-2: finish the parser"
        );
        assert_eq!(b.headline(None), "exported a handoff: finish the parser");
    }

    #[test]
    fn diffs_are_cut_on_a_line_boundary_and_say_so() {
        let short = HandoffBundle::cap_diff("+a\n".into(), PathBuf::from("/wt"));
        assert!(!short.truncated);
        assert_eq!(short.patch, "+a\n");
        let line = "+".to_owned() + &"x".repeat(99) + "\n";
        let long = line.repeat(DIFF_CAP / 100 + 5);
        let cut = HandoffBundle::cap_diff(long, PathBuf::from("/wt"));
        assert!(cut.truncated);
        assert!(cut.patch.ends_with("… (truncated)\n"));
        assert!(cut.patch.len() <= DIFF_CAP, "marker included");
        let body = cut.patch.trim_end_matches("… (truncated)\n");
        assert!(body.ends_with('\n'), "cut on a line boundary");
        assert_eq!(cut.worktree, PathBuf::from("/wt"));
        // Multibyte text with no newline near the cap: no panic, no split
        // character, still within the cap.
        let wide = "é".repeat(DIFF_CAP);
        let cut = HandoffBundle::cap_diff(wide, PathBuf::from("/wt"));
        assert!(cut.truncated);
        assert!(cut.patch.len() <= DIFF_CAP);
        assert!(
            cut.patch
                .trim_end_matches("… (truncated)\n")
                .chars()
                .all(|c| c == 'é')
        );
    }

    #[test]
    fn import_rehomes_and_leaves_leases_and_cursor_behind() {
        let now = Utc::now();
        let mark = |path: &str| ReadMark {
            path: PathBuf::from(path),
            at: now,
            version: "h".into(),
            head: None,
        };
        let mut b = bundle();
        b.read_set = vec![mark("/work/alpha/src/a.rs")];
        let b = b
            .imported(AgentId::from("c3"), PathBuf::from("/elsewhere"), now)
            .unwrap();
        assert_eq!(b.to, Some(AgentId::from("c3")));
        assert_eq!(b.checkout, PathBuf::from("/elsewhere"));
        assert_eq!(b.read_set[0].path, PathBuf::from("/elsewhere/src/a.rs"));
        for bad in [
            "/outside/x",
            "/work/alpha/../../etc/passwd",
            "/work/alpha/src/../../x",
        ] {
            let mut b = bundle();
            b.read_set = vec![mark(bad)];
            assert!(
                b.imported(AgentId::from("c3"), PathBuf::from("/elsewhere"), now)
                    .is_err(),
                "{bad} must not be re-homed"
            );
        }
        assert!(!b.transfer_leases);
        assert_eq!(b.journal_cursor, None);
        assert_eq!(b.imported_at, Some(now));
        assert_eq!(b.version, "v1", "content identity travels");
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(serde_json::from_str::<HandoffBundle>(&json).unwrap(), b);
    }
}
