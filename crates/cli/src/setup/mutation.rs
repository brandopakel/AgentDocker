//! Coordinate our configuration writers across daemon homes and saved plans.
//! Provider CLIs and editors do not participate; ownership checks still apply.
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use agentdocker_host::{dirs, lock, project};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

pub(crate) struct Guard {
    targets: BTreeSet<PathBuf>,
    _locks: Vec<lock::Lock>,
}

impl Guard {
    pub(crate) fn acquire(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let targets = paths
            .into_iter()
            .map(|path| project::try_canonical(&path))
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        let mut locks = Vec::new();
        if !targets.is_empty() {
            // A fixed per-user namespace deliberately ignores AGENTDOCKER_HOME,
            // TMPDIR and provider profile overrides. Never unlink lock files:
            // replacing the inode would let concurrent writers both acquire it.
            // SAFETY: geteuid has no preconditions.
            let directory = PathBuf::from(format!("/tmp/agentdocker-config-locks-{}", unsafe {
                libc::geteuid()
            }));
            dirs::ensure_private_dir(&directory)?;
            for target in &targets {
                let key = format!("{:x}", Sha256::digest(target.as_os_str().as_bytes()));
                let path = directory.join(format!("{key}.lock"));
                dirs::private_file(&path, true, false)?;
                locks.push(
                    lock::try_exclusive_existing(&path)?.with_context(|| {
                        format!(
                            "another AgentDocker setup operation is editing {}; try again after it finishes",
                            target.display()
                        )
                    })?,
                );
            }
        }
        Ok(Self {
            targets,
            _locks: locks,
        })
    }

    pub(crate) fn covers(&self, path: &Path) -> Result<()> {
        ensure!(
            self.targets.contains(&project::try_canonical(path)?),
            "configuration target changed after locking; create a fresh setup preview"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_contend_missing_targets_are_stable_and_other_profiles_are_independent() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&profile, &alias).unwrap();
        let target = profile.join("config.json");
        let guard = Guard::acquire([target.clone()]).unwrap();
        assert!(Guard::acquire([alias.join("config.json")]).is_err());
        assert!(Guard::acquire([profile.join("other.json")]).is_ok());
        std::fs::write(&target, "{}").unwrap();
        guard.covers(&alias.join("config.json")).unwrap();
        assert!(Guard::acquire([target]).is_err());
        assert!(guard.covers(&profile.join("other.json")).is_err());
    }

    #[test]
    fn failed_multi_target_acquisition_releases_earlier_locks() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("a.json");
        let last = tmp.path().join("z.json");
        let _held = Guard::acquire([last.clone()]).unwrap();
        assert!(Guard::acquire([first.clone(), last]).is_err());
        assert!(Guard::acquire([first]).is_ok());
    }
}
