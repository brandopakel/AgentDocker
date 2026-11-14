//! The connector's state file, `state.json`, and the one lock every read
//! that authorizes and every change to it is made under.
//!
//! Two processes touch the file: the serving connector, the only one that
//! creates and rotates grants and registers clients, and `connector
//! revoke`, which only ends a grant. Each takes `state.lock` (beside the
//! file) before it reads, holds it while it decides and changes, and lets
//! it go only once what it wrote is durable: the file staged, synced and
//! renamed over the old one, the directory synced. So a revocation is
//! never overwritten by a writer that read the file before it, and a
//! token is never handed out from a grant the file says is revoked.
//!
//! Whatever goes wrong refuses: a lock that cannot be had in
//! [`LOCK_WAIT`], a file that cannot be read, does not parse, or is gone
//! once the server has made it, a write that does not land. Nothing is
//! authorized or reported done from a view that could be stale.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::oauth::Store;

/// How long a reader or writer waits for another to finish with the file.
/// Each holds it for one read and one write, a few milliseconds.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);

/// What kept a read or a write of the state from happening.
#[derive(Debug)]
pub enum StateError {
    /// Another process held the lock past the wait.
    Busy,
    /// The lock file could not be opened or locked.
    Lock(std::io::Error),
    /// The file is not there, though it should be.
    Missing,
    /// The file could not be read.
    Unreadable(std::io::Error),
    /// The file does not parse as a connector state.
    Corrupt(serde_json::Error),
    /// The new state could not be written: the old file stands.
    Publish(std::io::Error),
    /// The new state replaced the file, so every reader sees it, but its
    /// directory could not be synced: a crash could still undo it.
    Unsynced(std::io::Error),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => write!(
                f,
                "another process held the connector state's lock for more than {} s",
                LOCK_WAIT.as_secs()
            ),
            Self::Lock(error) => write!(f, "cannot lock the connector state: {error}"),
            Self::Missing => write!(f, "the connector state file is gone"),
            Self::Unreadable(error) => write!(f, "cannot read the connector state: {error}"),
            Self::Corrupt(error) => write!(f, "the connector state does not parse: {error}"),
            Self::Publish(error) => write!(f, "cannot save the connector state: {error}"),
            Self::Unsynced(error) => write!(
                f,
                "the connector state was replaced but cannot be made durable: {error}"
            ),
        }
    }
}

impl std::error::Error for StateError {}

/// `state.json` and the lock beside it.
#[derive(Clone, Debug)]
pub struct StateFile {
    path: PathBuf,
    lock: PathBuf,
    wait: Duration,
}

/// The lock on the state, held while it lives.
pub struct Held(#[allow(dead_code)] agentdocker_host::lock::Lock);

impl StateFile {
    pub fn at(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            lock: path.with_file_name("state.lock"),
            wait: LOCK_WAIT,
        }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The same file, waiting `wait` for the lock rather than [`LOCK_WAIT`].
    #[cfg(test)]
    pub fn waiting(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// Take the lock, waiting at most the wait for another holder.
    pub fn lock(&self) -> Result<Held, StateError> {
        let parent = self.path.parent().ok_or(StateError::Missing)?;
        std::fs::create_dir_all(parent).map_err(StateError::Lock)?;
        let deadline = Instant::now() + self.wait;
        loop {
            match agentdocker_host::lock::try_exclusive(&self.lock) {
                Ok(Some(lock)) => return Ok(Held(lock)),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(None) => return Err(StateError::Busy),
                Err(error) => return Err(StateError::Lock(error)),
            }
        }
    }

    /// The file as it is now, read under the lock. A file that is not
    /// there is an empty state only when `missing_is_empty`: at the
    /// server's first start, or for a command with nothing to find.
    pub fn read(&self, _held: &Held, missing_is_empty: bool) -> Result<Store, StateError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(StateError::Corrupt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if missing_is_empty {
                    Ok(Store::default())
                } else {
                    Err(StateError::Missing)
                }
            }
            Err(error) => Err(StateError::Unreadable(error)),
        }
    }

    /// Write the store whole and make it durable before returning: staged
    /// beside the file, readable by its owner only, synced, renamed over
    /// the file, the directory synced. A failure before the rename leaves
    /// the old file ([`StateError::Publish`]); one after it leaves the new
    /// file in place, not yet durable ([`StateError::Unsynced`]).
    pub fn publish(&self, _held: &Held, store: &Store) -> Result<(), StateError> {
        let parent = self.path.parent().ok_or(StateError::Missing)?;
        let staged = parent.join(format!(".state.{}.tmp", std::process::id()));
        let written = (|| {
            let bytes = serde_json::to_vec_pretty(store).map_err(std::io::Error::other)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&staged)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(&bytes)?;
            file.sync_all()
        })();
        // Unix renames, then syncs the directory, so a failure is known to
        // fall before the file was replaced or after; Windows moves it
        // written through, in one step.
        #[cfg(unix)]
        let replaced = written.and_then(|()| std::fs::rename(&staged, &self.path));
        #[cfg(not(unix))]
        let replaced =
            written.and_then(|()| agentdocker_host::files::publish_staged(&staged, &self.path));
        if let Err(error) = replaced {
            let _ = std::fs::remove_file(&staged);
            return Err(StateError::Publish(error));
        }
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|folder| folder.sync_all())
            .map_err(StateError::Unsynced)?;
        Ok(())
    }

    /// The state at a server's start, the file made if there was none:
    /// from then on a missing file is a fault, never an empty state.
    pub fn initialize(&self) -> Result<Store, StateError> {
        let held = self.lock()?;
        let store = self.read(&held, true)?;
        self.publish(&held, &store)?;
        Ok(store)
    }

    /// End the grant `reference` names, as `connector revoke` does: under
    /// the lock, from the file as it is now, changing only that grant's
    /// revocation and publishing durably before saying so. A grant already
    /// revoked keeps the moment it was, and is published again all the
    /// same: an earlier revoke may have replaced the file without making
    /// it durable, and only a publish that completes says it is.
    pub fn revoke(&self, reference: &str, now: DateTime<Utc>) -> Result<Revoked, RevokeError> {
        let held = self.lock()?;
        let mut store = self.read(&held, true)?;
        let Some((grant_id, grant)) = store.grant_for_agent(reference) else {
            return Err(RevokeError::NoGrant(reference.to_owned()));
        };
        let already = grant.revoked_at();
        let revoked = Revoked {
            agent_id: grant.agent_id.clone(),
            agent_name: grant.agent_name.clone(),
            at: already.unwrap_or(now),
            already: already.is_some(),
            grant_id,
        };
        store.revoke(&revoked.grant_id, now);
        match self.publish(&held, &store) {
            Ok(()) => Ok(revoked),
            // The old file stands, and it had no revocation.
            Err(error @ StateError::Publish(_)) if !revoked.already => {
                Err(RevokeError::State(error))
            }
            // The file holds the revocation, written now or before, and
            // nothing says it is durable.
            Err(error) => Err(RevokeError::NotDurable(Box::new(revoked), error)),
        }
    }
}

/// A grant `connector revoke` ended, or found ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revoked {
    pub grant_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub at: DateTime<Utc>,
    /// It was revoked before this call, at `at`.
    pub already: bool,
}

/// Why `connector revoke` ended nothing.
#[derive(Debug)]
pub enum RevokeError {
    NoGrant(String),
    /// Nothing was revoked.
    State(StateError),
    /// The file holds the revocation, so the serving connector refuses the
    /// grant, but it is not known to be durable: a crash could undo it.
    NotDurable(Box<Revoked>, StateError),
}

impl From<StateError> for RevokeError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}

impl std::fmt::Display for RevokeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGrant(reference) => write!(
                f,
                "no connector agent named `{reference}`; see `agentdocker connector grants`"
            ),
            Self::State(error) => write!(f, "nothing was revoked: {error}"),
            Self::NotDurable(revoked, error) => write!(
                f,
                "{} ({}): the revocation is written but not confirmed durable, so a crash could undo it ({error}). The daemon was not asked to finish the agent. Run `agentdocker connector revoke {}` again once the connector's state can be saved",
                revoked.agent_name, revoked.agent_id, revoked.agent_id
            ),
        }
    }
}

impl std::error::Error for RevokeError {}
