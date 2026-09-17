//! Text saved between windows. Delivery state is deliberately window-local:
//! restored text is editable and is never submitted automatically.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};

pub const MAX_TEXT_CHARS: usize = 16_000;
pub const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PER_KIND: usize = 128;
const MAX_KEY_BYTES: usize = 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub(crate) version: u32,
    pub sessions: BTreeMap<String, String>,
    pub conversations: BTreeMap<String, String>,
    pub channels: BTreeMap<String, String>,
    #[serde(default)]
    pub answers: BTreeMap<String, String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            version: 2,
            sessions: BTreeMap::new(),
            conversations: BTreeMap::new(),
            channels: BTreeMap::new(),
            answers: BTreeMap::new(),
        }
    }
}

impl Snapshot {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == 2 || (self.version == 1 && self.answers.is_empty()),
            "Unsupported saved-draft version"
        );
        let mut total = 0usize;
        for entries in [
            &self.sessions,
            &self.conversations,
            &self.channels,
            &self.answers,
        ] {
            anyhow::ensure!(entries.len() <= MAX_PER_KIND, "Too many saved drafts");
            for (key, text) in entries {
                anyhow::ensure!(
                    !key.is_empty() && key.len() <= MAX_KEY_BYTES,
                    "Invalid draft destination"
                );
                anyhow::ensure!(
                    text.chars().count() <= MAX_TEXT_CHARS,
                    "A draft exceeds 16,000 characters"
                );
                total += text.len();
            }
        }
        anyhow::ensure!(
            total <= MAX_TOTAL_BYTES,
            "Draft storage is full. Finish or clear an earlier draft first"
        );
        Ok(())
    }

    pub fn load(home: &Path) -> anyhow::Result<Self> {
        let path = home.join("drafts.json");
        // symlink_metadata also sees a dangling symlink, which must be refused.
        match path.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let mut bytes = Vec::new();
        agentdocker_host::dirs::private_file(&path, false, false)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_FILE_BYTES,
            "Saved drafts exceed 32 MiB"
        );
        let mut saved: Self = serde_json::from_slice(&bytes)?;
        saved.validate()?;
        saved.version = 2;
        Ok(saved)
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        self.validate()?;
        agentdocker_host::dirs::secure_state_dir(home)?;
        let target = home.join("drafts.json");
        match target.symlink_metadata() {
            Ok(_) => {
                agentdocker_host::dirs::private_file(&target, false, false)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let bytes = serde_json::to_vec(self)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_FILE_BYTES,
            "Saved drafts exceed 32 MiB"
        );
        let mut file = tempfile::NamedTempFile::new_in(home)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(&target)?;
        // Persist the rename as well as its contents on supported Unix hosts.
        #[cfg(unix)]
        std::fs::File::open(home)?.sync_all()?;
        Ok(())
    }
}

/// One save in flight. An old completion cannot mark newer text saved.
#[derive(Default)]
pub struct Persistence {
    pub readable: bool,
    pub error: Option<String>,
    pub close_blocked: bool,
    pub discard_on_close: bool,
    saving: bool,
    generation: u64,
    saved_generation: u64,
    first_change: Option<Instant>,
    last_change: Option<Instant>,
}

impl Persistence {
    pub fn loaded() -> Self {
        Self {
            readable: true,
            ..Default::default()
        }
    }
    pub fn unavailable(error: String) -> Self {
        Self {
            error: Some(error),
            ..Default::default()
        }
    }
    pub fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let now = Instant::now();
        self.first_change.get_or_insert(now);
        self.last_change = Some(now);
    }

    pub fn clean(&self) -> bool {
        !self.saving && self.generation == self.saved_generation
    }

    pub fn begin(&mut self, closing: bool) -> Option<u64> {
        let due = closing
            || self
                .first_change
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(1))
            || self
                .last_change
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(250));
        if !self.readable || self.error.is_some() || self.saving || self.clean() || !due {
            return None;
        }
        self.saving = true;
        Some(self.generation)
    }

    pub fn complete(&mut self, generation: u64, result: Result<(), String>) {
        self.saving = false;
        match result {
            Ok(()) => {
                self.saved_generation = generation;
                self.first_change = (!self.clean()).then(Instant::now);
            }
            Err(error) => {
                self.error = Some(format!(
                    "Drafts could not be saved: {error}. Your text is still in this window."
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_destinations_round_trip_and_refused_writes_preserve_the_file() {
        let home = tempfile::tempdir().unwrap();
        let mut saved = Snapshot::default();
        saved
            .sessions
            .insert("agent-a".into(), "session draft".into());
        saved
            .conversations
            .insert("dm:a:b".into(), "你好\nfirst line".into());
        saved
            .conversations
            .insert("dm:a:b/thread".into(), "thread draft".into());
        saved.channels.insert("legacy-room".into(), "  ".into());
        saved.save(home.path()).unwrap();
        assert_eq!(Snapshot::load(home.path()).unwrap(), saved);
        let before = std::fs::read(home.path().join("drafts.json")).unwrap();
        let mut oversized = saved.clone();
        oversized
            .sessions
            .insert("another".into(), "x".repeat(MAX_TEXT_CHARS + 1));
        assert!(oversized.save(home.path()).is_err());
        assert_eq!(
            std::fs::read(home.path().join("drafts.json")).unwrap(),
            before
        );
        let mut full = Snapshot::default();
        for i in 0..128 {
            full.sessions
                .insert(i.to_string(), "x".repeat(MAX_TEXT_CHARS));
            full.conversations
                .insert(i.to_string(), "x".repeat(MAX_TEXT_CHARS));
            full.channels
                .insert(i.to_string(), "x".repeat(MAX_TEXT_CHARS));
        }
        assert!(full.save(home.path()).is_err());
        assert_eq!(
            std::fs::read(home.path().join("drafts.json")).unwrap(),
            before
        );
        std::fs::write(home.path().join("drafts.json"), b"not json").unwrap();
        assert!(Snapshot::load(home.path()).is_err());
        assert_eq!(
            std::fs::read(home.path().join("drafts.json")).unwrap(),
            b"not json"
        );
    }

    #[test]
    fn version_one_text_is_loaded_without_rewriting_and_unknown_versions_are_preserved() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("drafts.json");
        let original = br#"{"version":1,"sessions":{"a":"keep"},"conversations":{},"channels":{}}"#;
        std::fs::write(&file, original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut saved = Snapshot::load(home.path()).unwrap();
        assert_eq!(saved.sessions["a"], "keep");
        assert!(saved.answers.is_empty());
        assert_eq!(std::fs::read(&file).unwrap(), original);
        saved.answers.insert("question".into(), "not sent".into());
        saved.save(home.path()).unwrap();
        assert_eq!(Snapshot::load(home.path()).unwrap(), saved);
        let unknown =
            br#"{"version":99,"sessions":{},"conversations":{},"channels":{},"answers":{}}"#;
        std::fs::write(&file, unknown).unwrap();
        assert!(Snapshot::load(home.path()).is_err());
        assert_eq!(std::fs::read(file).unwrap(), unknown);
    }

    #[test]
    fn close_waits_for_the_latest_save_and_failure_can_be_retried() {
        let mut state = Persistence {
            readable: true,
            ..Default::default()
        };
        state.changed();
        let first = state.begin(true).unwrap();
        state.changed();
        assert!(state.begin(true).is_none());
        state.complete(first, Ok(()));
        assert!(!state.clean());
        let latest = state.begin(true).unwrap();
        state.complete(latest, Err("disk full".into()));
        assert!(!state.clean());
        assert!(state.begin(true).is_none());
        state.error = None;
        let retry = state.begin(true).unwrap();
        assert_eq!(retry, latest);
        state.complete(retry, Ok(()));
        assert!(state.clean());
    }
}
