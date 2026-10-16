//! Bounded, ephemeral evidence for read guards. Persistent read marks remain
//! authoritative; a missing/evicted proof can never make a changed mark fresh.
use agentdocker_core::{AgentId, Attribution, ReadMark};
use agentdocker_host::content::{self, Snapshot};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const CACHE_BYTES: usize = 16 * 1024 * 1024;

struct Cache<K, V> {
    entries: HashMap<K, (u64, usize, V)>,
    used: usize,
    serial: u64,
}

impl<K, V> Default for Cache<K, V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            used: 0,
            serial: 0,
        }
    }
}

impl<K: Eq + Hash + Clone, V> Cache<K, V> {
    fn remove(&mut self, key: &K) {
        if let Some((_, bytes, _)) = self.entries.remove(key) {
            self.used -= bytes;
        }
    }

    fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.remove(&key);
        if bytes > CACHE_BYTES {
            return;
        }
        while self.used + bytes > CACHE_BYTES {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (age, _, _))| age)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            self.remove(&oldest);
        }
        self.serial = self.serial.saturating_add(1);
        self.entries.insert(key, (self.serial, bytes, value));
        self.used += bytes;
    }

    fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key).map(|(_, _, value)| value)
    }
}

struct ReadContent {
    mark: ReadMark,
    snapshot: Arc<Snapshot>,
}

struct WrittenContent {
    agent: AgentId,
    at: DateTime<Utc>,
    version: String,
}

#[derive(Default)]
pub(super) struct Proofs {
    reads: Cache<(AgentId, PathBuf), ReadContent>,
    writes: Cache<PathBuf, WrittenContent>,
}

impl Proofs {
    pub(super) fn forget_read(&mut self, agent: &AgentId, path: &Path) {
        self.reads.remove(&(agent.clone(), path.to_path_buf()));
    }

    pub(super) fn observe(&mut self, agent: &AgentId, mark: ReadMark, snapshot: Arc<Snapshot>) {
        let bytes = snapshot.retained_bytes() + 512 + mark.path.as_os_str().len() * 2;
        self.reads.insert(
            (agent.clone(), mark.path.clone()),
            ReadContent { mark, snapshot },
            bytes,
        );
    }

    pub(super) fn baseline(&self, agent: &AgentId, mark: &ReadMark) -> Option<Arc<Snapshot>> {
        self.reads
            .get(&(agent.clone(), mark.path.clone()))
            .filter(|read| read.mark == *mark)
            .map(|read| read.snapshot.clone())
    }

    pub(super) fn interested(&self, agent: &AgentId, path: &Path) -> bool {
        self.reads
            .entries
            .iter()
            .any(|((reader, _), (_, _, read))| {
                reader == agent
                    && (path.starts_with(&read.mark.path) || read.mark.path.starts_with(path))
            })
    }

    /// Called only after the matching ledger row was committed. Capturing code
    /// requires the same attribution before and after reading the entry.
    pub(super) fn record(
        &mut self,
        path: PathBuf,
        by: &Attribution,
        version: Option<String>,
        at: DateTime<Utc>,
    ) {
        self.writes.remove(&path);
        if let (Some(agent), Some(version)) = (by.agent(), version) {
            let bytes = 512 + path.as_os_str().len() * 2 + version.len() * 2;
            self.writes.insert(
                path,
                WrittenContent {
                    agent: agent.clone(),
                    at,
                    version,
                },
                bytes,
            );
        }
    }

    fn owns(&self, agent: &AgentId, mark: &ReadMark, relative: &Path, version: &str) -> bool {
        self.writes
            .get(&mark.path.join(relative))
            .is_some_and(|proof| {
                proof.agent == *agent && proof.at >= mark.at && proof.version == version
            })
    }

    pub(super) fn explains(
        &self,
        agent: &AgentId,
        mark: &ReadMark,
        changes: &[(PathBuf, String)],
        current: &Snapshot,
    ) -> bool {
        changes.iter().all(|(path, version)| {
            if self.owns(agent, mark, path, version) {
                return true;
            }
            if version != content::MISSING {
                return false;
            }
            // Removing a directory removes its descendants too. It must still
            // be absent (or replaced by a non-directory) in the current snapshot;
            // a recreated directory cannot borrow an earlier removal proof.
            path.ancestors().skip(1).any(|ancestor| {
                let version = current.entry(ancestor);
                version != "directory" && self.owns(agent, mark, ancestor, version)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_eviction_and_oversize_proofs_leave_unknown_content() {
        let mut cache = Cache::default();
        cache.insert("old", 1, CACHE_BYTES / 2);
        cache.insert("other", 2, CACHE_BYTES / 2);
        cache.insert("new", 3, CACHE_BYTES / 2);
        assert_eq!(cache.get(&"old"), None);
        assert_eq!(cache.get(&"other"), Some(&2));
        assert_eq!(cache.get(&"new"), Some(&3));
        cache.insert("new", 4, CACHE_BYTES + 1);
        assert_eq!(cache.get(&"new"), None);
        assert!(cache.used <= CACHE_BYTES);
    }
}
