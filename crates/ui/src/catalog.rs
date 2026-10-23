//! Local project bookmarks. Physical roots join pinned and discovered projects.
use agentdocker_core::ProjectRef;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// How many projects the list holds, and how many removed folders.
pub const MAX_PROJECTS: usize = 512;
const MAX_STATE_BYTES: u64 = 2 * 1024 * 1024;
/// Dismissed notices are per process generation; the oldest go first.
const MAX_DISMISSED: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub project: ProjectRef,
    pub pinned: bool,
    /// A name the person gave this project here, instead of its folder's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Entry {
    /// What the sidebar calls the project: the chosen name, else the folder.
    pub fn name(&self) -> String {
        self.label.clone().unwrap_or_else(|| self.project.name())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Catalog {
    pub projects: Vec<Entry>,
    /// Folders the person removed from the list; discovery does not bring
    /// them back, adding one again does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<PathBuf>,
    pub selected: Option<PathBuf>,
    pub dark: bool,
    pub unassigned: bool,
    pub appearance: Option<crate::theme::Settings>,
    pub updates: crate::desktop::UpdateSchedule,
    /// The column widths the person dragged the window's dividers to.
    pub panes: crate::app::panes::Widths,
    /// Ended sessions whose undelivered messages the person has seen and
    /// dismissed from Needs you. The messages stay queued; only the notice
    /// goes, and only for that process: a resumed session is a new notice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dismissed: Vec<Dismissed>,
    /// The editor a folder was last opened in, by its kind (`vscode`,
    /// `rustrover`): **Open in…** lists it first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
}

/// One dismissed notice: the session and the process it was about, by birth
/// and pid, so a resumed process whose birth is unknown is still a new notice.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dismissed {
    pub agent: String,
    pub process_started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// Why a project was left off the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The folder is not an absolute path, so no project.
    Relative,
    /// `/` or the home folder, where a session runs when nobody chose a
    /// folder: a project only when the person adds (pins) it.
    Broad,
    /// A folder the person removed from the list: discovery does not bring
    /// it back.
    Removed,
    /// The list already holds [`MAX_PROJECTS`] projects.
    Full,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relative => f.write_str("Project folders must be absolute"),
            Self::Broad => f.write_str(
                "The filesystem root and the home folder are projects only when you add them",
            ),
            Self::Removed => f.write_str(
                "This folder was removed from the project list; add it again to list it",
            ),
            Self::Full => write!(
                f,
                "The project list is full ({MAX_PROJECTS} projects); forget an old project first"
            ),
        }
    }
}

impl std::error::Error for Refusal {}

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
        catalog.hidden.retain(|root| root.is_absolute());
        // Entries discovery made for `/` or the home folder before it stopped
        // doing so; a pinned one is the person's and stays.
        catalog
            .projects
            .retain(|entry| entry.pinned || !is_broad(&entry.project.root));
        let excess = catalog.dismissed.len().saturating_sub(MAX_DISMISSED);
        catalog.dismissed.drain(..excess);
        anyhow::ensure!(
            catalog.hidden.len() <= MAX_PROJECTS,
            "Too many removed folders"
        );
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

    /// Whether the person dismissed the notice for this session's process.
    pub fn is_dismissed(
        &self,
        agent: &str,
        process_started_at: Option<chrono::DateTime<chrono::Utc>>,
        pid: Option<u32>,
    ) -> bool {
        self.dismissed
            .iter()
            .any(|d| d.agent == agent && d.process_started_at == process_started_at && d.pid == pid)
    }

    /// Dismiss a notice; false when it already was.
    pub fn dismiss(
        &mut self,
        agent: &str,
        process_started_at: Option<chrono::DateTime<chrono::Utc>>,
        pid: Option<u32>,
    ) -> bool {
        if self.is_dismissed(agent, process_started_at, pid) {
            return false;
        }
        if self.dismissed.len() >= MAX_DISMISSED {
            self.dismissed.remove(0);
        }
        self.dismissed.push(Dismissed {
            agent: agent.to_owned(),
            process_started_at,
            pid,
        });
        true
    }

    /// Whether a root is too broad to be a project unless the person pins
    /// it: the filesystem root or the home folder, where a session started
    /// because nobody chose a folder. Its sessions are Other sessions.
    pub fn broad_unpinned(&self, root: &Path) -> bool {
        is_broad(root)
            && !self
                .projects
                .iter()
                .any(|e| e.pinned && e.project.root == root)
    }

    /// Whether a session in this folder is one of the Other sessions rather
    /// than a project's: `/` or the home folder unless pinned, or a folder
    /// that is not an absolute path.
    pub fn projectless(&self, root: &Path) -> bool {
        !root.is_absolute() || self.broad_unpinned(root)
    }

    /// List a project found through an agent, or pin one the person added
    /// (`pin`). `Ok` says whether the list changed; `Err` says why the
    /// project was left off it. Discovery never lists a folder the person
    /// removed; adding it again does.
    pub fn remember(&mut self, project: ProjectRef, pin: bool) -> Result<bool, Refusal> {
        self.list(project, pin, pin)
    }

    /// List the project of a session the person opened. Going there is
    /// asking for it, so a folder they removed comes back to the list
    /// (not pinned) and its sessions are coordinated again. `/`, the home
    /// folder and a relative folder are still no project, and a full list
    /// still has no room.
    pub fn visit(&mut self, project: ProjectRef) -> Result<bool, Refusal> {
        self.list(project, false, true)
    }

    /// `restore`: whether a folder the person removed comes back. It does
    /// only once it is sure of a place, so a refusal leaves it removed.
    fn list(&mut self, project: ProjectRef, pin: bool, restore: bool) -> Result<bool, Refusal> {
        if !project.root.is_absolute() {
            return Err(Refusal::Relative);
        }
        // Discovery never makes `/` or the home folder a project; the
        // person still can, by adding it.
        if !pin && is_broad(&project.root) {
            return Err(Refusal::Broad);
        }
        let removed = self.hidden.contains(&project.root);
        if removed && !restore {
            return Err(Refusal::Removed);
        }
        let listed = self.projects.iter().any(|p| p.project.root == project.root);
        if !listed && self.projects.len() >= MAX_PROJECTS {
            return Err(Refusal::Full);
        }
        if removed {
            self.hidden.retain(|h| h != &project.root);
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
            return Ok(removed || *entry != before);
        }
        let mut project = project;
        project.worktree = None;
        self.projects.push(Entry {
            project,
            pinned: pin,
            label: None,
        });
        self.sort_projects();
        // Discovery changes the catalog, not the user's selection. None is the
        // explicit All projects view and must survive new arrivals.
        Ok(true)
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.projects
            .iter()
            .find(|e| Some(&e.project.root) == self.selected.as_ref())
    }

    fn sort_projects(&mut self) {
        self.projects.sort_by(|a, b| {
            a.name()
                .cmp(&b.name())
                .then_with(|| a.project.root.cmp(&b.project.root))
        });
    }

    /// Drop discovered folders that no longer exist: a fixture's checkout
    /// or a scratch worktree that was deleted is not a project any more. A
    /// pinned folder stays and says it is unavailable. Whether anything
    /// went.
    pub fn forget_missing(&mut self) -> bool {
        let before = self.projects.len();
        self.projects
            .retain(|e| e.pinned || e.project.root.is_dir());
        if self.projects.len() == before {
            return false;
        }
        if self.selected.is_some() && self.selected().is_none() {
            self.selected = self.projects.first().map(|e| e.project.root.clone());
        }
        true
    }

    /// Take a project off the list and keep it off until it is added again.
    /// The list of removed folders is bounded like the list itself, and a
    /// removal at the bound is refused rather than forget an earlier one:
    /// what was removed stays removed.
    pub fn remove(&mut self, root: &Path) -> anyhow::Result<bool> {
        if !self.projects.iter().any(|e| e.project.root == root) {
            return Ok(false);
        }
        if !self.hidden.iter().any(|h| h == root) {
            anyhow::ensure!(
                self.hidden.len() < MAX_PROJECTS,
                "The list of removed folders is full ({MAX_PROJECTS}); add one of them back before removing another"
            );
            self.hidden.push(root.to_path_buf());
        }
        self.projects.retain(|e| e.project.root != root);
        if self.selected.as_deref() == Some(root) {
            self.selected = self.projects.first().map(|e| e.project.root.clone());
        }
        Ok(true)
    }

    /// Name a project here; an empty name goes back to the folder's.
    pub fn rename(&mut self, root: &Path, label: &str) -> bool {
        let Some(entry) = self.projects.iter_mut().find(|e| e.project.root == root) else {
            return false;
        };
        let label = label.trim();
        let next = (!label.is_empty() && label != entry.project.name())
            .then(|| label.chars().take(80).collect::<String>());
        if entry.label == next {
            return false;
        }
        entry.label = next;
        self.sort_projects();
        true
    }

    /// The names more than one listed project shares, so the list can say
    /// which folder each of those is.
    pub fn shared_names(&self) -> std::collections::BTreeSet<String> {
        let mut seen = std::collections::BTreeSet::new();
        let mut shared = std::collections::BTreeSet::new();
        for entry in &self.projects {
            let name = entry.name();
            if !seen.insert(name.clone()) {
                shared.insert(name);
            }
        }
        shared
    }

    pub fn pin(&mut self, project: ProjectRef) -> anyhow::Result<()> {
        let root = project.root.clone();
        self.remember(project, true)?;
        self.unassigned = false;
        self.selected = Some(root);
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

    /// Pass the removed folders to the hooks, which read them from a file
    /// of their own: removing a project stops AgentDocker coordinating
    /// sessions there, and adding it back starts it again. Kept apart from
    /// `save` so a failure here is retried on its own, not mistaken for an
    /// unsaved workspace.
    pub fn sync_exclusions(&self, home: &Path) -> std::io::Result<()> {
        agentdocker_host::coordination::write(home, &self.hidden)
    }
}

/// Whether a folder lives under the per-user temporary directory (the
/// system's `temp_dir`, on macOS `/var/folders/…/T`), where test fixtures
/// make and drop folders by the hundred: such a folder is listed only when
/// pinned by hand, never because an agent happened to run in it. `/tmp`
/// is not that: people put checkouts there on purpose.
pub fn is_temporary(root: &Path) -> bool {
    is_temporary_under(root, &std::env::temp_dir())
}

fn is_temporary_under(root: &Path, temp: &Path) -> bool {
    // Linux's default temp_dir is a shared scratch directory, not a
    // per-user fixture root. Never hide every checkout there (or below a
    // filesystem root accidentally selected as TMPDIR).
    let shared = temp.parent().is_none()
        || ["/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp"]
            .iter()
            .any(|path| temp == Path::new(path));
    let private = Path::new("/private").join(temp.strip_prefix("/").unwrap_or(temp));
    (!shared && (root.starts_with(temp) || root.starts_with(&private)))
        || root.starts_with("/private/var/folders")
        || root.starts_with("/var/folders")
}

/// The filesystem root or the home folder: where a session runs when nobody
/// chose a folder for it.
pub fn is_broad(root: &Path) -> bool {
    // `std::env::home_dir` reads HOME and, on Windows, falls back to
    // USERPROFILE: an interactive Windows session often has no HOME.
    root.parent().is_none() || std::env::home_dir().is_some_and(|home| home == root)
}

/// Whether a folder is scratch: under the per-user temporary directory
/// (see [`is_temporary`]) or under the shared scratch roots — `/tmp`,
/// `/var/tmp` and their `/private` spellings. A discovered folder there
/// is listed (people do put checkouts in `/tmp` on purpose), but not among
/// the person's projects: the sidebar keeps it in its collapsed
/// *Temporary* group until it is pinned. Never under the home directory.
pub fn is_scratch(root: &Path) -> bool {
    is_temporary(root)
        || ["/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp"]
            .iter()
            .any(|scratch| root.starts_with(scratch))
}

pub fn resolve(folder: &Path) -> anyhow::Result<ProjectRef> {
    anyhow::ensure!(folder.is_dir(), "Choose an existing project folder");
    Ok(agentdocker_host::project::discover(folder))
}

#[cfg(test)]
mod tests {
    #[test]
    fn discovery_preserves_home_project_and_projectless_selection_across_save() {
        let dir = tempfile::tempdir().unwrap();
        let project = |name: &str| ProjectRef::directory(dir.path().join(name));
        let mut catalog = Catalog::default();
        catalog.remember(project("alpha"), false).unwrap();
        catalog.remember(project("beta"), false).unwrap();
        assert!(catalog.selected.is_none());
        assert!(!catalog.unassigned);
        let state = dir.path().join("state");
        catalog.editor = Some("rustrover".into());
        catalog.save(&state).unwrap();
        assert_eq!(Catalog::load(&state).unwrap(), catalog);
        catalog.selected = Some(project("alpha").root);
        catalog.remember(project("gamma"), false).unwrap();
        assert_eq!(catalog.selected, Some(project("alpha").root));
        catalog.selected = None;
        catalog.unassigned = true;
        catalog.remember(project("delta"), false).unwrap();
        assert!(catalog.selected.is_none());
        assert!(catalog.unassigned);
    }

    use super::*;

    #[test]
    fn temporary_discovery_filter_keeps_shared_scratch_checkouts() {
        for temp in ["/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp", "/"] {
            assert!(
                !is_temporary_under(&Path::new(temp).join("checkout"), Path::new(temp)),
                "{temp}"
            );
        }
        assert!(!is_temporary_under(
            Path::new("/private/tmp/checkout"),
            Path::new("/tmp")
        ));
        for root in [
            "/var/folders/ab/user/T/fixture",
            "/private/var/folders/ab/user/T/fixture",
        ] {
            assert!(is_temporary_under(
                Path::new(root),
                Path::new("/var/folders/ab/user/T")
            ));
        }
        assert!(is_temporary_under(
            Path::new("/run/user/1000/tmp/fixture"),
            Path::new("/run/user/1000/tmp")
        ));
        assert!(!is_temporary_under(
            Path::new("/run/user/1000/tmp-checkout"),
            Path::new("/run/user/1000/tmp")
        ));
        assert!(!is_temporary_under(
            Path::new("/home/person/project"),
            Path::new("/run/user/1000/tmp")
        ));
    }

    /// A discovered folder that no longer exists leaves the list; a pinned
    /// one stays, unavailable, and the selection moves off a vanished one.
    /// A checkout under /tmp or /private/tmp is scratch — a fixture's
    /// workspace, a trial's worktree — and so is anything the temporary
    /// filter hides; a checkout under the home directory is not, whatever
    /// its name.
    #[test]
    fn scratch_is_the_shared_temporary_roots_and_the_private_ones() {
        for scratch in [
            "/tmp/agentdocker-newcomer",
            "/private/tmp/agentdocker-fixture/workspace",
            "/var/tmp/x",
            "/private/var/tmp/x",
            "/private/var/folders/t1/abc/T/fixture",
        ] {
            assert!(is_scratch(Path::new(scratch)), "{scratch}");
        }
        for kept in [
            "/Users/somebody/AgentDocker",
            "/Users/somebody/tmp/notes",
            "/home/somebody/src/tmpfs-tools",
            "/tmpx/y",
        ] {
            assert!(!is_scratch(Path::new(kept)), "{kept}");
        }
    }

    #[test]
    fn vanished_discovered_folders_leave_the_list_and_pinned_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("here");
        std::fs::create_dir(&existing).unwrap();
        let gone = dir.path().join("gone");
        let pinned_gone = dir.path().join("pinned-gone");
        let mut catalog = Catalog::default();
        catalog
            .remember(ProjectRef::directory(existing.clone()), false)
            .unwrap();
        catalog
            .remember(ProjectRef::directory(gone.clone()), false)
            .unwrap();
        catalog
            .remember(ProjectRef::directory(pinned_gone.clone()), true)
            .unwrap();
        catalog.selected = Some(gone.clone());
        assert!(catalog.forget_missing());
        let roots: Vec<_> = catalog
            .projects
            .iter()
            .map(|e| e.project.root.clone())
            .collect();
        assert_eq!(roots, vec![existing.clone(), pinned_gone.clone()]);
        assert_eq!(catalog.selected.as_ref(), Some(&existing));
        assert!(!catalog.forget_missing(), "nothing more to forget");
    }

    #[test]
    fn a_removed_project_stays_off_the_list_until_added_again_and_a_name_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let project = |name: &str| ProjectRef::directory(dir.path().join(name));
        let mut catalog = Catalog::default();
        catalog.remember(project("alpha"), false).unwrap();
        catalog.remember(project("beta"), false).unwrap();
        catalog.selected = Some(project("alpha").root);
        assert!(catalog.remove(&project("alpha").root).unwrap());
        assert_eq!(catalog.projects.len(), 1);
        assert_eq!(catalog.selected, Some(project("beta").root));
        // Discovery does not bring it back; adding it does.
        assert_eq!(
            catalog.remember(project("alpha"), false),
            Err(Refusal::Removed)
        );
        assert_eq!(catalog.projects.len(), 1);
        assert_eq!(catalog.remember(project("alpha"), true), Ok(true));
        assert!(catalog.hidden.is_empty());
        // A chosen name sorts and saves; the folder's name is no label.
        assert!(catalog.rename(&project("beta").root, "  zed  "));
        assert_eq!(catalog.projects.last().unwrap().name(), "zed");
        assert!(catalog.rename(&project("beta").root, "beta"));
        assert_eq!(catalog.projects[1].label, None);
        let home = dir.path().join("state");
        catalog.rename(&project("beta").root, "zed");
        catalog.remove(&project("alpha").root).unwrap();
        catalog.save(&home).unwrap();
        assert_eq!(Catalog::load(&home).unwrap(), catalog);
        // Removing a project takes it out of the hooks' hands too, and
        // adding it back gives it back.
        catalog.sync_exclusions(&home).unwrap();
        assert_eq!(
            agentdocker_host::coordination::roots(&home),
            [project("alpha").root]
        );
        assert_eq!(catalog.remember(project("alpha"), true), Ok(true));
        catalog.save(&home).unwrap();
        catalog.sync_exclusions(&home).unwrap();
        assert!(agentdocker_host::coordination::roots(&home).is_empty());
        catalog.remove(&project("alpha").root).unwrap();
        // Two folders called the same are told apart by name.
        catalog.remember(project("nested/zed"), true).unwrap();
        assert_eq!(catalog.shared_names().len(), 1);
        // The hidden list is bounded like the list itself: at the bound a
        // removal is refused and the project stays, and nothing removed
        // earlier comes back.
        for i in 0..MAX_PROJECTS - 1 {
            let folder = project(&format!("gone-{i}"));
            catalog.remember(folder.clone(), false).unwrap();
            catalog.remove(&folder.root).unwrap();
        }
        assert_eq!(catalog.hidden.len(), MAX_PROJECTS);
        let one_more = project("one-more");
        catalog.remember(one_more.clone(), false).unwrap();
        assert!(catalog.remove(&one_more.root).is_err());
        assert!(
            catalog
                .projects
                .iter()
                .any(|e| e.project.root == one_more.root)
        );
        assert!(catalog.hidden.contains(&project("alpha").root));
        assert!(catalog.hidden.contains(&project("gone-0").root));
        // Removing one already hidden (listed again by hand) needs no room.
        catalog.projects.push(Entry {
            project: project("gone-0"),
            pinned: false,
            label: None,
        });
        assert!(catalog.remove(&project("gone-0").root).unwrap());
    }

    /// Each refusal says which case it is. Opening a session's project
    /// brings back a folder the person removed, unpinned; `/`, the home
    /// folder and a relative folder are still no project; and only a list
    /// at its limit is full, which leaves a removed folder removed.
    #[test]
    fn a_refusal_says_why_and_a_visit_brings_a_removed_folder_back() {
        let dir = tempfile::tempdir().unwrap();
        let project = |name: &str| ProjectRef::directory(dir.path().join(name));
        let home = std::env::home_dir().unwrap();
        let mut catalog = Catalog::default();
        let relative = ProjectRef::directory("relative/folder");
        assert_eq!(
            catalog.remember(relative.clone(), false),
            Err(Refusal::Relative)
        );
        assert_eq!(catalog.visit(relative), Err(Refusal::Relative));
        assert_eq!(
            catalog.visit(ProjectRef::directory(&home)),
            Err(Refusal::Broad)
        );
        assert_eq!(
            catalog.visit(ProjectRef::directory("/")),
            Err(Refusal::Broad)
        );
        assert!(catalog.projects.is_empty());

        catalog.remember(project("alpha"), false).unwrap();
        assert!(catalog.remove(&project("alpha").root).unwrap());
        assert_eq!(
            catalog.remember(project("alpha"), false),
            Err(Refusal::Removed),
            "discovery leaves a removed folder off"
        );
        assert_eq!(catalog.visit(project("alpha")), Ok(true));
        assert!(catalog.hidden.is_empty());
        assert_eq!(catalog.projects.len(), 1);
        assert!(!catalog.projects[0].pinned, "back, not pinned");
        assert_eq!(catalog.visit(project("alpha")), Ok(false), "already listed");

        assert!(catalog.remove(&project("alpha").root).unwrap());
        for i in 0..MAX_PROJECTS {
            catalog.remember(project(&format!("p-{i}")), false).unwrap();
        }
        assert_eq!(
            catalog.remember(project("one-more"), false),
            Err(Refusal::Full)
        );
        assert_eq!(catalog.visit(project("alpha")), Err(Refusal::Full));
        assert!(catalog.pin(project("alpha")).is_err());
        assert_eq!(
            catalog.hidden,
            [project("alpha").root],
            "no room: it stays removed"
        );
        assert_eq!(
            catalog.visit(project("p-0")),
            Ok(false),
            "listed: no room needed"
        );
        let error = catalog.pin(project("one-more")).unwrap_err().to_string();
        assert!(error.contains("full (512 projects)"), "{error}");
        assert!(error.contains("forget an old project"), "{error}");
    }

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
        catalog.remember(discovered.clone(), false).unwrap();
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
        catalog.remember(worktree, false).unwrap();
        assert_eq!(catalog.projects.len(), 1);
        assert!(catalog.projects[0].project.worktree.is_none());
    }

    #[test]
    fn dismissed_notices_are_per_process_bounded_and_saved() {
        let home = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::default();
        let at = Some(chrono::Utc::now());
        assert!(catalog.dismiss("a", at, Some(7)));
        assert!(!catalog.dismiss("a", at, Some(7)), "once is enough");
        assert!(catalog.is_dismissed("a", at, Some(7)));
        assert!(
            !catalog.is_dismissed("a", None, Some(7)),
            "another process is another notice"
        );
        // Two processes of unknown birth are still told apart by pid.
        assert!(catalog.dismiss("b", None, Some(1)));
        assert!(!catalog.is_dismissed("b", None, Some(2)));
        for n in 0..MAX_DISMISSED + 10 {
            catalog.dismiss(&format!("x{n}"), None, None);
        }
        assert_eq!(catalog.dismissed.len(), MAX_DISMISSED);
        assert!(
            !catalog.is_dismissed("a", at, Some(7)),
            "the oldest go first"
        );
        catalog.save(home.path()).unwrap();
        let loaded = Catalog::load(home.path()).unwrap();
        assert_eq!(loaded.dismissed, catalog.dismissed);
    }

    #[test]
    fn the_root_and_home_are_not_projects_unless_pinned() {
        let home = std::env::home_dir().unwrap();
        let mut catalog = Catalog::default();
        assert_eq!(
            catalog.remember(ProjectRef::directory("/"), false),
            Err(Refusal::Broad)
        );
        assert_eq!(
            catalog.remember(ProjectRef::directory(&home), false),
            Err(Refusal::Broad)
        );
        assert!(catalog.projects.is_empty());
        assert!(catalog.broad_unpinned(Path::new("/")));
        assert!(
            !catalog.broad_unpinned(&home.join("code")),
            "a folder in home is a project"
        );
        // The person can still make one a project, by adding (pinning) it.
        assert_eq!(
            catalog.remember(ProjectRef::directory(&home), true),
            Ok(true)
        );
        assert!(!catalog.broad_unpinned(&home));
        // Saved unpinned entries from before are dropped on load.
        let dir = tempfile::tempdir().unwrap();
        let mut old = Catalog::default();
        old.projects.push(Entry {
            project: ProjectRef::directory("/"),
            pinned: false,
            label: None,
        });
        old.projects.push(Entry {
            project: ProjectRef::directory(&home),
            pinned: true,
            label: None,
        });
        old.save(dir.path()).unwrap();
        let loaded = Catalog::load(dir.path()).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project.root, home);
    }
}
