//! One private record file with an owner: the mechanics under the managed
//! bridge's delivery record and the existing-session receiver's native-queue
//! ledger, each of which keeps its own record and its own meaning. Opening
//! takes the directory's lifetime lock, so one controller owns an agent's
//! delivery at a time; reading is bounded and refuses what is not a private
//! regular file; writing lands in a private temporary file, flushed, and is
//! published over the old record so a reader of the old snapshot keeps it.
use agentdocker_host::{dirs, files, lock};
use anyhow::{Context, Result, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(super) struct Durable {
    _owner: lock::Lock,
    path: PathBuf,
    max_bytes: usize,
    what: &'static str,
}

impl Durable {
    /// Own `directory`'s record for this process's life: `owner.lock` taken
    /// exclusively (`taken` is the refusal when another process holds it),
    /// and the record at `name` read and written under it, at most
    /// `max_bytes` long.
    pub fn open(
        directory: &Path,
        name: &str,
        max_bytes: usize,
        what: &'static str,
        taken: &'static str,
    ) -> Result<Self> {
        let lock_path = directory.join("owner.lock");
        dirs::private_file(&lock_path, true, false)?;
        let owner = lock::try_exclusive_existing(&lock_path)?.context(taken)?;
        Ok(Self {
            _owner: owner,
            path: directory.join(name),
            max_bytes,
            what,
        })
    }

    /// Where the record lives, for a test that inspects the bytes.
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The record as last published, or `None` when none has been.
    pub fn read<R: DeserializeOwned>(&self) -> Result<Option<R>> {
        match dirs::read_private_file(&self.path) {
            Ok(file) => bounded(file, self.max_bytes, self.what).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Publish `record` as the record. The destination is checked before it
    /// is replaced (the directory and its lock are shared by every writer of
    /// this kind); the bytes land in a private temporary file beside it,
    /// flushed, and take its place atomically. Nothing in memory changes
    /// here: a caller moves its own copy only once this has returned.
    pub fn publish<R: Serialize>(&self, record: &R) -> Result<()> {
        let bytes = serde_json::to_vec(record)?;
        ensure!(
            bytes.len() <= self.max_bytes,
            "{} exceeds its size limit",
            self.what
        );
        match dirs::read_private_file(&self.path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let directory = self
            .path
            .parent()
            .with_context(|| format!("{} has no directory", self.what))?;
        let mut temporary =
            tempfile::Builder::new().make_in(directory, dirs::create_private_file)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        // Windows cannot open a directory as an ordinary file to sync it; the
        // shared publisher keeps a concurrent reader of the prior record whole.
        files::publish_snapshot(&temporary.into_temp_path(), &self.path)?;
        Ok(())
    }
}

/// A record read without owning it, for an inspection that must neither
/// take the lock nor change the file: at most `max_bytes` of a private
/// snapshot.
pub(super) fn snapshot<R: DeserializeOwned>(
    path: &Path,
    max_bytes: usize,
    what: &'static str,
) -> Result<R> {
    bounded(dirs::open_private_snapshot(path)?, max_bytes, what)
}

fn bounded<R: DeserializeOwned>(file: std::fs::File, max_bytes: usize, what: &str) -> Result<R> {
    let mut data = Vec::new();
    file.take((max_bytes + 1) as u64).read_to_end(&mut data)?;
    ensure!(data.len() <= max_bytes, "{what} exceeds its size limit");
    serde_json::from_slice(&data).with_context(|| format!("cannot read the retained {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_owner_at_a_time_bounded_reads_and_atomic_publication() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("records");
        dirs::secure_state_dir(&directory).unwrap();
        let file = Durable::open(&directory, "record.json", 64, "test record", "taken").unwrap();
        let second = Durable::open(&directory, "record.json", 64, "test record", "taken");
        assert!(
            second
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("taken"))
        );
        drop(second);
        assert_eq!(file.read::<Vec<u32>>().unwrap(), None);
        file.publish(&vec![1_u32, 2, 3]).unwrap();
        assert_eq!(file.read::<Vec<u32>>().unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(
            snapshot::<Vec<u32>>(file.path(), 64, "test record").unwrap(),
            vec![1, 2, 3]
        );
        let too_long = (0..100).collect::<Vec<u32>>();
        let refused = file.publish(&too_long).unwrap_err().to_string();
        assert!(refused.contains("exceeds its size limit"), "{refused}");
        assert_eq!(
            file.read::<Vec<u32>>().unwrap(),
            Some(vec![1, 2, 3]),
            "a refused publication leaves the record as it was"
        );
        assert!(
            snapshot::<Vec<u32>>(file.path(), 4, "test record")
                .unwrap_err()
                .to_string()
                .contains("exceeds its size limit")
        );
        drop(file);
        let again = Durable::open(&directory, "record.json", 64, "test record", "taken").unwrap();
        assert_eq!(again.read::<Vec<u32>>().unwrap(), Some(vec![1, 2, 3]));
    }
}
