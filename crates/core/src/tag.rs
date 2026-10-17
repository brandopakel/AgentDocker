//! A project's tags: the words cards are sorted by, each with a colour.
//! An agent tags a card only with a tag the project has; the person's
//! new words become tags as they use them.
use crate::ProjectId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// At most this many tags in a project.
pub const TAGS_PER_PROJECT: usize = 200;

/// A tag's colour, by name: the app draws each in its own tone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagColor {
    #[default]
    Gray,
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
}

impl TagColor {
    pub const ALL: [TagColor; 8] = [
        TagColor::Gray,
        TagColor::Red,
        TagColor::Orange,
        TagColor::Yellow,
        TagColor::Green,
        TagColor::Teal,
        TagColor::Blue,
        TagColor::Purple,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TagColor::Gray => "gray",
            TagColor::Red => "red",
            TagColor::Orange => "orange",
            TagColor::Yellow => "yellow",
            TagColor::Green => "green",
            TagColor::Teal => "teal",
            TagColor::Blue => "blue",
            TagColor::Purple => "purple",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.as_str() == text.trim().to_lowercase())
    }

    /// The next colour round the palette: a new tag's, after the last.
    pub fn after(count: usize) -> Self {
        // Gray is for the person to choose; new tags start coloured.
        Self::ALL[1 + count % (Self::ALL.len() - 1)]
    }
}

/// One tag of a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub project: ProjectId,
    pub name: String,
    pub color: TagColor,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The store's id for a project's tag.
pub fn tag_id(project: &ProjectId, name: &str) -> String {
    format!("{}:{name}", project.as_str())
}

/// Whether words make a tag: one to thirty-two letters, digits, -, _ or .
pub fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= crate::task::TAG_CHARS
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// The tags of `wanted` the catalog does not have, in order.
pub fn unknown<'a>(wanted: &'a [String], catalog: &[Tag]) -> Vec<&'a str> {
    wanted
        .iter()
        .filter(|t| !catalog.iter().any(|c| &c.name == *t))
        .map(String::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse_and_new_tags_start_coloured() {
        assert_eq!(TagColor::parse(" Teal "), Some(TagColor::Teal));
        assert_eq!(TagColor::parse("mauve"), None);
        assert_ne!(TagColor::after(0), TagColor::Gray);
        assert_ne!(TagColor::after(7), TagColor::Gray);
        assert!(valid("ui-v2"));
        assert!(!valid("two words"));
        let now = Utc::now();
        let catalog = vec![Tag {
            project: ProjectId::from("p"),
            name: "ui".into(),
            color: TagColor::Blue,
            created_by: "user".into(),
            created_at: now,
            updated_at: now,
        }];
        assert_eq!(unknown(&["ui".into(), "api".into()], &catalog), ["api"]);
    }
}
