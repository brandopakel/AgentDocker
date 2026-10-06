//! Explicit Unix dependencies, preserving literal and physical aliases.
use super::*;
use std::collections::BTreeSet;
use std::path::Component;

#[derive(Default, Debug)]
pub(crate) struct References {
    pub(crate) any: bool,
    all: bool,
    versions: BTreeSet<String>,
}

impl From<bool> for References {
    fn from(present: bool) -> Self {
        Self {
            any: present,
            all: present,
            versions: BTreeSet::new(),
        }
    }
}

impl References {
    pub(crate) fn retains(&self, id: &str) -> bool {
        self.all || self.versions.contains(id)
    }

    pub(crate) fn include(&mut self, root: &Path, path: &Path) -> Result<()> {
        ensure!(
            path.is_absolute() && !path.components().any(|c| c == Component::ParentDir),
            "ambiguous service path"
        );
        self.spelling(root, path);
        let mut prefix = PathBuf::new();
        for part in path.components() {
            prefix.push(part.as_os_str());
            let physical = agentdocker_host::project::try_canonical(&prefix)?;
            if physical
                .strip_prefix(root.join("versions"))
                .ok()
                .and_then(|p| p.components().next())
                .and_then(|c| c.as_os_str().to_str())
                .is_some_and(valid_id)
            {
                self.spelling(root, &physical);
            }
        }
        self.spelling(root, &agentdocker_host::project::try_canonical(path)?);
        Ok(())
    }

    fn spelling(&mut self, root: &Path, path: &Path) {
        let Ok(relative) = path.strip_prefix(root) else {
            return;
        };
        self.any = true;
        let mut parts = relative.components();
        match (parts.next(), parts.next()) {
            (Some(first), Some(id))
                if first.as_os_str() == "versions"
                    && id.as_os_str().to_str().is_some_and(valid_id) =>
            {
                self.versions
                    .insert(id.as_os_str().to_str().unwrap().to_owned());
            }
            _ => self.all = true,
        }
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Verify the precise executable location in an immutable owned payload, also
/// for a service installed in another prefix. Never create foreign-store pins.
pub(super) fn known_program(
    path: &Path,
    name: &str,
    verified: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    ensure!(path.is_absolute(), "relative service executable");
    let physical = path.canonicalize()?;
    let versions = physical
        .ancestors()
        .find(|p| p.ends_with(".local/share/agentdocker/desktop/versions"))
        .context("unrecognized service executable")?;
    let id = physical
        .strip_prefix(versions)?
        .components()
        .next()
        .context("missing service version")?;
    ensure!(
        id.as_os_str().to_str().is_some_and(valid_id),
        "unrecognized service version"
    );
    let version = versions.join(id);
    let inside = if cfg!(target_os = "macos") {
        "AgentDocker.app/Contents/MacOS"
    } else {
        "agentdocker-desktop/bin"
    };
    ensure!(
        physical == version.join(inside).join(name),
        "unrecognized service executable location"
    );
    if !verified.contains(&version) {
        let root = versions.parent().context("missing service store")?;
        ensure!(
            super::super::checked_version_at(root, &version)? == installation::LOCK_FORMAT,
            "unrecognized service payload"
        );
        verified.insert(version);
    }
    Ok(())
}

/// Only known scalar settings and explicit absolute resource paths are accepted.
/// Unknown environment semantics must not authorize selective deletion.
pub(super) fn environment_paths(
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for (key, value) in environment {
        match key.as_str() {
            "HOME" | "SHELL" | "TMPDIR" | "TMP" | "TEMP" | "XDG_RUNTIME_DIR"
            | "XDG_CONFIG_HOME" | "XDG_CACHE_HOME" | "XDG_DATA_HOME" | "SSH_AUTH_SOCK"
            | "AGENTDOCKER_HOME" => paths.push(PathBuf::from(value)),
            "PATH" | "XDG_DATA_DIRS" | "XDG_CONFIG_DIRS" => {
                paths.extend(value.split(':').map(PathBuf::from))
            }
            "DBUS_SESSION_BUS_ADDRESS" => {
                if let Some(path) = value.strip_prefix("unix:path=") {
                    let path = path.split(',').next().context("missing bus path")?;
                    ensure!(!path.contains('%'), "escaped bus path");
                    paths.push(path.into());
                } else {
                    ensure!(value.starts_with("unix:abstract="), "unknown bus address");
                }
            }
            "LANG"
            | "LANGUAGE"
            | "LOGNAME"
            | "USER"
            | "RUST_LOG"
            | "OSLogRateLimit"
            | "XPC_SERVICE_NAME"
            | "GSM_SKIP_SSH_AGENT_WORKAROUND" => {}
            key if key.starts_with("LC_") => {}
            _ => bail!("unknown service environment setting"),
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_preserve_both_versions_without_preserving_unrelated_builds() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("store");
        let first = root.join("versions").join("a".repeat(64));
        let second = root.join("versions").join("b".repeat(64));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&second, first.join("alias")).unwrap();
            let mut refs = References::default();
            refs.include(&root, &first.join("alias/future/log"))
                .unwrap();
            assert!(refs.retains(&"a".repeat(64)) && refs.retains(&"b".repeat(64)));
            assert!(!refs.retains(&"c".repeat(64)));
            assert!(!second.join("future").exists());
        }
        let mut refs = References::default();
        refs.include(&root, &root.with_file_name("store-neighbor"))
            .unwrap();
        assert!(!refs.any);
        assert!(refs.include(&root, &first.join("../different")).is_err());
        assert!(refs.include(&root, Path::new("relative")).is_err());
        refs.include(&root, &root.join("versions/unknown/data"))
            .unwrap();
        assert!(refs.retains(&"c".repeat(64)));
    }

    #[test]
    fn foreign_payload_validation_does_not_resolve_its_unrelated_launcher() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp
            .path()
            .canonicalize()
            .unwrap()
            .join(".local/share/agentdocker/desktop");
        let staging = root.join("versions").join("a".repeat(64));
        let payload = staging.join(if cfg!(target_os = "macos") {
            "AgentDocker.app"
        } else {
            "agentdocker-desktop"
        });
        let bin = payload.join(if cfg!(target_os = "macos") {
            "Contents/MacOS/agentd"
        } else {
            "bin/agentd"
        });
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "fixture executable").unwrap();
        let metadata = payload.join(if cfg!(target_os = "macos") {
            "Contents/Resources/build.json"
        } else {
            "build.json"
        });
        std::fs::create_dir_all(metadata.parent().unwrap()).unwrap();
        std::fs::write(
            metadata,
            json!({"format":1,"product":"agentdocker","installation_lock":1}).to_string(),
        )
        .unwrap();
        let id = tree_hash(&payload).unwrap();
        let version = root.join("versions").join(id);
        std::fs::rename(&staging, &version).unwrap();
        std::fs::write(
            root.join("launcher.json"),
            json!({"format":1,"application":"/unrelated/launcher.app"}).to_string(),
        )
        .unwrap();
        let program = version.join(bin.strip_prefix(&staging).unwrap());
        known_program(&program, "agentd", &mut BTreeSet::new()).unwrap();
        std::fs::write(&program, "modified executable").unwrap();
        assert!(known_program(&program, "agentd", &mut BTreeSet::new()).is_err());
    }

    #[test]
    fn unknown_environment_cannot_hide_an_unclassified_resource() {
        use std::collections::BTreeMap;
        let mut env = BTreeMap::from([
            ("HOME".into(), "/profile".into()),
            ("PATH".into(), "/bin:/tools".into()),
        ]);
        assert_eq!(
            environment_paths(&env).unwrap(),
            ["/profile", "/bin", "/tools"].map(PathBuf::from)
        );
        env.insert("DYLD_INSERT_LIBRARIES".into(), "/old/version/plugin".into());
        assert!(environment_paths(&env).is_err());
    }
}
