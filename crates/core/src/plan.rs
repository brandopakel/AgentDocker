//! A project's plans and files. A plan is a spec in Markdown that cards
//! carry out (a card names its plan); a file is anything the work needs
//! kept beside it — notes, a design, a log — linked to the cards it is
//! for. Both are the project's, not one board's.
use crate::{ProjectId, TaskId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A plan's title: at most this many characters.
pub const PLAN_TITLE_CHARS: usize = 200;
/// A plan's body: at most this many bytes of Markdown.
pub const PLAN_BODY_BYTES: usize = 512 * 1024;
/// At most this many plans in a project.
pub const PLANS_PER_PROJECT: usize = 500;
/// A file: at most this many bytes.
pub const FILE_BYTES: usize = 4 * 1024 * 1024;
/// At most this many files in a project.
pub const FILES_PER_PROJECT: usize = 500;
/// A file's name: at most this many characters.
pub const FILE_NAME_CHARS: usize = 200;
/// A plan remembers this many of its last edits: who made each, when, and
/// which sections it changed.
pub const PLAN_EDITS_KEPT: usize = 20;
/// How a plan's text before its first `## ` heading is named among the
/// sections an edit changed.
pub const PLAN_PREAMBLE: &str = "(before the first heading)";

/// Where a plan stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    #[default]
    Draft,
    Active,
    Done,
    Archived,
}

impl PlanStatus {
    pub const ALL: [PlanStatus; 4] = [
        PlanStatus::Draft,
        PlanStatus::Active,
        PlanStatus::Done,
        PlanStatus::Archived,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PlanStatus::Draft => "draft",
            PlanStatus::Active => "active",
            PlanStatus::Done => "done",
            PlanStatus::Archived => "archived",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PlanStatus::Draft => "Draft",
            PlanStatus::Active => "Active",
            PlanStatus::Done => "Done",
            PlanStatus::Archived => "Archived",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str() == text.trim().to_lowercase())
    }
}

/// One plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub project: ProjectId,
    pub title: String,
    /// The spec, in Markdown. Lists leave it out; `plan_get` has it.
    #[serde(default)]
    pub body: String,
    pub status: PlanStatus,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// How many times the plan's text has been written: one for a new
    /// plan, raised by every edit that changes the body. An edit of the
    /// body names the revision it was written from, so one written from
    /// an older reading is told what changed instead of overwriting it. A
    /// plan stored before revisions were kept reads as revision zero.
    #[serde(default)]
    pub revision: u64,
    /// Its last edits of the body, the oldest first, at most
    /// [`PLAN_EDITS_KEPT`]: what a writer whose reading is out of date is
    /// told. Lists leave it out, as they do the body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<PlanEdit>,
}

/// One edit of a plan's body: who made it, when, the revision it made,
/// and the `## ` sections whose text it changed, added or removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEdit {
    pub revision: u64,
    pub by: String,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<String>,
}

/// A plan's Markdown as its `## ` sections: each heading with the text
/// under it, and what stands before the first one under
/// [`PLAN_PREAMBLE`]. A heading inside a fenced block is still read as
/// one: this names what changed, it does not parse the document. Two
/// sections with one heading are told apart by their order.
fn sections(body: &str) -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = vec![(PLAN_PREAMBLE.to_owned(), String::new())];
    for line in body.lines() {
        match line.strip_prefix("## ") {
            Some(heading) => all.push((heading.trim().to_owned(), String::new())),
            None => {
                let text = &mut all.last_mut().expect("the preamble is always there").1;
                text.push_str(line.trim_end());
                text.push('\n');
            }
        }
    }
    all
}

/// The sections whose text differs between two bodies of a plan: changed,
/// added or removed, in the order of the newer body and then what the
/// older alone had. Trailing spaces and the blank lines at a section's
/// end do not count as a change.
pub fn changed_sections(before: &str, after: &str) -> Vec<String> {
    let keyed = |body: &str| {
        let mut seen: std::collections::BTreeMap<String, usize> = Default::default();
        sections(body)
            .into_iter()
            .map(|(heading, text)| {
                let nth = seen.entry(heading.clone()).or_default();
                *nth += 1;
                ((heading, *nth), text.trim_end().to_owned())
            })
            .collect::<Vec<_>>()
    };
    let (old, new) = (keyed(before), keyed(after));
    let mut changed: Vec<String> = Vec::new();
    let mut name = |heading: &str| {
        if !changed.iter().any(|known| known == heading) {
            changed.push(heading.to_owned());
        }
    };
    for (key, text) in &new {
        if old
            .iter()
            .find(|(other, _)| other == key)
            .map(|(_, text)| text)
            != Some(text)
        {
            name(&key.0);
        }
    }
    for (key, _) in &old {
        if !new.iter().any(|(other, _)| other == key) {
            name(&key.0);
        }
    }
    changed
}

impl Plan {
    /// The plan as an edit leaves it. A `body` that differs from its own
    /// takes its place, raises the revision and is noted among its edits
    /// with the sections it changed; one that is the same text, or none,
    /// leaves the revision where it was, so a change of title or status
    /// puts nobody's reading of the text out of date.
    pub fn edited(mut self, body: Option<String>, by: &str, now: DateTime<Utc>) -> Self {
        self.updated_at = now;
        if let Some(body) = body
            && body != self.body
        {
            self.revision += 1;
            self.edits.push(PlanEdit {
                revision: self.revision,
                by: by.to_owned(),
                at: now,
                sections: changed_sections(&self.body, &body),
            });
            let extra = self.edits.len().saturating_sub(PLAN_EDITS_KEPT);
            self.edits.drain(..extra);
            self.body = body;
        }
        self
    }

    /// Whether an edit written from revision `read` was written from the
    /// plan as it is now.
    pub fn is_current(&self, read: u64) -> bool {
        read == self.revision
    }

    /// What a writer who read revision `read` has not seen: the edits of
    /// the body made since, the oldest first, as far back as they are
    /// kept.
    pub fn edits_since(&self, read: u64) -> Vec<&PlanEdit> {
        self.edits
            .iter()
            .filter(|edit| edit.revision > read)
            .collect()
    }

    pub fn new(
        project: ProjectId,
        title: &str,
        body: &str,
        created_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, &'static str> {
        let plan = Self {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
            project,
            title: title.trim().to_owned(),
            body: body.to_owned(),
            status: PlanStatus::Draft,
            created_by: created_by.to_owned(),
            created_at: now,
            updated_at: now,
            revision: 1,
            edits: Vec::new(),
        };
        plan.check()?;
        Ok(plan)
    }

    pub fn check(&self) -> Result<(), &'static str> {
        let title = self.title.trim();
        if title.is_empty() || title.chars().count() > PLAN_TITLE_CHARS {
            return Err("a plan's title is one to 200 characters");
        }
        if self.body.len() > PLAN_BODY_BYTES {
            return Err("a plan is at most 512 KiB of Markdown");
        }
        Ok(())
    }

    /// The same plan without its body or its edits, as lists show it.
    pub fn summary(&self) -> Self {
        Self {
            body: String::new(),
            edits: Vec::new(),
            ..self.clone()
        }
    }
}

/// One file of a project. Its bytes are kept apart (`file_body`), by its
/// id, so a list of files never carries them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub id: String,
    pub project: ProjectId,
    pub name: String,
    /// `text/markdown`, `application/json`, `image/png`…: what the app
    /// opens it as. Guessed from the name when not said.
    pub media_type: String,
    pub size: u64,
    /// Hex SHA-256 of its bytes.
    pub sha256: String,
    /// The cards it is for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskId>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Whether a file's name is one the project can keep: one to 200
/// characters, no path separators or control characters, not `.` or
/// `..`.
pub fn valid_file_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.chars().count() <= FILE_NAME_CHARS
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

/// A media type from a file's name, for one that does not say.
pub fn media_type_of(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "rs" | "py" | "go" | "js" | "ts" | "tsx" | "sh" | "c" | "h" | "swift" | "java" | "kt"
        | "rb" | "sql" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Whether a file of this type reads as text.
pub fn is_text(media_type: &str) -> bool {
    media_type.starts_with("text/")
        || matches!(
            media_type,
            "application/json" | "application/yaml" | "application/toml" | "image/svg+xml"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan's edits are told by the sections they touch: changed, added
    /// and removed ones, what stands before the first heading under its
    /// own name, two sections of one name apart, and nothing for a change
    /// of trailing space alone.
    #[test]
    fn an_edit_is_told_by_the_sections_it_changed() {
        let before = "Opening.\n\n## Goals\nShip it.\n\n## Order\n1. First.\n\n## Notes\nOld.\n";
        assert_eq!(changed_sections(before, before), Vec::<String>::new());
        assert_eq!(
            changed_sections(before, &before.replace("Ship it.", "Ship it well.")),
            ["Goals"]
        );
        assert_eq!(
            changed_sections(before, &before.replace("Opening.", "A new opening.")),
            [PLAN_PREAMBLE]
        );
        let reordered_and_grown = "Opening.\n\n## Goals\nShip it.\n\n## Risks\nNone yet.\n\n## Order\n1. First.\n2. Second.\n";
        assert_eq!(
            changed_sections(before, reordered_and_grown),
            ["Risks", "Order", "Notes"],
            "added, changed, then what the older alone had"
        );
        assert_eq!(
            changed_sections(before, &before.replace("Ship it.\n", "Ship it.   \n\n\n")),
            Vec::<String>::new(),
            "trailing space is no change"
        );
        let twice = "## Log\none\n\n## Log\ntwo\n";
        assert_eq!(
            changed_sections(twice, &twice.replace("two", "three")),
            ["Log"]
        );
    }

    /// The revision follows the text: a new plan is at one, an edit that
    /// changes the body raises it and is remembered with who made it, the
    /// same text or a change of title alone leaves it, and only the last
    /// edits are kept.
    #[test]
    fn a_plans_revision_follows_its_text() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let plan = Plan::new(
            ProjectId::from("p"),
            "Where it stands",
            "## Lead\none\n\n## Reviewer\nnone\n",
            "agent-1",
            now,
        )
        .unwrap();
        assert_eq!((plan.revision, plan.edits.len()), (1, 0));
        assert!(plan.is_current(1) && !plan.is_current(0));

        let same = plan.clone().edited(Some(plan.body.clone()), "agent-2", now);
        assert_eq!((same.revision, same.edits.len()), (1, 0), "the same text");
        let retitled = plan.clone().edited(None, "agent-2", now);
        assert_eq!(retitled.revision, 1, "no text at all");

        let edited = plan
            .clone()
            .edited(Some(plan.body.replace("none", "PASS")), "agent-2", now);
        assert_eq!(edited.revision, 2);
        assert_eq!(
            edited.edits,
            [PlanEdit {
                revision: 2,
                by: "agent-2".into(),
                at: now,
                sections: vec!["Reviewer".into()],
            }]
        );
        assert_eq!(edited.edits_since(1).len(), 1);
        assert!(edited.edits_since(2).is_empty());
        assert!(edited.summary().edits.is_empty() && edited.summary().revision == 2);

        let mut busy = plan;
        for round in 0..PLAN_EDITS_KEPT + 5 {
            busy = busy
                .clone()
                .edited(Some(format!("## Lead\n{round}\n")), "agent-1", now);
        }
        assert_eq!(busy.revision, 1 + (PLAN_EDITS_KEPT as u64) + 5);
        assert_eq!(busy.edits.len(), PLAN_EDITS_KEPT);
        assert_eq!(busy.edits.last().unwrap().revision, busy.revision);

        // A plan stored before revisions were kept reads as revision zero.
        let stored = serde_json::json!({
            "id": "abc", "project": "p", "title": "Old", "body": "text", "status": "draft",
            "created_by": "agent-1", "created_at": now, "updated_at": now
        });
        let old: Plan = serde_json::from_value(stored).unwrap();
        assert_eq!((old.revision, old.edits.len()), (0, 0));
        assert!(old.is_current(0));
    }

    #[test]
    fn plans_and_file_names_are_bounded() {
        let now = Utc::now();
        assert!(Plan::new(ProjectId::from("p"), "Auth", "# Auth", "user", now).is_ok());
        assert!(Plan::new(ProjectId::from("p"), " ", "", "user", now).is_err());
        assert!(
            Plan::new(
                ProjectId::from("p"),
                "Big",
                &"x".repeat(PLAN_BODY_BYTES + 1),
                "user",
                now
            )
            .is_err()
        );
        assert!(valid_file_name("notes.md"));
        assert!(!valid_file_name("../etc/passwd"));
        assert!(!valid_file_name(".."));
        assert_eq!(media_type_of("Spec.MD"), "text/markdown");
        assert_eq!(media_type_of("blob"), "application/octet-stream");
        assert!(is_text("application/json"));
        assert!(!is_text("image/png"));
        assert_eq!(PlanStatus::parse("Active"), Some(PlanStatus::Active));
    }
}
