//! What an installation's files say about it.
//!
//! `Layout::active` reads the `current` pointer and nothing else, so an
//! installation whose pointer was swung and whose application was never
//! published, or whose command links are half removed, read as installed.
//! This reads the rest: the retained releases, each command link, the
//! launcher people open, and puts them together into one answer with the
//! way out of whatever is wrong.
//!
//! It only reads. Nothing here creates, repairs or removes a file, takes
//! the installation lock or stops at the first thing that is wrong: a
//! piece that cannot be read is reported as not this installation's.
//!
//! Two things the files cannot tell, and this does not pretend to:
//! nothing on disk records that an uninstall happened, or that an
//! operation is in progress now. "Deactivated" is therefore what the
//! files look like after an uninstall (retained releases, no pointer, no
//! launchers), which is also what a first install looks like when it
//! stopped before it made anything outside its own folder.

use std::path::Path;

use serde::Serialize;

use super::{Activation, BINARIES, Layout, tree_hash};

/// One file the installer puts in a place people also use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Piece {
    /// Nothing is there.
    Missing,
    /// What this installation puts there.
    Ours,
    /// Something else: another program's, an edited copy, or what an
    /// interrupted operation left half written or half removed. The
    /// installer never replaces or removes it.
    Foreign,
}

/// The launcher people open, as found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Launcher {
    /// Nothing is there.
    Missing,
    /// The launcher file of this prefix, exactly (where the launcher is a
    /// file that follows `current` by itself).
    Exact,
    /// A copy, byte for byte, of the retained release of this id (where
    /// the launcher is the application bundle). `redirects` is whether
    /// that bundle can run a release other than its own.
    Copy { release: String, redirects: bool },
    /// A form earlier releases installed, which follows `current` by
    /// itself: the next activation replaces it.
    Earlier,
    /// Not this installation's, or not recognisable as its own.
    Foreign,
}

/// The active release, as far as the classification needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Active {
    pub release: String,
    /// Its files are in `versions/`.
    pub retained: bool,
    /// It supplies a launcher of its own. A release that does not keeps
    /// the launcher of an earlier one that can run it.
    pub supplies_launcher: bool,
}

/// Everything read from disk, before any conclusion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Facts {
    pub active: Option<Active>,
    /// How many releases are kept in `versions/`.
    pub retained: usize,
    /// Each command link, by name, in the order they are made.
    pub links: Vec<(&'static str, Piece)>,
    pub launcher: Launcher,
    /// The per-user link kept beside a launcher in the system folder;
    /// None where there is no such link.
    pub legacy: Option<Piece>,
    /// Whether the launcher is an application bundle that is published
    /// after the pointer moves (it is a file made before it otherwise).
    pub bundle: bool,
    /// The installation these facts are of, when it is not the one a
    /// command with no `--prefix` acts on: every command a way out names
    /// has to name it, or following the advice would act on another
    /// installation.
    pub prefix: Prefix,
    /// Where the launcher and the legacy link are, for the words.
    pub application: String,
    pub legacy_path: Option<String>,
    pub bin: String,
}

/// Which installation a command has to be told to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Prefix {
    /// The one a command acts on when it names none.
    Default,
    /// Another, by its path.
    Named(String),
    /// Another, whose path cannot be written in a command line.
    Unwritable,
}

/// A word of a command line for a POSIX shell, exactly: single quotes
/// around it, and each single quote inside it closed, escaped and opened
/// again.
fn quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The conclusion: what `desktop status` prints beside the pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Condition {
    /// `absent`: nothing is installed and nothing is kept.
    /// `active`: the pointer names a release; `launcher` and `links` say
    /// whether the rest of it is in place.
    /// `deactivated`: releases are kept and nothing is active.
    /// `interrupted`: nothing is active, yet pieces of an installation
    /// are in place: an install stopped before it was activated.
    pub installation: &'static str,
    /// `published`, `kept` (an earlier release's launcher, kept because
    /// the active one supplies none), `stale` (another release's),
    /// `earlier` (an older form that follows the pointer by itself),
    /// `missing` or `foreign`.
    pub launcher: &'static str,
    /// The release the application is a copy of, when it is not the
    /// active one's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher_release: Option<String>,
    /// `complete`, `partial` or `none`: the command links that are this
    /// installation's.
    pub links: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_links: Vec<String>,
    /// What is in the installer's places and is not its own: it is never
    /// replaced or removed, and an install or an uninstall is refused
    /// while it is there.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<String>,
    /// How many releases are kept.
    pub retained: usize,
    /// What is wrong, each with what to do about it. Empty when the
    /// installation is whole, absent or deactivated with nothing in the
    /// way.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub way_out: Vec<String>,
}

/// What to do about something found where the installer puts its own
/// files. Preflight's refusals end with it, and status says it for each
/// such thing.
pub(super) const PRESERVED: &str = "it is not replaced or removed. If an install or an uninstall of AgentDocker was interrupted, it can be what that left half written or half removed: remove it yourself and run the command again. If it is something else of yours, move it or install under another --prefix";

/// The conclusion the facts allow. Pure: every row of the table in the
/// tests is a set of facts and the answer to it.
pub(super) fn condition(facts: &Facts) -> Condition {
    // A command of this installation, as it has to be typed: one that
    // names no prefix acts on the default installation, whichever this is.
    let desktop = |rest: &str| match &facts.prefix {
        Prefix::Default => format!("`agentdocker desktop {rest}`"),
        Prefix::Named(prefix) => {
            format!("`agentdocker desktop --prefix {} {rest}`", quoted(prefix))
        }
        Prefix::Unwritable => format!(
            "`agentdocker desktop {rest}` with the same `--prefix` this was asked with (its path cannot be written here)"
        ),
    };
    let uninstall = format!(
        "{} removes what is left of it, and an install after that puts all of it back",
        desktop("uninstall")
    );
    let install = desktop("install --from <package>");
    let rollback = desktop("rollback");

    let missing_links: Vec<String> = facts
        .links
        .iter()
        .filter(|(_, piece)| *piece == Piece::Missing)
        .map(|(name, _)| (*name).to_owned())
        .collect();
    let ours = facts
        .links
        .iter()
        .filter(|(_, piece)| *piece == Piece::Ours)
        .count();
    let links = if ours == facts.links.len() && ours > 0 {
        "complete"
    } else if ours == 0 {
        "none"
    } else {
        "partial"
    };
    let mut foreign: Vec<String> = facts
        .links
        .iter()
        .filter(|(_, piece)| *piece == Piece::Foreign)
        .map(|(name, _)| format!("{}/{name}", facts.bin))
        .collect();
    if facts.launcher == Launcher::Foreign {
        foreign.push(facts.application.clone());
    }
    if facts.legacy == Some(Piece::Foreign)
        && let Some(path) = &facts.legacy_path
    {
        foreign.push(path.clone());
    }

    let (launcher, launcher_release) = match (&facts.launcher, &facts.active) {
        (Launcher::Missing, _) => ("missing", None),
        (Launcher::Exact, _) => ("published", None),
        (Launcher::Earlier, _) => ("earlier", None),
        (Launcher::Foreign, _) => ("foreign", None),
        // A release that supplies its launcher is published when the
        // application is its own, byte for byte.
        (Launcher::Copy { release, .. }, Some(active)) if active.supplies_launcher => {
            if *release == active.release {
                ("published", None)
            } else {
                ("stale", Some(release.clone()))
            }
        }
        // One that supplies none runs under an application a later
        // release left, which is how it was installed: any that can run
        // a release other than its own will do, and none that cannot,
        // its own included.
        (Launcher::Copy { release, redirects }, Some(_)) => {
            if *redirects {
                ("kept", Some(release.clone()))
            } else {
                ("stale", Some(release.clone()))
            }
        }
        (Launcher::Copy { release, .. }, None) => ("stale", Some(release.clone())),
    };

    let mut way_out = Vec::new();
    let installation = match &facts.active {
        Some(active) => {
            if !active.retained {
                way_out.push(format!(
                    "The active release {} is not in the installation's versions folder. Install its package again ({install}): that puts its files back.",
                    short(&active.release)
                ));
            }
            let found = match &launcher_release {
                Some(release) => format!("release {}'s", short(release)),
                None => "missing".to_owned(),
            };
            match launcher {
                // The pointer moved and the application was not
                // published after it. A release that supplies its
                // application publishes it when installed again.
                "missing" | "stale" if facts.bundle && active.supplies_launcher => {
                    way_out.push(format!(
                        "The application at {} is {found}: the release was activated and its application was not published. Install the same package again ({install}): that publishes the application and leaves the active release as it is.",
                        facts.application
                    ))
                }
                // One that supplies none cannot: installing it again is
                // refused for want of an application that can run it.
                "missing" | "stale" if facts.bundle => way_out.push(format!(
                    "The application at {} is {found}, and the active release {} supplies none of its own: it runs under one a later release leaves, which has to be able to run it. Installing the same package again is refused for that reason. Install a current package ({install}): it becomes the active release and publishes its application; {rollback} then returns to this release under that application, where the two keep their state in the same form.",
                    facts.application,
                    short(&active.release)
                )),
                "missing" => way_out.push(format!(
                    "The launcher {} is missing, as an uninstall that stopped part-way leaves it. Installing the same package again does not write it: {uninstall}.",
                    facts.application
                )),
                _ => {}
            }
            if !missing_links.is_empty() {
                way_out.push(format!(
                    "Of the commands in {}, {} {} missing, as an uninstall that stopped part-way leaves them. Installing the same package again does not make them: {uninstall}; installing another release, or a rollback, also makes them.",
                    facts.bin,
                    missing_links.join(", "),
                    if missing_links.len() == 1 { "is" } else { "are" }
                ));
            }
            if facts.legacy == Some(Piece::Missing)
                && let Some(path) = &facts.legacy_path
            {
                way_out.push(if active.supplies_launcher {
                    format!(
                        "The link {path}, which commands written by earlier releases still name, is missing. Install the same package again ({install}): that puts it back."
                    )
                } else {
                    format!(
                        "The link {path}, which commands written by earlier releases still name, is missing. Installing the same package again does not put it back, since the active release {} supplies no application of its own. Installing a current package does ({install}): it becomes the active release, and {rollback} returns to this one.",
                        short(&active.release)
                    )
                });
            }
            "active"
        }
        None => {
            let pieces = ours > 0
                || matches!(
                    facts.launcher,
                    Launcher::Exact | Launcher::Copy { .. } | Launcher::Earlier
                )
                || facts.legacy == Some(Piece::Ours);
            if pieces {
                way_out.push(format!(
                    "Launchers or commands of an installation are in place and no release is active: an install stopped before it was activated. Run the install again ({install}), which keeps what is there and finishes; or {}, which removes them.",
                    desktop("uninstall")
                ));
                "interrupted"
            } else if facts.retained > 0 {
                "deactivated"
            } else {
                "absent"
            }
        }
    };
    for path in &foreign {
        way_out.push(format!("{path} is not this installation's: {PRESERVED}."));
    }
    Condition {
        installation,
        launcher,
        launcher_release,
        links,
        missing_links,
        foreign,
        retained: facts.retained,
        way_out,
    }
}

/// A release id, short enough to read.
fn short(release: &str) -> &str {
    release.get(..12).unwrap_or(release)
}

impl Layout {
    /// One file in a shared place: nothing, the link this installation
    /// makes there, or something else. A path that cannot be read is
    /// something else.
    fn piece(path: &Path, target: &Path) -> Piece {
        match path.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Piece::Missing,
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    && path.read_link().is_ok_and(|found| found == target) =>
            {
                Piece::Ours
            }
            _ => Piece::Foreign,
        }
    }

    /// The launcher as found. Where it is a file, it is this prefix's
    /// launcher or it is not. Where it is the application bundle, what
    /// preflight would accept as replaceable is this installation's, and
    /// its bytes say which retained release it is a copy of.
    fn launcher_found(&self) -> Launcher {
        let metadata = match self.application.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Launcher::Missing,
            Err(_) => return Launcher::Foreign,
            Ok(metadata) => metadata,
        };
        if !cfg!(target_os = "macos") {
            let exact = metadata.is_file()
                && self.launcher().is_ok_and(|expected| {
                    std::fs::read_to_string(&self.application).is_ok_and(|found| found == expected)
                });
            return if exact {
                Launcher::Exact
            } else {
                Launcher::Foreign
            };
        }
        if !self.application_is_replaceable().unwrap_or(false) {
            return Launcher::Foreign;
        }
        // Replaceable and not a plain copy of a retained release: the
        // symlink or the marked launcher of earlier releases.
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Launcher::Earlier;
        }
        let retained = tree_hash(&self.application).ok().filter(|hash| {
            self.root
                .join("versions")
                .join(hash)
                .join("AgentDocker.app")
                .is_dir()
        });
        match retained {
            Some(release) => {
                let build = self.application.join("Contents/Resources/build.json");
                let redirects = std::fs::read(&build)
                    .ok()
                    .filter(|bytes| bytes.len() <= 1024 * 1024)
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .is_some_and(|value| value["launcher_redirect"] == 1);
                Launcher::Copy { release, redirects }
            }
            None => Launcher::Earlier,
        }
    }

    /// How a command has to name this installation: not at all when it
    /// is the one a command with no `--prefix` acts on, the home's.
    fn prefix_to_name(&self) -> Prefix {
        let home = std::env::home_dir().and_then(|home| super::project::try_canonical(&home).ok());
        if home.as_deref() == Some(self.prefix.as_path()) {
            return Prefix::Default;
        }
        match self.prefix.to_str() {
            Some(prefix) => Prefix::Named(prefix.to_owned()),
            None => Prefix::Unwritable,
        }
    }

    /// Everything the classification reads, read once.
    pub(super) fn facts(&self, active: Option<&Activation>) -> Facts {
        let retained = std::fs::read_dir(self.root.join("versions"))
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.len() == 64
                            && name.bytes().all(|byte| byte.is_ascii_hexdigit())
                            && entry.path().is_dir()
                    })
                    .count()
            })
            .unwrap_or(0);
        Facts {
            active: active.map(|activation| Active {
                release: activation.current.id.clone(),
                retained: self.payload(&activation.current).is_dir(),
                supplies_launcher: activation.current.launcher_redirect == 1,
            }),
            retained,
            links: BINARIES
                .iter()
                .copied()
                .zip(self.links())
                .map(|(name, (path, target))| (name, Self::piece(&path, &target)))
                .collect(),
            launcher: self.launcher_found(),
            legacy: self
                .legacy_application
                .as_ref()
                .map(|path| match path.symlink_metadata() {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Piece::Missing,
                    _ if self.path_is_replaceable(path).unwrap_or(false) => Piece::Ours,
                    _ => Piece::Foreign,
                }),
            bundle: cfg!(target_os = "macos"),
            prefix: self.prefix_to_name(),
            application: self.application.display().to_string(),
            legacy_path: self
                .legacy_application
                .as_ref()
                .map(|path| path.display().to_string()),
            bin: self.bin.display().to_string(),
        }
    }

    /// The installation as its files have it.
    pub(super) fn condition(&self, active: Option<&Activation>) -> Condition {
        condition(&self.facts(active))
    }
}

// The fixture is made of symbolic links, which is what the installer
// makes where these tests run.
#[cfg(all(test, unix))]
mod tests {
    use super::super::tests::release;
    use super::*;
    use std::os::unix::fs::symlink;

    const ACTIVE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A whole installation, on a system whose launcher is the
    /// application bundle: each row of the table changes one thing in it.
    fn whole() -> Facts {
        Facts {
            active: Some(Active {
                release: ACTIVE.into(),
                retained: true,
                supplies_launcher: true,
            }),
            retained: 2,
            links: BINARIES.iter().map(|name| (*name, Piece::Ours)).collect(),
            launcher: Launcher::Copy {
                release: ACTIVE.into(),
                redirects: true,
            },
            legacy: None,
            bundle: true,
            prefix: Prefix::Default,
            application: "/prefix/Applications/AgentDocker.app".into(),
            legacy_path: None,
            bin: "/prefix/.local/bin".into(),
        }
    }

    /// Nothing of an installation anywhere.
    fn nothing() -> Facts {
        Facts {
            active: None,
            retained: 0,
            links: BINARIES
                .iter()
                .map(|name| (*name, Piece::Missing))
                .collect(),
            launcher: Launcher::Missing,
            ..whole()
        }
    }

    fn copy_of(release: &str, redirects: bool) -> Launcher {
        Launcher::Copy {
            release: release.into(),
            redirects,
        }
    }

    /// The derived view, row by row: the facts, then the installation,
    /// the launcher, the links, and a phrase each way out must contain
    /// (none when nothing is wrong).
    #[test]
    fn the_condition_follows_from_the_files() {
        type Change = fn(&mut Facts);
        /// What the row is, what it changes in a whole installation, and
        /// then the installation, the launcher, the links and the ways
        /// out expected of it.
        type Row = (
            &'static str,
            Change,
            &'static str,
            &'static str,
            &'static str,
            &'static [&'static str],
        );
        let rows: &[Row] = &[
            ("whole", |_| {}, "active", "published", "complete", &[]),
            (
                "the pointer moved and the application was not published: another release's is there",
                |f| f.launcher = copy_of(OTHER, true),
                "active",
                "stale",
                "complete",
                &["was not published. Install the same package again"],
            ),
            (
                "the same on a first install: no application yet",
                |f| f.launcher = Launcher::Missing,
                "active",
                "missing",
                "complete",
                &[
                    "is missing: the release was activated",
                    "Install the same package again",
                ],
            ),
            (
                "an older release that supplies no launcher keeps a later one's",
                |f| {
                    f.active.as_mut().unwrap().supplies_launcher = false;
                    f.launcher = copy_of(OTHER, true);
                },
                "active",
                "kept",
                "complete",
                &[],
            ),
            (
                "it cannot keep one that cannot run it, and installing it again would be refused",
                |f| {
                    f.active.as_mut().unwrap().supplies_launcher = false;
                    f.launcher = copy_of(OTHER, false);
                },
                "active",
                "stale",
                "complete",
                &[
                    "supplies none of its own",
                    "Installing the same package again is refused",
                    "Install a current package",
                ],
            ),
            (
                "nor is a copy of itself an application it can run under",
                |f| {
                    f.active.as_mut().unwrap().supplies_launcher = false;
                    f.launcher = copy_of(ACTIVE, false);
                },
                "active",
                "stale",
                "complete",
                &["Install a current package"],
            ),
            (
                "the same release with no application at all",
                |f| {
                    f.active.as_mut().unwrap().supplies_launcher = false;
                    f.launcher = Launcher::Missing;
                },
                "active",
                "missing",
                "complete",
                &[
                    "Installing the same package again is refused",
                    "it becomes the active release",
                    "`agentdocker desktop rollback` then returns to this release",
                ],
            ),
            (
                "the same release with only its compatibility link gone",
                |f| {
                    f.active.as_mut().unwrap().supplies_launcher = false;
                    f.launcher = copy_of(OTHER, true);
                    f.legacy = Some(Piece::Missing);
                    f.legacy_path = Some("/prefix/Applications/AgentDocker.app".into());
                    f.application = "/Applications/AgentDocker.app".into();
                },
                "active",
                "kept",
                "complete",
                &[
                    "Installing the same package again does not put it back",
                    "Installing a current package does",
                ],
            ),
            (
                "a launcher of an earlier form follows the pointer by itself",
                |f| f.launcher = Launcher::Earlier,
                "active",
                "earlier",
                "complete",
                &[],
            ),
            (
                "where the launcher is a file, it is this prefix's",
                |f| {
                    f.bundle = false;
                    f.launcher = Launcher::Exact;
                },
                "active",
                "published",
                "complete",
                &[],
            ),
            (
                "a launcher file gone, as a stopped uninstall leaves it",
                |f| {
                    f.bundle = false;
                    f.launcher = Launcher::Missing;
                },
                "active",
                "missing",
                "complete",
                &[
                    "Installing the same package again does not write it",
                    "`agentdocker desktop uninstall`",
                ],
            ),
            (
                "one command gone",
                |f| f.links[1].1 = Piece::Missing,
                "active",
                "published",
                "partial",
                &[
                    "agentd is missing",
                    "does not make them",
                    "`agentdocker desktop uninstall`",
                ],
            ),
            (
                "every command gone",
                |f| f.links.iter_mut().for_each(|link| link.1 = Piece::Missing),
                "active",
                "published",
                "none",
                &["agentdocker, agentd, agentdocker-ui are missing"],
            ),
            (
                "the active release's files are not kept",
                |f| f.active.as_mut().unwrap().retained = false,
                "active",
                "published",
                "complete",
                &["is not in the installation's versions folder"],
            ),
            (
                "the link earlier releases' commands name is gone",
                |f| {
                    f.legacy = Some(Piece::Missing);
                    f.legacy_path = Some("/prefix/Applications/AgentDocker.app".into());
                    f.application = "/Applications/AgentDocker.app".into();
                },
                "active",
                "published",
                "complete",
                &["which commands written by earlier releases still name, is missing"],
            ),
            (
                "something else where the application goes",
                |f| f.launcher = Launcher::Foreign,
                "active",
                "foreign",
                "complete",
                &[
                    "/prefix/Applications/AgentDocker.app is not this installation's",
                    "remove it yourself",
                ],
            ),
            (
                "something else where a command goes",
                |f| f.links[0].1 = Piece::Foreign,
                "active",
                "published",
                "partial",
                &["/prefix/.local/bin/agentdocker is not this installation's"],
            ),
            (
                "nothing anywhere",
                |f| *f = nothing(),
                "absent",
                "missing",
                "none",
                &[],
            ),
            (
                "releases kept, nothing active, nothing in place",
                |f| {
                    *f = nothing();
                    f.retained = 3;
                },
                "deactivated",
                "missing",
                "none",
                &[],
            ),
            (
                "a first install stopped after it made a command",
                |f| {
                    *f = nothing();
                    f.retained = 1;
                    f.links[0].1 = Piece::Ours;
                },
                "interrupted",
                "missing",
                "partial",
                &[
                    "an install stopped before it was activated",
                    "Run the install again",
                ],
            ),
            (
                "or after it wrote the launcher file",
                |f| {
                    *f = nothing();
                    f.bundle = false;
                    f.launcher = Launcher::Exact;
                },
                "interrupted",
                "published",
                "none",
                &["an install stopped before it was activated"],
            ),
            (
                "nothing installed, and another program's launcher in the way",
                |f| {
                    *f = nothing();
                    f.launcher = Launcher::Foreign;
                },
                "absent",
                "foreign",
                "none",
                &["is not this installation's"],
            ),
        ];
        for (name, change, installation, launcher, links, ways) in rows {
            let mut facts = whole();
            change(&mut facts);
            let found = condition(&facts);
            assert_eq!(
                (found.installation, found.launcher, found.links),
                (*installation, *launcher, *links),
                "{name}: {found:?}"
            );
            assert_eq!(
                found.way_out.is_empty(),
                ways.is_empty(),
                "{name}: {:?}",
                found.way_out
            );
            for way in *ways {
                assert!(
                    found.way_out.iter().any(|said| said.contains(way)),
                    "{name}: no way out says {way:?} in {:?}",
                    found.way_out
                );
            }
        }
        // The release the application is a copy of is named when it is
        // not the active one's, and only then.
        let mut stale = whole();
        stale.launcher = copy_of(OTHER, true);
        assert_eq!(condition(&stale).launcher_release.as_deref(), Some(OTHER));
        assert_eq!(condition(&whole()).launcher_release, None);
        // What is in the way is listed, each thing once.
        let mut blocked = whole();
        blocked.links[2].1 = Piece::Foreign;
        blocked.launcher = Launcher::Foreign;
        assert_eq!(
            condition(&blocked).foreign,
            vec![
                "/prefix/.local/bin/agentdocker-ui".to_owned(),
                "/prefix/Applications/AgentDocker.app".to_owned()
            ]
        );
    }

    /// Every command a way out names acts on the installation the facts
    /// are of. A command with no `--prefix` acts on the home's, so for
    /// any other each one names its prefix, quoted exactly for a shell:
    /// read back by a real shell, the word is the path. Without it,
    /// following the advice for a trial installation would have
    /// uninstalled the home's.
    #[test]
    fn every_command_names_the_installation_it_is_for() {
        let awkward = "/tmp/a prefix with 'quotes' and $dollars";
        let named = |change: fn(&mut Facts)| {
            let mut facts = whole();
            facts.prefix = Prefix::Named(awkward.into());
            change(&mut facts);
            condition(&facts).way_out
        };
        let argument = quoted(awkward);
        assert_eq!(
            argument,
            r"'/tmp/a prefix with '\''quotes'\'' and $dollars'"
        );
        let read_back = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {argument}"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(read_back.stdout).unwrap(), awkward);

        // Each state whose way out names a command: an uninstall that
        // stopped (commands, or the launcher file), an activation that
        // was not published, an older release without its application or
        // its link, a release whose files are gone, and an install that
        // stopped before it was activated.
        let states: &[fn(&mut Facts)] = &[
            |f| f.links[0].1 = Piece::Missing,
            |f| {
                f.bundle = false;
                f.launcher = Launcher::Missing;
            },
            |f| f.launcher = Launcher::Missing,
            |f| {
                f.active.as_mut().unwrap().supplies_launcher = false;
                f.launcher = Launcher::Missing;
            },
            |f| {
                f.active.as_mut().unwrap().supplies_launcher = false;
                f.launcher = copy_of(OTHER, true);
                f.legacy = Some(Piece::Missing);
                f.legacy_path = Some("/prefix/Applications/AgentDocker.app".into());
            },
            |f| f.active.as_mut().unwrap().retained = false,
            |f| {
                *f = Facts {
                    prefix: Prefix::Named("/tmp/a prefix with 'quotes' and $dollars".into()),
                    ..nothing()
                };
                f.links[0].1 = Piece::Ours;
            },
        ];
        for (at, state) in states.iter().enumerate() {
            let ways = named(*state);
            assert!(!ways.is_empty(), "state {at} has a way out");
            for way in &ways {
                let commands = way.matches("`agentdocker desktop ").count();
                assert!(commands > 0, "state {at} names a command: {way}");
                assert_eq!(
                    way.matches(&format!("`agentdocker desktop --prefix {argument} "))
                        .count(),
                    commands,
                    "state {at}: every command names the prefix: {way}"
                );
            }
        }
        // The home's installation is the one a bare command acts on: it
        // is not named, and no path of it is printed in a command.
        let mut home = whole();
        home.links[0].1 = Piece::Missing;
        let way = &condition(&home).way_out[0];
        assert!(way.contains("`agentdocker desktop uninstall`") && !way.contains("--prefix"));
        // A prefix that cannot be written is not guessed at: the command
        // is given with the instruction to name it.
        let mut unwritable = whole();
        unwritable.prefix = Prefix::Unwritable;
        unwritable.links[0].1 = Piece::Missing;
        let way = &condition(&unwritable).way_out[0];
        assert!(
            way.contains("with the same `--prefix` this was asked with"),
            "{way}"
        );
    }

    /// Swing `current` back to the generation that activated `release`,
    /// as a pointer that moved without the application being published
    /// leaves it: the pointer names one release, the application is
    /// another's.
    #[cfg(target_os = "macos")]
    fn point_at(layout: &Layout, release: &str) {
        let generation = std::fs::read_dir(layout.root.join("generations"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                std::fs::read_to_string(path.join("activation.json"))
                    .ok()
                    .and_then(|text| serde_json::from_str::<Activation>(&text).ok())
                    .is_some_and(|activation| activation.current.id == release)
            })
            .expect("the release was activated once");
        let staging = tempfile::tempdir_in(&layout.root).unwrap();
        symlink(&generation, staging.path().join("current")).unwrap();
        std::fs::rename(staging.path().join("current"), layout.root.join("current")).unwrap();
    }

    /// An older release that supplies no application of its own runs
    /// under one a later release left. When that application is gone, or
    /// is one that cannot run it, installing the older package again is
    /// not the way out: the installer refuses it. The way out given is the
    /// one that works, and both halves of it are run here.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_older_active_release_is_given_the_way_out_that_works_for_it() {
        use super::super::tests::rehash;
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let modern = release(&layout, "modern");
        let mut older = release(&layout, "older");
        older.launcher_redirect = 0;
        std::fs::write(
            layout.payload(&older).join("Contents/Resources/build.json"),
            serde_json::json!({"format": 1, "product": "agentdocker", "installation_lock": 1})
                .to_string(),
        )
        .unwrap();
        let older = rehash(&layout, older);
        layout.activate(modern.clone(), None).unwrap();
        layout
            .activate(older.clone(), Some(modern.clone()))
            .unwrap();

        // Whole: the later release's application is kept, by design.
        let active = layout.active().unwrap();
        assert_eq!(active.as_ref().unwrap().current, older);
        let found = layout.condition(active.as_ref());
        assert_eq!(
            (found.installation, found.launcher),
            ("active", "kept"),
            "{found:?}"
        );
        assert_eq!(found.launcher_release.as_deref(), Some(modern.id.as_str()));
        assert!(found.way_out.is_empty(), "{:?}", found.way_out);

        // The application gone, as an uninstall that stopped leaves it.
        std::fs::remove_dir_all(&layout.application).unwrap();
        let found = layout.condition(active.as_ref());
        assert_eq!(found.launcher, "missing");
        assert!(
            found.way_out[0].contains("Installing the same package again is refused")
                && found.way_out[0].contains("Install a current package"),
            "{:?}",
            found.way_out
        );
        // The installer does refuse the same package here.
        assert!(layout.check_launcher_support(&older).is_err());
        // And a current package does publish an application, under which
        // the older release is active again and whole.
        layout
            .activate(modern.clone(), Some(older.clone()))
            .unwrap();
        layout
            .activate(older.clone(), Some(modern.clone()))
            .unwrap();
        let active = layout.active().unwrap();
        let found = layout.condition(active.as_ref());
        assert_eq!(
            (found.launcher, found.way_out.len()),
            ("kept", 0),
            "{found:?}"
        );

        // An application that cannot run it: a copy of the older release
        // itself, which runs only its own files.
        std::fs::remove_dir_all(&layout.application).unwrap();
        super::super::copy_payload(&layout.payload(&older), &layout.application).unwrap();
        let found = layout.condition(active.as_ref());
        assert_eq!(found.launcher, "stale", "{found:?}");
        assert_eq!(found.launcher_release.as_deref(), Some(older.id.as_str()));
        assert!(
            found.way_out[0].contains("Install a current package"),
            "{:?}",
            found.way_out
        );
        assert!(layout.check_launcher_support(&older).is_err());
    }

    /// The same answers read from real files: an installation made by the
    /// installer's own `activate`, then taken apart the way an interrupted
    /// install or uninstall leaves it. Reading it changes nothing.
    #[test]
    fn the_files_of_an_installation_say_what_state_it_is_in() {
        let tmp = tempfile::tempdir().unwrap();
        // A trial prefix, with a space and a quote in its name: every
        // command a way out gives has to act on this installation and
        // not on the home's.
        let prefix = tmp.path().join("a trial's prefix");
        let layout = Layout::new(prefix.clone()).unwrap();
        let named = format!(
            "`agentdocker desktop --prefix {} ",
            quoted(layout.prefix.to_str().unwrap())
        );

        // Nothing installed: said without making anything.
        let found = layout.condition(None);
        assert_eq!(
            (found.installation, found.launcher, found.links),
            ("absent", "missing", "none")
        );
        assert!(found.way_out.is_empty());
        assert!(!prefix.exists(), "reading creates nothing");

        // Installed whole.
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        layout.activate(first.clone(), None).unwrap();
        let active = layout.active().unwrap();
        let found = layout.condition(active.as_ref());
        assert_eq!(
            (found.installation, found.launcher, found.links),
            ("active", "published", "complete"),
            "{found:?}"
        );
        assert_eq!((found.retained, found.way_out.len()), (1, 0), "{found:?}");

        // A command removed, as an uninstall that stopped after its first
        // step leaves it.
        let command = layout.bin.join("agentdocker");
        std::fs::remove_file(&command).unwrap();
        let found = layout.condition(active.as_ref());
        assert_eq!((found.installation, found.links), ("active", "partial"));
        assert_eq!(found.missing_links, vec!["agentdocker".to_owned()]);
        assert!(
            found.way_out[0].contains(&format!("{named}uninstall`")),
            "the uninstall it advises is this prefix's: {:?}",
            found.way_out
        );
        // Somebody's own file where the command goes: not ours, listed,
        // and every command that changes the installation refuses with
        // the way out.
        std::fs::write(&command, "somebody's own command").unwrap();
        let found = layout.condition(active.as_ref());
        assert_eq!(found.foreign, vec![command.display().to_string()]);
        let refused = format!("{:#}", layout.preflight().unwrap_err());
        assert!(
            refused.contains("already exists outside this installation; preserved")
                && refused.contains("remove it yourself and run the command again"),
            "{refused}"
        );
        std::fs::remove_file(&command).unwrap();
        symlink(&layout.links()[0].1, &command).unwrap();
        assert_eq!(layout.condition(active.as_ref()).links, "complete");

        // The launcher.
        if cfg!(target_os = "macos") {
            // A second release activated, then the pointer back on the
            // first: the application is the second's, which is what a
            // pointer swung without the publication that follows it
            // leaves.
            #[cfg(target_os = "macos")]
            {
                let second = release(&layout, "second");
                layout
                    .activate(second.clone(), Some(first.clone()))
                    .unwrap();
                point_at(&layout, &first.id);
                let active = layout.active().unwrap();
                assert_eq!(active.as_ref().unwrap().current, first);
                let found = layout.condition(active.as_ref());
                assert_eq!(
                    (found.installation, found.launcher),
                    ("active", "stale"),
                    "{found:?}"
                );
                assert_eq!(found.launcher_release.as_deref(), Some(second.id.as_str()));
                assert!(
                    found.way_out[0].contains("Install the same package again"),
                    "{:?}",
                    found.way_out
                );
            }
            // No application at all.
            std::fs::remove_dir_all(&layout.application).unwrap();
            let active = layout.active().unwrap();
            let found = layout.condition(active.as_ref());
            assert_eq!(found.launcher, "missing");
            assert!(found.way_out[0].contains("Install the same package again"));
            // Which is true: what installing the active release again
            // does for a release that supplies its application puts it
            // back, and the active release is as it was.
            #[cfg(target_os = "macos")]
            {
                let staged = layout.prepare_launcher_bundle(&first).unwrap();
                layout.record_application().unwrap();
                layout.repair_legacy_launcher().unwrap();
                layout.publish_launcher_bundle(staged).unwrap();
                let found = layout.condition(active.as_ref());
                assert_eq!(found.launcher, "published", "{found:?}");
                assert_eq!(layout.active().unwrap(), active);
                std::fs::remove_dir_all(&layout.application).unwrap();
            }
            // A bundle that is no release of this installation's, as a
            // removal that stopped part-way leaves one.
            std::fs::create_dir_all(layout.application.join("Contents")).unwrap();
            std::fs::write(layout.application.join("Contents/left"), "half").unwrap();
            let found = layout.condition(active.as_ref());
            assert_eq!(found.launcher, "foreign");
            let refused = format!("{:#}", layout.preflight().unwrap_err());
            assert!(
                refused.contains("already exists outside this installation; preserved")
                    && refused.contains("half written or half removed"),
                "{refused}"
            );
            std::fs::remove_dir_all(&layout.application).unwrap();
        } else {
            // The launcher file gone.
            std::fs::remove_file(&layout.application).unwrap();
            let found = layout.condition(active.as_ref());
            assert_eq!(found.launcher, "missing");
            assert!(found.way_out[0].contains("uninstall` removes what is left"));
            // An empty one, as an install that stopped while writing it
            // leaves.
            std::fs::write(&layout.application, "").unwrap();
            let found = layout.condition(active.as_ref());
            assert_eq!(found.launcher, "foreign");
            let refused = format!("{:#}", layout.preflight().unwrap_err());
            assert!(
                refused.contains("is an existing or edited launcher; preserved")
                    && refused.contains("half written or half removed"),
                "{refused}"
            );
            std::fs::remove_file(&layout.application).unwrap();
        }

        // Nothing active and a command in place: an install that stopped
        // before it was activated.
        std::fs::remove_file(layout.root.join("current")).unwrap();
        assert!(layout.active().unwrap().is_none());
        let found = layout.condition(None);
        assert_eq!(found.installation, "interrupted", "{found:?}");
        assert!(found.way_out[0].contains("Run the install again"));
        assert!(
            found.way_out[0].contains(&format!("{named}install --from <package>`"))
                && found.way_out[0].contains(&format!("{named}uninstall`")),
            "both commands are this prefix's: {:?}",
            found.way_out
        );
        // The commands gone too: releases kept and nothing active, which
        // is what an uninstall leaves.
        for (path, _) in layout.links() {
            std::fs::remove_file(path).unwrap();
        }
        let found = layout.condition(None);
        assert_eq!(found.installation, "deactivated", "{found:?}");
        assert!(found.retained >= 1 && found.way_out.is_empty());
    }
}
