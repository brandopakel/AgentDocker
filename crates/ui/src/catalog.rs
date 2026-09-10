//! Local project bookmarks. Physical roots join pinned and discovered projects.
use agentdocker_core::ProjectRef;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_PROJECTS: usize = 512;
const MAX_STATE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub project: ProjectRef,
    pub pinned: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Catalog {
    pub projects: Vec<Entry>,
    pub selected: Option<PathBuf>,
    pub dark: bool,
    pub unassigned: bool,
    pub appearance: Option<crate::theme::Settings>,
}

impl Catalog {
    pub fn load(home: &Path) -> anyhow::Result<Self> {
        let path = home.join("workspace.json");
        if !path.try_exists()? {
            return Ok(Self::default());
        }
        let mut bytes = Vec::new();
        agentdocker_host::dirs::private_file(&path, false, false)?
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_STATE_BYTES,
            "Workspace preferences exceed 2 MiB"
        );
        let mut catalog: Self = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            catalog.projects.len() <= MAX_PROJECTS,
            "Too many saved projects"
        );
        catalog
            .projects
            .retain(|entry| entry.project.root.is_absolute());
        catalog
            .projects
            .sort_by(|a, b| a.project.root.cmp(&b.project.root));
        catalog
            .projects
            .dedup_by(|a, b| a.project.root == b.project.root);
        catalog.sort_projects();
        if catalog.selected.is_some() && catalog.selected().is_none() {
            catalog.selected = catalog.projects.first().map(|e| e.project.root.clone());
        }
        Ok(catalog)
    }

    pub fn remember(&mut self, project: ProjectRef, pin: bool) -> bool {
        if !project.root.is_absolute() {
            return false;
        }
        if let Some(entry) = self
            .projects
            .iter_mut()
            .find(|p| p.project.root == project.root)
        {
            let before = entry.clone();
            // Discovery lacks the fingerprint supplied by registered sessions.
            if project.fingerprint.is_some() || entry.project.fingerprint.is_none() {
                entry.project = project;
                entry.project.worktree = None;
            }
            entry.pinned |= pin;
            return *entry != before;
        }
        if self.projects.len() >= MAX_PROJECTS {
            return false;
        }
        let mut project = project;
        project.worktree = None;
        self.projects.push(Entry {
            project,
            pinned: pin,
        });
        self.sort_projects();
        if self.selected.is_none() && !self.unassigned {
            self.selected = Some(self.projects[0].project.root.clone());
        }
        true
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.projects
            .iter()
            .find(|e| Some(&e.project.root) == self.selected.as_ref())
    }

    fn sort_projects(&mut self) {
        self.projects.sort_by(|a, b| {
            a.project
                .name()
                .cmp(&b.project.name())
                .then_with(|| a.project.root.cmp(&b.project.root))
        });
    }

    pub fn pin(&mut self, project: ProjectRef) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.projects.len() < MAX_PROJECTS
                || self.projects.iter().any(|e| e.project.root == project.root),
            "The workspace holds at most 512 projects; forget an old project first"
        );
        anyhow::ensure!(
            project.root.is_absolute(),
            "Project folders must be absolute"
        );
        self.unassigned = false;
        self.selected = Some(project.root.clone());
        self.remember(project, true);
        Ok(())
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        agentdocker_host::dirs::secure_state_dir(home)?;
        let target = home.join("workspace.json");
        // Refuse a symlink/foreign file before replacing an existing preference.
        if target.symlink_metadata().is_ok() {
            agentdocker_host::dirs::private_file(&target, false, false)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_STATE_BYTES,
            "Workspace preferences exceed 2 MiB"
        );
        let mut tmp = tempfile::NamedTempFile::new_in(home)?;
        tmp.write_all(&bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(&target)?;
        Ok(())
    }
}

pub fn resolve(folder: &Path) -> anyhow::Result<ProjectRef> {
    anyhow::ensure!(folder.is_dir(), "Choose an existing project folder");
    Ok(agentdocker_host::project::discover(folder))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_name_order_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::default();
        for suffix in ["a-parent/z-project", "z-parent/a-project"] {
            let folder = dir.path().join(suffix);
            std::fs::create_dir_all(&folder).unwrap();
            catalog.pin(resolve(&folder).unwrap()).unwrap();
        }
        assert_eq!(catalog.projects[0].project.name(), "a-project");
        let home = dir.path().join("state");
        catalog.save(&home).unwrap();
        assert_eq!(Catalog::load(&home).unwrap(), catalog);
    }

    #[test]
    fn a_pin_and_later_discovery_share_identity_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let home = dir.path().join("state");
        std::fs::create_dir(&project).unwrap();
        let mut catalog = Catalog::default();
        catalog.pin(resolve(&project).unwrap()).unwrap();
        let mut discovered = agentdocker_host::project::discover(&project);
        discovered.fingerprint = Some("from-daemon".into());
        catalog.remember(discovered.clone(), false);
        assert_eq!(catalog.projects.len(), 1);
        assert!(catalog.projects[0].pinned);
        assert_eq!(catalog.selected().unwrap().project.id(), discovered.id());
        catalog.save(&home).unwrap();
        std::fs::remove_dir(&project).unwrap();
        assert_eq!(
            Catalog::load(&home).unwrap(),
            catalog,
            "Quiet or unavailable projects retain their selection"
        );
    }
    #[cfg(unix)]
    #[test]
    fn aliases_and_linked_checkouts_do_not_duplicate_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let mut catalog = Catalog::default();
        catalog.pin(resolve(&root).unwrap()).unwrap();
        catalog.pin(resolve(&alias).unwrap()).unwrap();
        let mut worktree = agentdocker_host::project::discover(&root);
        worktree.worktree = Some(dir.path().join("checkout"));
        catalog.remember(worktree, false);
        assert_eq!(catalog.projects.len(), 1);
        assert!(catalog.projects[0].project.worktree.is_none());
    }
}
