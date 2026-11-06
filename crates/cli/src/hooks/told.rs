//! The memories a session has been told, so a prompt tells it only what it
//! has not had: the ids its start's profile carried, then those each
//! prompt's recall added, one per line in `<home>/hooks/told/<agent>`.
//! A session start begins the list again, as its context does. The list is
//! what reached the session, not a profile read again later, so a memory
//! kept or backdated since, or one the start's byte bound left out, is
//! still told once it bears on a prompt. A hook's answer carries what it
//! tells as a [`Telling`], written to the list only once the answer is
//! delivered: Claude Code's and Codex's when the hook's output is written,
//! OpenCode's when the plugin reports the turn that carried it completed.
//! An answer that is lost, cut short or replaced tells nothing, and the
//! next prompt may say the same memories again.
//!
//! Every step fails open: a list that cannot be read is empty (a memory may
//! be told twice), one that cannot be written is left as it was, and no
//! AgentDocker home is made for it.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A list nobody has written to for this long belongs to a session that
/// is over; a session start clears those out.
const KEPT: Duration = Duration::from_secs(14 * 24 * 60 * 60);

pub(crate) struct Told {
    path: PathBuf,
}

impl Told {
    /// The list for `agent` under AgentDocker's `home`; none when there is
    /// no home yet or the id is not one a file name can carry as it is.
    pub(crate) fn at(home: &Path, agent: &str) -> Option<Self> {
        let plain = !agent.is_empty()
            && agent.len() <= 128
            && agent
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        (plain && home.is_dir()).then(|| Self {
            path: home.join("hooks").join("told").join(agent),
        })
    }

    /// The ids told so far.
    pub(crate) fn read(&self) -> HashSet<String> {
        std::fs::read_to_string(&self.path)
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// A session start told `ids`: the list begins again with them, and
    /// lists of sessions long over are removed.
    pub(crate) fn start(&self, ids: &[String]) {
        let Some(dir) = self.path.parent() else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let _ = std::fs::write(&self.path, lines(ids));
        let now = SystemTime::now();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > KEPT));
            if old && entry.path() != self.path {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// A prompt told `ids` as well.
    pub(crate) fn add(&self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let Some(dir) = self.path.parent() else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(lines(ids).as_bytes());
        }
    }
}

/// What one hook answer tells a session: its agent's memories by id, and
/// whether it is the session's start, which begins the list again.
#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq)]
pub struct Telling {
    pub agent: String,
    #[serde(default)]
    pub start: bool,
    #[serde(default)]
    pub memories: Vec<String>,
}

impl Telling {
    /// The session reached by this answer has been told its memories.
    pub(crate) fn delivered(&self, home: &Path) {
        let Some(told) = Told::at(home, &self.agent) else {
            return;
        };
        if self.start {
            told.start(&self.memories);
        } else {
            told.add(&self.memories);
        }
    }
}

/// One id a line; an id that could break its line, as one handed back by
/// a plugin might, is no memory's and is left out.
fn lines(ids: &[String]) -> String {
    ids.iter()
        .filter(|id| !id.is_empty() && !id.contains(['\n', '\r']))
        .map(|id| format!("{id}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_session_start_begins_the_list_and_prompts_add_to_it() {
        let home = tempfile::tempdir().unwrap();
        let told = Told::at(home.path(), "agent-1").unwrap();
        assert!(told.read().is_empty(), "nothing told yet");
        told.start(&ids(&["m1", "m2"]));
        told.add(&ids(&["m3"]));
        assert_eq!(told.read(), ["m1", "m2", "m3"].map(String::from).into());
        told.add(&ids(&["m5\nm6", ""]));
        assert_eq!(told.read().len(), 3, "no id splits into two");
        told.start(&ids(&["m4"]));
        assert_eq!(told.read(), ["m4"].map(String::from).into(), "a new start");
        let other = Told::at(home.path(), "agent-2").unwrap();
        assert!(other.read().is_empty(), "each session its own");
    }

    #[test]
    fn a_delivered_telling_writes_the_list_and_a_start_begins_it_again() {
        let home = tempfile::tempdir().unwrap();
        let told = Told::at(home.path(), "agent-1").unwrap();
        let telling = |start: bool, memories: &[&str]| Telling {
            agent: "agent-1".into(),
            start,
            memories: ids(memories),
        };
        telling(false, &["m1"]).delivered(home.path());
        telling(false, &["m2"]).delivered(home.path());
        assert_eq!(told.read(), ["m1", "m2"].map(String::from).into());
        telling(true, &["m3"]).delivered(home.path());
        assert_eq!(told.read(), ["m3"].map(String::from).into());
        // Nothing for a home that is not there, or an id no file can name.
        telling(false, &["m4"]).delivered(&home.path().join("missing"));
        Telling {
            agent: "../agent-1".into(),
            ..telling(false, &["m5"])
        }
        .delivered(home.path());
        assert_eq!(told.read(), ["m3"].map(String::from).into());
        assert!(!home.path().join("missing").exists());
    }

    #[test]
    fn no_list_without_a_home_or_for_an_id_a_path_could_misread() {
        let home = tempfile::tempdir().unwrap();
        assert!(Told::at(&home.path().join("missing"), "agent-1").is_none());
        for agent in ["", "../agent", "a/b", "a\\b", "a.b"] {
            assert!(Told::at(home.path(), agent).is_none(), "{agent:?}");
        }
    }

    #[test]
    fn a_start_clears_lists_of_sessions_long_over() {
        let home = tempfile::tempdir().unwrap();
        let stale = Told::at(home.path(), "gone").unwrap();
        stale.start(&ids(&["m1"]));
        let recent = Told::at(home.path(), "recent").unwrap();
        recent.start(&ids(&["m2"]));
        let long_ago = SystemTime::now() - KEPT - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&stale.path)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        Told::at(home.path(), "new").unwrap().start(&ids(&["m3"]));
        assert!(!stale.path.exists(), "a list nobody wrote for weeks");
        assert!(recent.path.exists());
    }
}
