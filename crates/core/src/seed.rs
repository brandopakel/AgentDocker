//! Seeding a new worktree with the build output of the checkout it came
//! from (`worktree_create`'s `seed_build`), so its first build starts warm.
//!
//! What may be named, and what is said about each name afterwards. The
//! copying is the host's; this is the part with no I/O in it.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// At most this many directories are seeded into one worktree.
pub const MAX_SEEDS: usize = 16;

/// What became of one directory named in `seed_build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Cloned whole with `clonefile(2)`: copy-on-write on APFS, so it
    /// takes no space until one side changes a file.
    Cloned,
    /// Every file cloned by reflink: copy-on-write on the Linux
    /// filesystems that share extents (Btrfs, XFS and the like).
    Reflinked,
    /// Copied byte for byte.
    Copied,
    /// The source checkout has no such directory; nothing was done.
    Skipped,
    /// Not seeded: it leads outside the source checkout or is not a
    /// directory, or the new worktree already has something there.
    Refused,
    /// The copy failed or ran out of time, and what it made was removed.
    Failed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloned => "cloned",
            Self::Reflinked => "reflinked",
            Self::Copied => "copied",
            Self::Skipped => "skipped",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }

    /// Whether the directory is now in the new worktree.
    pub fn seeded(self) -> bool {
        matches!(self, Self::Cloned | Self::Reflinked | Self::Copied)
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `seed_build` directory and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seeded {
    /// The directory as it was named.
    pub dir: String,
    pub outcome: Outcome,
    /// Why it was skipped, refused or failed, or why a clone became a
    /// copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The directory `name` names relative to a checkout: plain names only
/// (`target`, `web/node_modules`; a leading `./` and a trailing `/` are
/// fine). Refused: an empty name, an absolute path, `..`, and git's own
/// `.git`, whose copy would be a second repository rather than a cache.
pub fn relative(name: &Path) -> Result<PathBuf, &'static str> {
    let mut path = PathBuf::new();
    for part in name.components() {
        match part {
            Component::Normal(part) if part == ".git" => {
                return Err("git's own directory is never seeded");
            }
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err("must not contain `..`"),
            Component::RootDir | Component::Prefix(_) => {
                return Err("must be relative to the checkout");
            }
        }
    }
    if path.as_os_str().is_empty() {
        return Err("names no directory");
    }
    Ok(path)
}

/// Check every name a request carries before anything is made: at most
/// [`MAX_SEEDS`], each one [`relative`]. The error names the first bad one.
pub fn check(names: &[String]) -> Result<(), String> {
    if names.len() > MAX_SEEDS {
        return Err(format!(
            "at most {MAX_SEEDS} seed_build directories, not {}",
            names.len()
        ));
    }
    for name in names {
        relative(Path::new(name)).map_err(|reason| format!("seed_build {name:?}: {reason}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_relative_names_are_accepted() {
        for (name, expected) in [
            ("target", "target"),
            ("target/", "target"),
            ("./target", "target"),
            ("web/node_modules", "web/node_modules"),
            (".venv", ".venv"),
            ("a/./b", "a/b"),
        ] {
            assert_eq!(
                relative(Path::new(name)).unwrap(),
                Path::new(expected),
                "{name}"
            );
        }
    }

    #[test]
    fn absolute_parent_empty_and_git_names_are_refused() {
        for name in [
            "",
            ".",
            "./",
            "/target",
            "/",
            "..",
            "../target",
            "target/../../x",
            "a/..",
            ".git",
            ".git/objects",
            "sub/.git",
        ] {
            assert!(relative(Path::new(name)).is_err(), "{name:?} accepted");
        }
        assert_eq!(relative(Path::new("../x")), Err("must not contain `..`"));
        assert_eq!(
            relative(Path::new("/x")),
            Err("must be relative to the checkout")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_and_root_names_are_refused() {
        for name in [r"C:\target", r"C:target", r"\target", r"\\server\share\x"] {
            assert!(relative(Path::new(name)).is_err(), "{name:?} accepted");
        }
    }

    #[test]
    fn a_request_is_checked_whole_and_bounded() {
        assert!(check(&[]).is_ok());
        assert!(check(&["target".into(), "node_modules".into()]).is_ok());
        let error = check(&["target".into(), "../up".into()]).unwrap_err();
        assert!(error.contains("../up") && error.contains(".."), "{error}");
        let many: Vec<String> = (0..=MAX_SEEDS).map(|i| format!("d{i}")).collect();
        assert!(check(&many).unwrap_err().contains("at most"));
    }

    #[test]
    fn outcomes_travel_as_snake_case_words() {
        let seeded = Seeded {
            dir: "target".into(),
            outcome: Outcome::Cloned,
            note: None,
        };
        assert_eq!(
            serde_json::to_value(&seeded).unwrap(),
            serde_json::json!({"dir": "target", "outcome": "cloned"})
        );
        for outcome in [
            Outcome::Cloned,
            Outcome::Reflinked,
            Outcome::Copied,
            Outcome::Skipped,
            Outcome::Refused,
            Outcome::Failed,
        ] {
            assert_eq!(
                serde_json::to_value(outcome).unwrap(),
                serde_json::json!(outcome.as_str())
            );
        }
        assert!(Outcome::Reflinked.seeded() && !Outcome::Skipped.seeded());
    }
}
