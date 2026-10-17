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
}

impl Plan {
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

    /// The same plan without its body, as lists show it.
    pub fn summary(&self) -> Self {
        Self {
            body: String::new(),
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
