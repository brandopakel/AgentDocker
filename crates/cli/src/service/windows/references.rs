//! Explicit service filesystem references. Ambiguous paths retain the store.
use anyhow::{Result, ensure};
use std::collections::BTreeSet;
use std::path::{Component, Path};

#[derive(Default, Debug)]
pub(crate) struct References {
    pub(crate) any: bool,
    pub(crate) all: bool,
    pub(crate) versions: BTreeSet<String>,
}

fn normalized(text: &str) -> String {
    let text = text.replace('/', "\\").to_lowercase();
    if let Some(rest) = text.strip_prefix(r"\\?\unc\") {
        format!(r"\\{rest}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }
}

impl References {
    pub(crate) fn unknown() -> Self {
        Self {
            any: true,
            all: true,
            versions: BTreeSet::new(),
        }
    }

    pub(crate) fn retains(&self, id: &str) -> bool {
        self.all || self.versions.contains(&id.to_ascii_lowercase())
    }

    /// Keep literal and physical dependencies, including an alias stored in a
    /// different version from its target. Missing data suffixes are permitted;
    /// unreadable paths and parent traversal require conservative retention.
    pub(crate) fn include(&mut self, root: &Path, path: &Path) -> Result<()> {
        ensure!(
            path.is_absolute() && !path.components().any(|c| c == Component::ParentDir),
            "ambiguous service path"
        );
        let root = normalized(&root.to_string_lossy());
        self.spelling(&root, &normalized(&path.to_string_lossy()));
        // Resolve every prefix: an intermediate alias can itself reside in an
        // old version even when the final target resolves outside the store.
        let mut prefix = std::path::PathBuf::new();
        for component in path.components() {
            prefix.push(component.as_os_str());
            if !prefix.is_absolute() {
                continue;
            }
            let physical = agentdocker_host::project::try_canonical(&prefix)?;
            let spelling = normalized(&physical.to_string_lossy());
            if spelling
                .strip_prefix(&format!("{root}\\versions\\"))
                .and_then(|rest| rest.split('\\').next())
                .is_some_and(|id| id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit()))
            {
                self.spelling(&root, &spelling);
            }
        }
        let physical = agentdocker_host::project::try_canonical(path)?;
        self.spelling(&root, &normalized(&physical.to_string_lossy()));
        Ok(())
    }

    fn spelling(&mut self, root: &str, path: &str) {
        let root = root.trim_end_matches('\\');
        if path == root {
            self.any = true;
            self.all = true;
            return;
        }
        let Some(rest) = path.strip_prefix(&format!("{root}\\")) else {
            return;
        };
        self.any = true;
        let mut parts = rest.split('\\');
        match parts.next() {
            Some("bin") => {} // Stable bootstraps select the protected current release.
            Some("versions") => {
                if let Some(id) = parts
                    .next()
                    .filter(|id| id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit()))
                {
                    self.versions.insert(id.to_owned());
                } else {
                    self.all = true;
                }
            }
            _ => self.all = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_and_windows_alias_spellings_retain_only_named_versions() {
        let root = normalized(r"C:\Users\Test\AgentDocker\desktop");
        let id = "a".repeat(64);
        let mut refs = References::default();
        refs.spelling(
            &root,
            &normalized(r"C:\Users\Test\AgentDocker\desktop-neighbor\versions\a"),
        );
        assert!(!refs.any);
        refs.spelling(
            &root,
            &normalized(r"\\?\C:\Users\Test\AgentDocker\desktop\bin\agentdocker.exe"),
        );
        assert!(refs.any && !refs.all && refs.versions.is_empty());
        refs.spelling(
            &root,
            &normalized(&format!(
                "C:/Users/Test/AgentDocker/desktop/versions/{id}/AgentDocker/agentd.exe"
            )),
        );
        assert!(refs.retains(&id) && !refs.retains(&"b".repeat(64)));
        assert_eq!(
            normalized(r"\\?\UNC\Server\Share\Path"),
            normalized(r"\\server\share\path")
        );
    }

    #[test]
    fn store_level_and_unknown_version_dependencies_are_conservative() {
        for suffix in [
            "",
            "\\versions",
            "\\versions\\unknown\\file",
            "\\state\\log",
        ] {
            let mut refs = References::default();
            refs.spelling(r"c:\store", &format!(r"c:\store{suffix}"));
            assert!(refs.any && refs.all);
        }
    }

    #[test]
    fn native_existing_versions_and_future_data_keep_only_exact_ids() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap().join("store");
        let id = "a".repeat(64);
        let version = root.join("versions").join(&id);
        std::fs::create_dir_all(&version).unwrap();
        let mut refs = References::default();
        refs.include(&root, &version.join("future-state/log"))
            .unwrap();
        assert!(refs.any && refs.retains(&id) && !refs.all);
        assert!(!refs.retains(&"b".repeat(64)));
        assert!(!version.join("future-state").exists());
        refs.include(&root, &root.join("bin/agentdocker.exe"))
            .unwrap();
        assert!(!refs.all);
    }

    #[cfg(unix)]
    #[test]
    fn physical_aliases_and_missing_suffixes_keep_intermediate_versions() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap().join("store");
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let first = root.join("versions").join(&a);
        let second = root.join("versions").join(&b);
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::os::unix::fs::symlink(&second, first.join("alias")).unwrap();
        let mut refs = References::default();
        refs.include(&root, &first.join("alias/future/log"))
            .unwrap();
        assert!(refs.retains(&a) && refs.retains(&b));
        // Ancestor prefixes are traversal dependencies, not whole-store data
        // references: exact named versions must not accidentally retain all.
        assert!(!refs.all);
        assert!(refs.include(&root, &first.join("../other")).is_err());
        assert!(refs.include(&root, Path::new("relative")).is_err());
    }
}
