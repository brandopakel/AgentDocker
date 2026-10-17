//! Bounded, ephemeral evidence for read guards. Persistent read marks remain
//! authoritative; a missing/evicted proof can never make a changed mark fresh.
use agentdocker_core::{AgentId, Attribution, ReadMark};
use agentdocker_host::content::{self, Snapshot};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const CACHE_BYTES: usize = 16 * 1024 * 1024;

struct Cache<K, V> {
    entries: BTreeMap<K, (u64, usize, V)>,
    order: BTreeMap<u64, K>,
    used: usize,
    serial: u64,
}

impl<K, V> Default for Cache<K, V> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            order: BTreeMap::new(),
            used: 0,
            serial: 0,
        }
    }
}

impl<K: Ord + Clone, V> Cache<K, V> {
    fn remove(&mut self, key: &K) {
        if let Some((serial, bytes, _)) = self.entries.remove(key) {
            self.order.remove(&serial);
            self.used -= bytes;
        }
    }

    fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.remove(&key);
        if bytes > CACHE_BYTES {
            return;
        }
        while self.used + bytes > CACHE_BYTES {
            let Some((_, oldest)) = self.order.first_key_value() else {
                break;
            };
            let oldest = oldest.clone();
            self.remove(&oldest);
        }
        // Overflow may discard supplementary evidence, but must not reuse an
        // age and orphan an entry from the eviction index.
        if self.serial == u64::MAX {
            self.entries.clear();
            self.order.clear();
            self.used = 0;
            self.serial = 0;
        }
        self.serial += 1;
        self.order.insert(self.serial, key.clone());
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
    identity: content::EntryProof,
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
        let bytes = snapshot.retained_bytes() + 512 + mark.path.as_os_str().len() * 3;
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
        // Ancestor reads cover this entry; the first ordered key at or after
        // it detects descendant reads. Unrelated agents and paths are never
        // scanned, even while an exclusive checkout lease covers a large batch.
        path.ancestors().any(|ancestor| {
            self.reads
                .entries
                .contains_key(&(agent.clone(), ancestor.to_path_buf()))
        }) || self
            .reads
            .entries
            .range((agent.clone(), path.to_path_buf())..)
            .next()
            .is_some_and(|((reader, read_path), _)| reader == agent && read_path.starts_with(path))
    }

    /// Called only after the matching ledger row was committed. Capturing code
    /// requires the same attribution before and after reading the entry.
    pub(super) fn record(
        &mut self,
        path: PathBuf,
        by: &Attribution,
        identity: Option<content::EntryProof>,
        at: DateTime<Utc>,
    ) {
        self.writes.remove(&path);
        if let (Some(agent), Some(identity)) = (by.agent(), identity) {
            let bytes = 512 + path.as_os_str().len() * 3 + identity.retained_bytes();
            self.writes.insert(
                path,
                WrittenContent {
                    agent: agent.clone(),
                    at,
                    identity,
                },
                bytes,
            );
        }
    }

    fn owns(&self, agent: &AgentId, mark: &ReadMark, relative: &Path, version: &str) -> bool {
        self.writes
            .get(&mark.path.join(relative))
            .is_some_and(|proof| {
                proof.agent == *agent && proof.at >= mark.at && proof.identity.matches(version)
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
    fn replacement_and_removal_keep_eviction_order_and_budget_consistent() {
        let mut cache = Cache::default();
        let half = CACHE_BYTES / 2;
        cache.insert("a", 1, half);
        cache.insert("b", 2, half);
        cache.insert("a", 3, half);
        cache.insert("c", 4, half);
        assert_eq!(cache.get(&"a"), Some(&3));
        assert_eq!(cache.get(&"b"), None, "replacement refreshes a's age");
        assert_eq!(cache.get(&"c"), Some(&4));
        cache.remove(&"a");
        cache.insert("d", 5, half);
        assert_eq!(cache.get(&"c"), Some(&4), "removal freed its budget");
        cache.insert("d", 6, CACHE_BYTES + 1);
        assert_eq!(cache.get(&"d"), None);
        cache.insert("e", 7, CACHE_BYTES);
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"e"), Some(&7));
        assert_eq!(cache.used, CACHE_BYTES);
    }

    #[test]
    fn age_overflow_discards_optional_evidence_without_orphaning_entries() {
        let mut cache = Cache::default();
        cache.insert("old", 1, 1);
        cache.serial = u64::MAX;
        cache.insert("new", 2, CACHE_BYTES);
        assert_eq!(cache.get(&"old"), None);
        assert_eq!(cache.get(&"new"), Some(&2));
        cache.insert("next", 3, CACHE_BYTES);
        assert_eq!(cache.get(&"new"), None);
        assert_eq!(cache.get(&"next"), Some(&3));
        assert_eq!(cache.used, CACHE_BYTES);
    }

    fn observe(proofs: &mut Proofs, agent: &AgentId, path: &Path) {
        let snapshot = Arc::new(Snapshot {
            version: "fixture".to_owned(),
            entries: None,
        });
        proofs.observe(
            agent,
            ReadMark {
                path: path.to_path_buf(),
                at: Utc::now(),
                version: snapshot.version.clone(),
                head: None,
            },
            snapshot,
        );
    }

    #[test]
    fn interest_matches_component_prefixes_without_crossing_agents() {
        let mut proofs = Proofs::default();
        let agents: Vec<_> = ["a", "b", "c"].into_iter().map(AgentId::from).collect();
        let mut reads = Vec::new();
        for (index, agent) in agents[..2].iter().enumerate() {
            for path in [
                "/tree/a",
                "/tree/a-/child",
                "/tree/deep/leaf",
                "relative/leaf",
            ] {
                let path = PathBuf::from(path);
                observe(&mut proofs, agent, &path);
                reads.push((agent.clone(), path));
            }
            for n in 0..512 {
                let path = PathBuf::from(format!("/other/{index}/{n}/leaf"));
                observe(&mut proofs, agent, &path);
                reads.push((agent.clone(), path));
            }
        }
        for agent in &agents {
            for path in [
                "/",
                "/tree",
                "/tree/a",
                "/tree/a/child",
                "/tree/ab",
                "/tree/a-",
                "/tree/a-/child",
                "/tree/deep",
                "/tree/deep/leaf",
                "/tree/deep/leaves",
                "/tree/deep/leaf/child",
                "/tree/z",
                "/other/0/511",
                "/other/1/511",
                "/other/2",
                "relative",
                "relative/leaf",
                "relative/leaf/child",
                "relative/leafy",
                "",
            ] {
                let path = Path::new(path);
                let expected = reads.iter().any(|(reader, read)| {
                    reader == agent && (path.starts_with(read) || read.starts_with(path))
                });
                assert_eq!(
                    proofs.interested(agent, path),
                    expected,
                    "{agent}: {path:?}"
                );
            }
        }
        proofs.forget_read(&agents[0], Path::new("/tree/a"));
        assert!(!proofs.interested(&agents[0], Path::new("/tree/a/child")));
        assert!(proofs.interested(&agents[1], Path::new("/tree/a/child")));
        assert!(proofs.interested(&agents[0], Path::new("/tree/a-")));
    }

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
