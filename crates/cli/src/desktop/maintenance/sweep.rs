//! What commands leave in the installation's own folder, found and removed
//! under the installation lock.
//!
//! Three things gathered there that nothing took away:
//!
//! - `retired/`: a prune moves a release out of `versions/` into a stage
//!   here before it deletes it, and one that stops in between leaves the
//!   stage;
//! - `generations/`: every activation makes one and none was removed, and
//!   an activation that stops leaves its staging folder there, or the
//!   pointer's beside `current`;
//! - `downloads/`: an update keeps its archive and its extraction after it
//!   has been applied.
//!
//! Each is recognised by what it holds, not by where it is: a folder that
//! is not exactly what a command of this installer leaves is kept, and
//! listed with the reason. Each removal goes in steps that leave something
//! this module still recognises, so a sweep that stops is finished by the
//! next one.

use std::fs::FileType;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

use super::super::{Activation, Layout, update};
use super::{Entry, checked_version_at};

/// One thing to remove.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Leftover {
    pub(super) path: PathBuf,
    /// `retired`, `generation`, `staging` or `download`.
    pub(super) kind: &'static str,
}

/// What a sweep found: what it removes, and what is in the same places
/// and stays, each with why.
#[derive(Default)]
pub(super) struct Found {
    pub(super) remove: Vec<Leftover>,
    pub(super) kept: Vec<Entry>,
}

/// More entries than this in one of the swept folders and it is left
/// alone, as the version inventory is.
const MAX: usize = 1000;

/// How the folders `tempfile` makes are named, which is how a stage is
/// told from anything else put beside it.
const STAGE: &str = ".tmp";

/// Written in a stage once the release moved into it has been verified,
/// before its deletion begins: with it, what is left of a deletion that
/// stopped is known to be one, and finished without asking the half
/// deleted tree to verify.
const VERIFIED: &str = "verified";

const UNRECOGNIZED: &str = "unrecognized; preserved";
const CONTENTS: &str = "unrecognized contents; preserved";
/// A folder that cannot be read is kept like one that is not recognised:
/// a sweep that cannot look does not stop the prune it is part of.
const UNREADABLE: &str = "cannot be read; preserved";

fn name_of(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
}

fn release_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The entries of one of the installation's folders, none of them
/// followed. `None` when the folder is not there; a reason when it is not
/// a folder this can sweep.
fn listing(directory: &Path) -> Option<Result<Vec<PathBuf>, &'static str>> {
    match directory.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(Err(UNREADABLE)),
        Ok(metadata) if !metadata.is_dir() => return Some(Err("not a directory; preserved")),
        Ok(_) => {}
    }
    let read = std::fs::read_dir(directory).and_then(|entries| {
        entries
            .take(MAX + 1)
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()
    });
    Some(match read {
        Err(_) => Err(UNREADABLE),
        Ok(found) if found.len() > MAX => Err("more than 1000 entries; not swept"),
        Ok(mut found) => {
            found.sort();
            Ok(found)
        }
    })
}

/// What one entry of a swept folder has to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Link,
    Folder,
}

impl Kind {
    fn is(self, found: &FileType) -> bool {
        match self {
            Kind::File => found.is_file(),
            Kind::Link => found.is_symlink(),
            Kind::Folder => found.is_dir(),
        }
    }
}

/// Everything a swept folder may hold: each name, with what it has to
/// be. The names are the ones the command that made the folder writes,
/// and no others: a folder holding anything else is not its leftover.
type Shape = Vec<(String, Kind)>;

fn shape(entries: &[(&str, Kind)]) -> Shape {
    entries
        .iter()
        .map(|(name, kind)| ((*name).to_owned(), *kind))
        .collect()
}

/// What a generation holds, and so what an activation's staging folder
/// in `generations/` may hold when the activation stopped: the record and
/// the link to the release.
fn generation_shape() -> Shape {
    shape(&[("activation.json", Kind::File), ("payload", Kind::Link)])
}

/// What the pointer's staging folder holds: the new pointer before it is
/// renamed into place, or the old one being put back.
fn pointer_shape() -> Shape {
    shape(&[("current", Kind::Link), ("restore", Kind::Link)])
}

/// What an update leaves in its version's folder, by the names the
/// update writes: the extraction, the archive being fetched, and the
/// archive. The extraction goes first, so that what is left of a removal
/// that stopped still has this shape.
fn download_shape() -> Shape {
    let archive = update::archive_name(update::host_target());
    vec![
        ("payload".to_owned(), Kind::Folder),
        (format!("{archive}.part"), Kind::File),
        (archive, Kind::File),
    ]
}

/// Whether `directory` is a real directory holding nothing but what
/// `shape` names, each as the kind it has to be, links not followed.
fn holds_only(directory: &Path, shape: &Shape) -> Result<bool> {
    if !directory.symlink_metadata()?.is_dir() {
        return Ok(false);
    }
    for (at, entry) in std::fs::read_dir(directory)?.enumerate() {
        let entry = entry?;
        let found = entry.file_type()?;
        let name = entry.file_name();
        let allowed = shape
            .iter()
            .any(|(allowed, kind)| name.to_str() == Some(allowed) && kind.is(&found));
        if at >= MAX || !allowed {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A stage under `retired/`, as found.
enum Stage {
    /// What a prune leaves: nothing, or a release that verifies, or one
    /// whose deletion had begun. The release folder is given when there
    /// is one left.
    Ours(Option<PathBuf>),
    Kept(&'static str),
}

fn stage(root: &Path, stage: &Path) -> Result<Stage> {
    if !name_of(stage).starts_with(STAGE) || !stage.symlink_metadata()?.is_dir() {
        return Ok(Stage::Kept(UNRECOGNIZED));
    }
    let mut release = None;
    let mut verified = false;
    for entry in std::fs::read_dir(stage)?.take(3) {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if name_of(&path) == VERIFIED && kind.is_file() {
            verified = true;
        } else if release_name(name_of(&path)) && kind.is_dir() && release.is_none() {
            release = Some(path);
        } else {
            return Ok(Stage::Kept(CONTENTS));
        }
    }
    Ok(match (release, verified) {
        // No mark: the prune stopped before it had verified what it
        // moved, so it is verified now; one that does not verify is the
        // changed release a prune keeps for the person to look at.
        (Some(release), false) if checked_version_at(root, &release).is_err() => {
            Stage::Kept("changed release; preserved")
        }
        (release, _) => Stage::Ours(release),
    })
}

/// Delete a verified release from its stage, then the stage. The mark is
/// made first and removed last: stopped anywhere, what is left is a stage
/// with the mark, or an empty one, and both are finished by a sweep.
pub(super) fn discard(stage: &Path, release: Option<&Path>) -> Result<()> {
    let mark = stage.join(VERIFIED);
    if let Some(release) = release {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&mark)?
            .sync_all()?;
        std::fs::File::open(stage)?.sync_all()?;
        std::fs::remove_dir_all(release)?;
    }
    match std::fs::remove_file(&mark) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    std::fs::remove_dir(stage)?;
    Ok(())
}

/// Everything to sweep, read once. `replaced` is the generation the
/// pointer named when the command began, when the command has moved it:
/// a reader that followed the pointer before it moved is still reading
/// that generation's files, so the command that moved it keeps it and a
/// later one removes it.
///
/// It does not fail: what cannot be read is kept, with that as the
/// reason, and the prune this is part of goes on.
pub(super) fn found(
    layout: &Layout,
    active: Option<&Activation>,
    replaced: Option<&Path>,
) -> Found {
    let mut found = Found::default();
    let root = &layout.root;
    let mut keep = |path: PathBuf, reason| found.kept.push(Entry { path, reason });
    let mut remove = Vec::new();
    let mut leftover = |path: PathBuf, kind| remove.push(Leftover { path, kind });
    // Whether a folder holds only what `shape` allows; `None` when it
    // cannot be read.
    let shaped = |path: &Path, shape: Shape| holds_only(path, &shape).ok();

    let retired = root.join("retired");
    match listing(&retired) {
        None => {}
        Some(Err(reason)) => keep(retired, reason),
        Some(Ok(stages)) => {
            for path in stages {
                match stage(root, &path) {
                    Ok(Stage::Ours(_)) => leftover(path, "retired"),
                    Ok(Stage::Kept(reason)) => keep(path, reason),
                    Err(_) => keep(path, UNREADABLE),
                }
            }
        }
    }

    let generations = root.join("generations");
    let named = root.join("current").read_link().ok();
    match listing(&generations) {
        None => {}
        Some(Err(reason)) => keep(generations, reason),
        Some(Ok(entries)) => {
            for path in entries {
                let name = name_of(&path);
                let staging = name.starts_with(STAGE);
                if !staging && uuid::Uuid::try_parse(name).is_err() {
                    keep(path, UNRECOGNIZED);
                } else if Some(path.as_path()) == named.as_deref()
                    || Some(path.as_path()) == replaced
                {
                    // The generation in use, and the one a reader may
                    // still be in: not leftovers.
                } else {
                    match shaped(&path, generation_shape()) {
                        Some(true) if staging => leftover(path, "staging"),
                        Some(true) => leftover(path, "generation"),
                        Some(false) => keep(path, CONTENTS),
                        None => keep(path, UNREADABLE),
                    }
                }
            }
        }
    }
    // The pointer's staging folder, beside the pointer. Nothing else in
    // the root is a leftover: the folders above have their own rules, and
    // the rest is the installation's.
    if let Some(Ok(entries)) = listing(root) {
        for path in entries {
            if name_of(&path).starts_with(STAGE) && shaped(&path, pointer_shape()) == Some(true) {
                leftover(path, "staging");
            }
        }
    }

    // A download is done with once its version is not newer than the
    // active release's: an update installs only what is newer. With
    // nothing active, or a version that cannot be compared, none is.
    let downloads = root.join("downloads");
    let installed = active.and_then(|active| update::parse_version(&active.current.version).ok());
    match (installed, listing(&downloads)) {
        (_, None) | (None, _) => {}
        (_, Some(Err(reason))) => keep(downloads, reason),
        (Some(installed), Some(Ok(entries))) => {
            for path in entries {
                let version = update::parse_version(name_of(&path))
                    .ok()
                    .filter(|version| version.to_string() == name_of(&path));
                match version {
                    None => keep(path, UNRECOGNIZED),
                    Some(version)
                        if version.cmp_precedence(&installed) == std::cmp::Ordering::Greater => {}
                    Some(_) => match shaped(&path, download_shape()) {
                        Some(true) => leftover(path, "download"),
                        Some(false) => keep(path, CONTENTS),
                        None => keep(path, UNREADABLE),
                    },
                }
            }
        }
    }
    found.remove = remove;
    found
}

/// Remove one leftover a plan lists, after reading it again: what it is
/// now decides, not what it was when the plan was made.
pub(super) fn remove(layout: &Layout, leftover: &Leftover) -> Result<()> {
    remove_held(layout, leftover, &mut || {})
}

/// `remove`, calling `validated` once the leftover has been read again
/// and before anything of it is removed, so that a test can change it in
/// between.
fn remove_held(layout: &Layout, leftover: &Leftover, validated: &mut dyn FnMut()) -> Result<()> {
    let path = &leftover.path;
    let root = &layout.root;
    let changed = || {
        format!(
            "{} changed since it was reviewed; preserved",
            path.display()
        )
    };
    let parent = path.parent().context("leftover has no parent")?;
    match leftover.kind {
        "retired" => match stage(root, path)? {
            Stage::Ours(release) if parent == root.join("retired") => {
                validated();
                discard(path, release.as_deref())?;
            }
            _ => bail!(changed()),
        },
        "generation" | "staging" => {
            let shape = if parent == root {
                pointer_shape()
            } else {
                generation_shape()
            };
            ensure!(
                (parent == root || parent == root.join("generations"))
                    && root.join("current").read_link().ok().as_deref() != Some(path.as_path())
                    && holds_only(path, &shape)?,
                changed()
            );
            validated();
            empty(path, &shape)?;
        }
        "download" => {
            let shape = download_shape();
            ensure!(
                parent == root.join("downloads") && holds_only(path, &shape)?,
                changed()
            );
            validated();
            let bundle = path.join("payload").join("AgentDocker.app");
            if bundle.symlink_metadata().is_ok_and(|found| found.is_dir()) {
                // A no-op wherever bundles are not registered.
                layout.forget_stored(&bundle);
            }
            empty(path, &shape)?;
        }
        kind => bail!("unknown leftover kind {kind}"),
    }
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Remove from a recognised folder what its shape names, then the folder.
/// Only those names are touched, each checked for its kind as it is
/// acted on: nothing is listed again, so whatever else is in the folder
/// by now, put there after it was read, is not removed, and the folder,
/// which is then not empty, stays with it.
fn empty(directory: &Path, shape: &Shape) -> Result<()> {
    for (name, kind) in shape {
        let path = directory.join(name);
        let found = match path.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
            Ok(found) => found.file_type(),
        };
        ensure!(
            kind.is(&found),
            "{} changed since it was reviewed; preserved",
            path.display()
        );
        if *kind == Kind::Folder {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    std::fs::remove_dir(directory).with_context(|| {
        format!(
            "{} is kept: it cannot be removed, or it holds something that is not the installer's",
            directory.display()
        )
    })
}

// The fixtures are made of symbolic links and permission bits.
#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeSet;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use agentdocker_host::dirs;

    use super::super::super::{Reading, Release};
    use super::super::tests::{fixture, retained};
    use super::super::{Plan, apply, clean_up, plan, plan_keeping};
    use super::*;

    fn review(layout: &Layout) -> Plan {
        plan(layout, Some(0), true, || Ok(false)).unwrap().0
    }

    fn swept(plan: &Plan) -> BTreeSet<(PathBuf, &'static str)> {
        plan.sweep
            .iter()
            .map(|leftover| (leftover.path.clone(), leftover.kind))
            .collect()
    }

    fn kept(plan: &Plan) -> BTreeSet<(PathBuf, &'static str)> {
        plan.unswept
            .iter()
            .map(|entry| (entry.path.clone(), entry.reason))
            .collect()
    }

    fn writable(path: &Path, yes: bool) {
        let mode = if yes { 0o700 } else { 0o500 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Permission bits do not stop the superuser, so a removal cannot be
    /// made to fail with them.
    fn superuser() -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    fn stage_in(parent: &Path) -> PathBuf {
        tempfile::tempdir_in(parent).unwrap().keep()
    }

    /// A release moved out of `versions/` into a stage, as a prune does
    /// it: the stage, and the release in it.
    fn retire(layout: &Layout, release: &Release) -> (PathBuf, PathBuf) {
        let retired = layout.root.join("retired");
        dirs::secure_state_dir(&retired).unwrap();
        let stage = stage_in(&retired);
        let moved = stage.join(&release.id);
        std::fs::rename(layout.root.join("versions").join(&release.id), &moved).unwrap();
        (stage, moved)
    }

    fn named(layout: &Layout) -> PathBuf {
        layout.root.join("current").read_link().unwrap()
    }

    /// A prune goes: make the stage, move the release into it, verify it
    /// there, mark it, delete it, remove the mark and the stage. Stopped
    /// after any of those, the next prune finds the stage and finishes.
    #[test]
    fn a_prune_stopped_at_any_step_is_finished_by_the_next() {
        type Stop = fn(&Layout, &Release) -> PathBuf;
        let stops: &[(&str, Stop)] = &[
            ("the stage made and nothing moved into it", |layout, _| {
                let retired = layout.root.join("retired");
                dirs::secure_state_dir(&retired).unwrap();
                stage_in(&retired)
            }),
            (
                "the release moved and not yet verified",
                |layout, release| retire(layout, release).0,
            ),
            (
                "verified and marked, its deletion not begun",
                |layout, release| {
                    let (stage, moved) = retire(layout, release);
                    std::fs::write(stage.join(VERIFIED), "").unwrap();
                    assert!(moved.is_dir());
                    stage
                },
            ),
            ("its deletion begun", |layout, release| {
                let (stage, moved) = retire(layout, release);
                std::fs::write(stage.join(VERIFIED), "").unwrap();
                let binaries = moved.join(&release.payload).join(layout.binary_subdir());
                std::fs::remove_dir_all(binaries).unwrap();
                stage
            }),
            (
                "the release deleted and the mark left",
                |layout, release| {
                    let (stage, moved) = retire(layout, release);
                    std::fs::write(stage.join(VERIFIED), "").unwrap();
                    std::fs::remove_dir_all(moved).unwrap();
                    stage
                },
            ),
        ];
        for (name, stop) in stops {
            let (_temp, layout) = fixture();
            let old = retained(&layout, "old", 1);
            let current = retained(&layout, "current", 1);
            layout.activate(current.clone(), None).unwrap();
            let stage = stop(&layout, &old);
            let found = review(&layout);
            assert_eq!(
                found.sweep,
                [Leftover {
                    path: stage.clone(),
                    kind: "retired"
                }],
                "{name}"
            );
            assert!(found.unswept.is_empty(), "{name}");
            apply(&layout, &found).unwrap();
            assert!(!stage.exists(), "{name}");
            assert_eq!(
                std::fs::read_dir(layout.root.join("retired"))
                    .unwrap()
                    .count(),
                0,
                "{name}"
            );
            assert!(layout.payload(&current).exists(), "{name}");
            assert!(review(&layout).sweep.is_empty(), "{name}");
        }
    }

    /// A deletion that fails, rather than a process that dies: the mark
    /// was made before it began, so what it leaves, which may no longer
    /// verify, is finished by the next sweep.
    #[test]
    fn a_deletion_that_fails_leaves_a_stage_the_next_sweep_finishes() {
        if superuser() {
            return;
        }
        let (_temp, layout) = fixture();
        let old = retained(&layout, "old", 1);
        let current = retained(&layout, "current", 1);
        layout.activate(current, None).unwrap();
        let (stage, moved) = retire(&layout, &old);
        let binaries = moved.join(&old.payload).join(layout.binary_subdir());
        writable(&binaries, false);
        let refused = discard(&stage, Some(&moved));
        writable(&binaries, true);
        assert!(refused.is_err());
        assert!(stage.join(VERIFIED).is_file(), "marked before deleting");
        let found = review(&layout);
        assert_eq!(swept(&found), BTreeSet::from([(stage.clone(), "retired")]));
        apply(&layout, &found).unwrap();
        assert!(!stage.exists());
    }

    /// What is in `retired/` and is not what a prune leaves stays, and is
    /// listed with why: a release changed after it left `versions/` (what
    /// a prune itself refuses to delete, and keeps for the person), a
    /// stage with something else in it, and a folder that is no stage.
    #[test]
    fn what_a_prune_did_not_leave_stays_in_retired() {
        let (_temp, layout) = fixture();
        let changed = retained(&layout, "changed", 1);
        let other = retained(&layout, "other", 1);
        let current = retained(&layout, "current", 1);
        layout.activate(current, None).unwrap();
        let (altered, moved) = retire(&layout, &changed);
        std::fs::write(
            moved.join(&changed.payload).join("added"),
            "not the release's",
        )
        .unwrap();
        let (odd, _) = retire(&layout, &other);
        std::fs::write(odd.join("notes"), "somebody's").unwrap();
        let by_hand = layout.root.join("retired").join("kept-by-hand");
        std::fs::create_dir(&by_hand).unwrap();

        let found = review(&layout);
        assert!(found.sweep.is_empty(), "{:?}", found.sweep);
        assert_eq!(
            kept(&found),
            BTreeSet::from([
                (altered.clone(), "changed release; preserved"),
                (odd.clone(), CONTENTS),
                (by_hand.clone(), UNRECOGNIZED),
            ])
        );
        apply(&layout, &found).unwrap();
        assert!(moved.join(&changed.payload).join("added").is_file());
        assert!(odd.join("notes").is_file() && by_hand.is_dir());
        // A plan that lists one of them all the same removes nothing.
        let refused = remove(
            &layout,
            &Leftover {
                path: altered.clone(),
                kind: "retired",
            },
        );
        assert!(
            format!("{:#}", refused.unwrap_err()).contains("changed since it was reviewed"),
            "a changed release is not deleted"
        );
        assert!(moved.is_dir());
    }

    /// Every activation makes a generation, and none was ever removed.
    /// The one the pointer names stays; the one it named when the command
    /// began stays through that command, for a reader that followed the
    /// pointer just before it moved, and goes with the next; the rest go.
    #[test]
    fn generations_no_pointer_names_are_swept_and_the_one_just_replaced_once_more() {
        let (_temp, layout) = fixture();
        let a = retained(&layout, "a", 1);
        let b = retained(&layout, "b", 1);
        let c = retained(&layout, "c", 1);
        layout.activate(a.clone(), None).unwrap();
        let first = named(&layout);
        layout.activate(b.clone(), Some(a)).unwrap();
        let second = named(&layout);
        layout.activate(c.clone(), Some(b)).unwrap();
        let third = named(&layout);

        // The command that moved the pointer off the second.
        let (found, _) = plan_keeping(&layout, Some(0), true, Some(&second), || Ok(false)).unwrap();
        assert_eq!(
            swept(&found),
            BTreeSet::from([(first.clone(), "generation")])
        );
        let done = clean_up(&layout, Some(&second), || Ok(false)).unwrap();
        assert_eq!(done.sweep.len(), 1);
        assert!(!first.exists() && second.is_dir() && third.is_dir());
        // The next one.
        let done = clean_up(&layout, None, || Ok(false)).unwrap();
        assert_eq!(
            swept(&done),
            BTreeSet::from([(second.clone(), "generation")])
        );
        assert!(!second.exists());
        assert_eq!(named(&layout), third);
        assert_eq!(layout.active().unwrap().unwrap().current.id, c.id);
        assert!(
            clean_up(&layout, None, || Ok(false))
                .unwrap()
                .sweep
                .is_empty()
        );
        // The generation in use is not removed even by a plan that lists it.
        let refused = remove(
            &layout,
            &Leftover {
                path: third.clone(),
                kind: "generation",
            },
        );
        assert!(
            format!("{:#}", refused.unwrap_err()).contains("changed since it was reviewed"),
            "the generation in use stays"
        );
        assert!(layout.active().unwrap().is_some());
    }

    /// An activation goes: make a staging folder in `generations/`, write
    /// the record, link the release, rename it to its name; then make a
    /// folder beside the pointer, make the new pointer in it and rename
    /// it over the old, or put the old one back. What each stop leaves is
    /// swept, and so is a generation whose own removal stopped half-way.
    #[test]
    fn what_an_activation_that_stopped_left_is_swept() {
        let (_temp, layout) = fixture();
        let current = retained(&layout, "current", 1);
        layout.activate(current.clone(), None).unwrap();
        let in_use = named(&layout);
        let generations = layout.root.join("generations");
        let payload = layout.payload(&current);

        let made = stage_in(&generations);
        let recorded = stage_in(&generations);
        std::fs::write(recorded.join("activation.json"), "{}").unwrap();
        let linked = stage_in(&generations);
        std::fs::write(linked.join("activation.json"), "{}").unwrap();
        symlink(&payload, linked.join("payload")).unwrap();
        let beside = stage_in(&layout.root);
        let swinging = stage_in(&layout.root);
        symlink(&in_use, swinging.join("current")).unwrap();
        let restoring = stage_in(&layout.root);
        symlink(&in_use, restoring.join("restore")).unwrap();
        let half = generations.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&half).unwrap();
        symlink(&payload, half.join("payload")).unwrap();

        let found = review(&layout);
        assert_eq!(
            swept(&found),
            BTreeSet::from([
                (made.clone(), "staging"),
                (recorded.clone(), "staging"),
                (linked.clone(), "staging"),
                (beside.clone(), "staging"),
                (swinging.clone(), "staging"),
                (restoring.clone(), "staging"),
                (half.clone(), "generation"),
            ])
        );
        assert!(found.unswept.is_empty());
        apply(&layout, &found).unwrap();
        for gone in [
            &made, &recorded, &linked, &beside, &swinging, &restoring, &half,
        ] {
            assert!(gone.symlink_metadata().is_err(), "{}", gone.display());
        }
        assert_eq!(named(&layout), in_use);
        assert_eq!(layout.active().unwrap().unwrap().current.id, current.id);
        assert!(payload.is_dir(), "a link's target is not followed");
    }

    /// What is in `generations/`, or beside the pointer, and is not what
    /// an activation leaves, stays.
    #[test]
    fn what_an_activation_did_not_leave_stays() {
        let (_temp, layout) = fixture();
        let current = retained(&layout, "current", 1);
        layout.activate(current, None).unwrap();
        let generations = layout.root.join("generations");
        let odd = generations.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&odd).unwrap();
        std::fs::write(odd.join("activation.json"), "{}").unwrap();
        std::fs::write(odd.join("notes"), "somebody's").unwrap();
        // A link where the record goes is not a record.
        let linked = generations.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&linked).unwrap();
        symlink(
            layout.prefix.join("elsewhere"),
            linked.join("activation.json"),
        )
        .unwrap();
        let by_hand = generations.join("kept-by-hand");
        std::fs::create_dir(&by_hand).unwrap();
        // A folder named like a stage beside the pointer, holding a file:
        // not the pointer's, and none of the sweep's business.
        let beside = stage_in(&layout.root);
        std::fs::write(beside.join("data"), "somebody's").unwrap();

        let found = review(&layout);
        assert!(found.sweep.is_empty(), "{:?}", found.sweep);
        assert_eq!(
            kept(&found),
            BTreeSet::from([
                (odd.clone(), CONTENTS),
                (linked.clone(), CONTENTS),
                (by_hand.clone(), UNRECOGNIZED),
            ])
        );
        apply(&layout, &found).unwrap();
        assert!(odd.join("notes").is_file() && linked.is_dir() && by_hand.is_dir());
        assert!(beside.join("data").is_file());
    }

    /// The archive's name, as the feed has to name it on this machine.
    fn archive() -> String {
        update::archive_name(update::host_target())
    }

    /// What an update leaves for a version, by the names it writes: the
    /// archive, a partial one and the extraction.
    fn download(layout: &Layout, version: &str) -> PathBuf {
        let folder = layout.root.join("downloads").join(version);
        let bundle = folder.join("payload").join("AgentDocker.app");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("file"), version).unwrap();
        std::fs::write(folder.join(archive()), version).unwrap();
        std::fs::write(folder.join(format!("{}.part", archive())), version).unwrap();
        folder
    }

    /// An update installs only a release newer than the active one, so a
    /// download whose version is not newer is done with, applied or
    /// passed over; a newer one is still to be applied and stays. With
    /// nothing active none is done with.
    #[test]
    fn downloads_are_swept_once_their_version_is_not_newer_than_the_active_one() {
        let (_temp, layout) = fixture();
        let current = retained(&layout, "current", 1);
        assert_eq!(current.version, "0.1.0");
        let applied = download(&layout, "0.1.0");
        let found = review(&layout);
        assert!(found.sweep.is_empty() && found.unswept.is_empty());

        layout.activate(current, None).unwrap();
        let superseded = download(&layout, "0.0.9");
        let candidate = download(&layout, "0.1.0-rc.1");
        let newer = download(&layout, "0.2.0");
        let preview = download(&layout, "0.1.1-rc.1");
        // One whose removal stopped half-way, and one that stopped
        // before its folder went.
        let half = download(&layout, "0.0.8");
        std::fs::remove_dir_all(half.join("payload")).unwrap();
        std::fs::remove_file(half.join(archive())).unwrap();
        let emptied = layout.root.join("downloads").join("0.0.7");
        std::fs::create_dir(&emptied).unwrap();
        // Not what an update leaves: a link, a second folder, a file an
        // update does not write (beside the archive, or alone), a file
        // where the extraction goes, a name that is no version.
        let linked = download(&layout, "0.0.6");
        symlink(&newer, linked.join("link")).unwrap();
        let nested = download(&layout, "0.0.5");
        std::fs::create_dir(nested.join("other")).unwrap();
        let noted = download(&layout, "0.0.4");
        std::fs::write(noted.join("notes.txt"), "somebody's").unwrap();
        let only_notes = layout.root.join("downloads").join("0.0.3");
        std::fs::create_dir(&only_notes).unwrap();
        std::fs::write(only_notes.join("notes.txt"), "somebody's").unwrap();
        let filed = layout.root.join("downloads").join("0.0.2");
        std::fs::create_dir(&filed).unwrap();
        std::fs::write(filed.join("payload"), "a file, not an extraction").unwrap();
        // Another machine's archive is not this installer's either.
        let foreign = layout.root.join("downloads").join("0.0.1");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(
            foreign.join("agentdocker.tar.gz"),
            "not a name the feed has",
        )
        .unwrap();
        let unnamed = layout.root.join("downloads").join("notes");
        std::fs::create_dir(&unnamed).unwrap();
        let partial = layout.root.join("downloads").join("0.1");
        std::fs::create_dir(&partial).unwrap();

        let found = review(&layout);
        assert_eq!(
            swept(&found),
            [&applied, &superseded, &candidate, &half, &emptied]
                .into_iter()
                .map(|path| (path.clone(), "download"))
                .collect()
        );
        assert_eq!(
            kept(&found),
            BTreeSet::from([
                (linked.clone(), CONTENTS),
                (nested.clone(), CONTENTS),
                (noted.clone(), CONTENTS),
                (only_notes.clone(), CONTENTS),
                (filed.clone(), CONTENTS),
                (foreign.clone(), CONTENTS),
                (unnamed.clone(), UNRECOGNIZED),
                (partial.clone(), UNRECOGNIZED),
            ])
        );
        apply(&layout, &found).unwrap();
        for gone in [&applied, &superseded, &candidate, &half, &emptied] {
            assert!(!gone.exists(), "{}", gone.display());
        }
        for stays in [
            &newer,
            &preview,
            &linked,
            &nested,
            &noted,
            &only_notes,
            &filed,
            &foreign,
            &unnamed,
            &partial,
        ] {
            assert!(stays.is_dir(), "{}", stays.display());
        }
        // Whole, with what was beside the installer's files in them.
        assert!(noted.join(archive()).is_file() && noted.join("payload").is_dir());
        assert_eq!(
            std::fs::read_to_string(noted.join("notes.txt")).unwrap(),
            "somebody's"
        );
        assert_eq!(
            std::fs::read_to_string(only_notes.join("notes.txt")).unwrap(),
            "somebody's"
        );
        assert!(newer.join("payload/AgentDocker.app/file").is_file());
        assert!(review(&layout).sweep.is_empty());
    }

    /// A removal that fails leaves what the next sweep still recognises
    /// and finishes: a generation, and a download.
    #[test]
    fn a_sweep_that_fails_is_finished_by_the_next() {
        if superuser() {
            return;
        }
        let (_temp, layout) = fixture();
        let a = retained(&layout, "a", 1);
        let b = retained(&layout, "b", 1);
        layout.activate(a.clone(), None).unwrap();
        let stale = named(&layout);
        layout.activate(b, Some(a)).unwrap();
        let done_with = download(&layout, "0.1.0");
        // The generation, which goes first, cannot be emptied: the sweep
        // stops there and nothing has gone.
        let found = review(&layout);
        assert_eq!(found.sweep.len(), 2);
        writable(&stale, false);
        let refused = apply(&layout, &found);
        writable(&stale, true);
        assert!(refused.is_err());
        assert!(stale.join("activation.json").is_file() && done_with.is_dir());
        // The download cannot be emptied: the generation before it goes,
        // and the download's folder is left.
        let found = review(&layout);
        assert_eq!(found.sweep.len(), 2);
        writable(&done_with, false);
        let refused = apply(&layout, &found);
        writable(&done_with, true);
        assert!(refused.is_err());
        assert!(!stale.exists() && done_with.is_dir());
        // What is left of it is still a download, and the next sweep's.
        let found = review(&layout);
        assert_eq!(
            swept(&found),
            BTreeSet::from([(done_with.clone(), "download")])
        );
        apply(&layout, &found).unwrap();
        assert!(!done_with.exists());
        assert!(layout.active().unwrap().is_some());
    }

    /// Something put into a leftover after it was read again and before
    /// it is removed is not the installer's, and is not removed: only
    /// the names the leftover's shape has are touched, so a file and a
    /// folder that appear in between are there afterwards, byte for
    /// byte, in the folder that now stays for them.
    #[test]
    fn what_appears_in_a_leftover_after_it_was_read_is_not_removed() {
        let (_temp, layout) = fixture();
        let a = retained(&layout, "a", 1);
        let b = retained(&layout, "b", 1);
        let old = retained(&layout, "old", 1);
        layout.activate(a.clone(), None).unwrap();
        let stale = named(&layout);
        layout.activate(b, Some(a)).unwrap();
        let done_with = download(&layout, "0.1.0");
        let (stage, moved) = retire(&layout, &old);
        let beside = stage_in(&layout.root);
        symlink(&stale, beside.join("restore")).unwrap();

        for (path, kind) in [
            (&stale, "generation"),
            (&beside, "staging"),
            (&done_with, "download"),
            (&stage, "retired"),
        ] {
            let leftover = Leftover {
                path: path.clone(),
                kind,
            };
            let refused = remove_held(&layout, &leftover, &mut || {
                std::fs::write(path.join("notes.txt"), "somebody's").unwrap();
                std::fs::create_dir(path.join("kept")).unwrap();
                std::fs::write(path.join("kept").join("inside"), "theirs too").unwrap();
            });
            assert!(refused.is_err(), "{kind}: the folder cannot go");
            assert_eq!(
                std::fs::read_to_string(path.join("notes.txt")).unwrap(),
                "somebody's",
                "{kind}"
            );
            assert_eq!(
                std::fs::read_to_string(path.join("kept").join("inside")).unwrap(),
                "theirs too",
                "{kind}"
            );
            // What was the installer's in it went; the rest is all that
            // is left, and from now on the folder is not a leftover.
            let left: BTreeSet<_> = std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            assert_eq!(
                left,
                BTreeSet::from(["kept".to_owned(), "notes.txt".to_owned()]),
                "{kind}"
            );
        }
        assert!(!moved.exists(), "the verified release went");
        let found = review(&layout);
        assert!(found.sweep.is_empty(), "{:?}", found.sweep);
        // The folder beside the pointer is none of the sweep's business
        // once it holds something else; the others are listed as kept.
        assert_eq!(
            kept(&found),
            BTreeSet::from([
                (stale.clone(), CONTENTS),
                (done_with.clone(), CONTENTS),
                (stage.clone(), CONTENTS),
            ])
        );
    }

    /// A read of the active installation is held where the schedule
    /// needs it: after it followed the pointer to a generation, or after
    /// it read that generation's record. Meanwhile another release is
    /// activated and a later prune sweeps the generation the read is in.
    /// The read finds its generation gone, sees that the pointer names
    /// another, and reads that one: it answers with the release that is
    /// active now, and does not fail.
    #[test]
    fn a_read_whose_generation_was_swept_reads_the_pointer_again() {
        for held_at in [Reading::Followed, Reading::Recorded] {
            let (_temp, layout) = fixture();
            let first = retained(&layout, "first", 1);
            let second = retained(&layout, "second", 1);
            layout.activate(first.clone(), None).unwrap();
            let stale = named(&layout);
            let mut held = false;
            let read = layout
                .active_held(&mut |at| {
                    if at == held_at && !held {
                        held = true;
                        // An install, which keeps the generation it
                        // replaced, and then a prune, which does not.
                        let replaced = named(&layout);
                        layout
                            .activate(second.clone(), Some(first.clone()))
                            .unwrap();
                        clean_up(&layout, Some(&replaced), || Ok(false)).unwrap();
                        assert!(stale.is_dir(), "{held_at:?}: kept by the command");
                        clean_up(&layout, None, || Ok(false)).unwrap();
                        assert!(!stale.exists(), "{held_at:?}: swept by the next");
                    }
                })
                .unwrap()
                .unwrap();
            assert!(held, "{held_at:?}");
            assert_eq!(read.current.id, second.id, "{held_at:?}");
            assert_eq!(read.previous.map(|release| release.id), Some(first.id));
        }
    }

    /// The retry is for a generation the pointer has left. One it still
    /// names that cannot be read is the installation's own damage, and
    /// is reported at once, as it was.
    #[test]
    fn a_damaged_generation_the_pointer_still_names_is_an_error() {
        let (_temp, layout) = fixture();
        let current = retained(&layout, "current", 1);
        layout.activate(current, None).unwrap();
        let in_use = named(&layout);
        let mut reads = 0;
        let payload = in_use.join("payload");
        let target = payload.read_link().unwrap();
        std::fs::remove_file(&payload).unwrap();
        let failed = layout.active_held(&mut |at| reads += usize::from(at == Reading::Followed));
        assert!(
            format!("{:#}", failed.unwrap_err()).contains("cannot read"),
            "the link is gone"
        );
        assert_eq!(reads, 1, "not read again");
        symlink(&target, &payload).unwrap();
        std::fs::remove_file(in_use.join("activation.json")).unwrap();
        let mut reads = 0;
        let failed = layout.active_held(&mut |at| reads += usize::from(at == Reading::Followed));
        assert!(
            format!("{:#}", failed.unwrap_err()).contains("cannot open"),
            "the record is gone"
        );
        assert_eq!(reads, 1, "not read again");
    }
}
