//! Per-user native desktop installation. Immutable version and activation
//! directories make the current release and rollback target one atomic switch.
// The installer, updates and retained versions run on macOS and Linux;
// on Windows only the refusal in `run` is live, and the rest waits its slice.
#![cfg_attr(windows, allow(dead_code))]
use std::collections::BTreeMap;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(windows)]
use std::os::windows::fs::symlink_file as symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use agentdocker_host::{command, dirs, project};

mod maintenance;
mod update;

/// Where Homebrew keeps the cask's record when it installed the app. The
/// app in `/Applications` is then Homebrew's copy: this installer must
/// neither update it nor put a second one beside it.
fn homebrew_caskrooms() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = std::env::var_os("HOMEBREW_PREFIX")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    roots.extend(["/opt/homebrew", "/usr/local"].map(PathBuf::from));
    roots
        .into_iter()
        .map(|prefix| prefix.join("Caskroom/agentdocker-app"))
        .collect()
}

/// The Homebrew cask record that owns an AgentDocker app on this machine:
/// present when the cask is installed and there is no managed activation.
/// The app itself may sit anywhere (`brew --appdir`), so the record, not
/// the application path, is the evidence.
fn homebrew_owner(active: Option<&Activation>, caskrooms: &[PathBuf]) -> Option<PathBuf> {
    if active.is_some() {
        return None;
    }
    caskrooms.iter().find(|room| room.is_dir()).cloned()
}

/// Refuse to install or update beside Homebrew's copy: one installation
/// per machine, maintained by the tool that made it.
fn refuse_homebrew_copy(active: Option<&Activation>) -> Result<()> {
    if let Some(room) = homebrew_owner(active, &homebrew_caskrooms()) {
        bail!(
            "AgentDocker is installed by Homebrew ({}); update it with `brew upgrade --cask agentdocker-app` or remove it with `brew uninstall --cask agentdocker-app` rather than installing a second copy beside it",
            room.display()
        );
    }
    Ok(())
}

const BINARIES: &[&str] = &["agentdocker", "agentd", "agentdocker-ui"];

/// How many times to re-follow the `current` pointer when a read of it
/// loses the race with the `rename` that moves it. See [`Layout::follow`].
const POINTER_ATTEMPTS: usize = 8;
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Use the managed command link for provider setup so registrations survive
/// activation of another release. Refuse setup from an obsolete running copy.
pub fn setup_executable() -> Result<PathBuf> {
    stable_executable(&agentdocker_host::procinfo::executable_path()?)
}

fn stable_executable(executable: &Path) -> Result<PathBuf> {
    for ancestor in executable.ancestors() {
        if !ancestor.ends_with(".local/share/agentdocker/desktop/versions") {
            continue;
        }
        let prefix = ancestor
            .ancestors()
            .nth(5)
            .context("managed installation lacks a prefix")?;
        let layout = Layout::new(prefix.to_owned())?;
        let active = layout
            .active()?
            .context("managed installation is inactive")?;
        ensure!(
            layout
                .payload(&active.current)
                .join(layout.binary_subdir())
                .join("agentdocker")
                == executable,
            "this copy is no longer active; reopen agentdocker before configuring integrations"
        );
        layout.preflight()?;
        let command = layout.bin.join("agentdocker");
        ensure!(
            command.canonicalize()? == executable,
            "managed command link changed"
        );
        return Ok(command);
    }
    Ok(executable.to_owned())
}

#[derive(Args)]
pub struct DesktopArgs {
    /// Base for per-user installation (default: your home). A trial prefix keeps all launchers and versions beneath it.
    #[arg(long, global = true)]
    prefix: Option<PathBuf>,
    #[command(subcommand)]
    command: DesktopCommand,
}

#[derive(Subcommand)]
enum DesktopCommand {
    /// Inspect an app/package and install it for the next launch.
    Install {
        /// Extracted desktop package, Mac application bundle, or Linux desktop payload.
        #[arg(long)]
        from: PathBuf,
        /// Print the installation paths and changes without writing anything.
        #[arg(long)]
        preview: bool,
        /// Permit an ad-hoc-signed Mac preview; public installs require Gatekeeper acceptance.
        #[arg(long)]
        local_preview: bool,
        /// Refuse a different payload than the reviewed SHA-256 release ID.
        #[arg(long)]
        expect_release: Option<String>,
        /// Refuse a changed active installation (use "none" for a first install).
        #[arg(long)]
        expect_current: Option<String>,
    },
    /// Activate the previous retained version, only with a compatible state schema.
    Rollback {
        /// Print the rollback paths and changes without writing anything.
        #[arg(long)]
        preview: bool,
        /// Permit an ad-hoc-signed Mac preview; public installs require Gatekeeper acceptance.
        #[arg(long)]
        local_preview: bool,
        /// Refuse a different payload than the reviewed SHA-256 release ID.
        #[arg(long)]
        expect_release: Option<String>,
        /// Refuse a changed active installation.
        #[arg(long)]
        expect_current: Option<String>,
    },
    /// Remove owned launchers and deactivate this installation; preserve running sessions and settings.
    Uninstall {
        /// Print the removal plan without changing the installation.
        #[arg(long)]
        preview: bool,
        /// Refuse changes since the reviewed maintenance plan.
        #[arg(long)]
        expect_plan: Option<String>,
    },
    /// Remove unused retained versions, keeping the active and rollback versions.
    Prune {
        /// Keep this many additional inactive versions, newest first.
        #[arg(long, default_value_t = 0)]
        keep: usize,
        /// Print the retention plan without removing any versions.
        #[arg(long)]
        preview: bool,
        /// Refuse changes since the reviewed maintenance plan.
        #[arg(long)]
        expect_plan: Option<String>,
    },
    /// Show the active and previous retained versions without starting the daemon.
    Status,
    /// Check the published download feed and, unless --check, download, verify and preview the newer release; --apply installs it for the next launch.
    Update {
        /// Feed URL (https://, or file:// with --local-preview), or AGENTDOCKER_UPDATE_FEED. Without it: the latest stable release's feed, plus the preview channel when this installation is a prerelease or --local-preview is given; the newest release wins.
        #[arg(long, env = update::FEED_ENV)]
        feed: Option<String>,
        /// Only report whether a newer release exists; download nothing.
        #[arg(long)]
        check: bool,
        /// Install the verified release for the next launch instead of previewing it.
        #[arg(long)]
        apply: bool,
        /// Permit an ad-hoc-signed Mac preview and preview feeds; public updates require Gatekeeper acceptance.
        #[arg(long)]
        local_preview: bool,
        /// Socket of the agentd daemon, asked only whether agents are live; never started.
        #[arg(long, env = "AGENTDOCKER_SOCKET")]
        socket: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Release {
    id: String,
    version: String,
    source_commit: String,
    state_schema: u32,
    target: String,
    tree_sha256: String,
    #[serde(default)]
    installation_lock: u32,
    #[serde(default)]
    launcher_redirect: u32,
    payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Activation {
    format: u32,
    current: Release,
    previous: Option<Release>,
}

struct Layout {
    prefix: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    /// Where the launcher lives. On a Mac installed under the home
    /// prefix this is `/Applications/AgentDocker.app` when that folder is
    /// writable, because that is where people look; otherwise the
    /// prefix's own `Applications`.
    application: PathBuf,
    /// The per-user launcher path. When the launcher lives in the system
    /// folder, a link stays here so absolute paths written into hook and
    /// MCP configuration by earlier releases keep resolving.
    legacy_application: Option<PathBuf>,
}

impl Layout {
    fn new(prefix: PathBuf) -> Result<Self> {
        let prefix = project::try_canonical(&prefix)?;
        ensure!(prefix.is_absolute(), "installation prefix must be absolute");
        let root = prefix.join(".local/share/agentdocker/desktop");
        let user_application = prefix.join("Applications/AgentDocker.app");
        let (application, legacy_application) = if cfg!(target_os = "macos") {
            match Self::recorded_or_default_application(&root, &prefix, &user_application)? {
                Some(system) => (system, Some(user_application)),
                None => (user_application, None),
            }
        } else {
            (
                prefix.join(".local/share/applications/agentdocker.desktop"),
                None,
            )
        };
        Ok(Self {
            root,
            bin: prefix.join(".local/bin"),
            prefix,
            application,
            legacy_application,
        })
    }

    /// The system Applications folder on this platform.
    const SYSTEM_APPLICATIONS: &'static str = "/Applications";

    /// The launcher location this installation already chose, else the
    /// default for a fresh one. Recorded so a later change in folder
    /// permissions cannot move the launcher out from under the Dock.
    fn recorded_or_default_application(
        root: &Path,
        prefix: &Path,
        user_application: &Path,
    ) -> Result<Option<PathBuf>> {
        let record = root.join("launcher.json");
        match std::fs::read_to_string(&record) {
            Ok(text) => {
                // Only the two places this installer ever writes are valid.
                // A record that says anything else is damage, not a choice.
                let value: serde_json::Value = serde_json::from_str(&text)
                    .with_context(|| format!("cannot read {}", record.display()))?;
                ensure!(value["format"] == 1, "unknown launcher record format");
                let system = Path::new(Self::SYSTEM_APPLICATIONS).join("AgentDocker.app");
                match value["application"].as_str().map(Path::new) {
                    Some(path) if path == user_application => Ok(None),
                    Some(path) if path == system => {
                        ensure!(
                            std::env::home_dir()
                                .and_then(|home| project::try_canonical(&home).ok())
                                .as_deref()
                                == Some(prefix),
                            "a trial installation cannot use the system Applications folder"
                        );
                        let directory = Path::new(Self::SYSTEM_APPLICATIONS);
                        ensure!(
                            directory.symlink_metadata()?.is_dir()
                                && project::try_canonical(directory)? == directory,
                            "system Applications folder changed to an unsupported location"
                        );
                        Ok(Some(system))
                    }
                    _ => bail!(
                        "{} names a launcher location this installer does not manage",
                        record.display()
                    ),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default_system_application(
                    prefix,
                    std::env::home_dir().as_deref(),
                    Path::new(Self::SYSTEM_APPLICATIONS),
                ))
            }
            Err(error) => Err(error).with_context(|| format!("cannot read {}", record.display())),
        }
    }

    /// `/Applications/AgentDocker.app` for a home-prefix installation when
    /// the folder is writable and the name is free or already ours; trial
    /// prefixes and locked-down Macs stay under their own prefix.
    fn default_system_application(
        prefix: &Path,
        home: Option<&Path>,
        system: &Path,
    ) -> Option<PathBuf> {
        if home.map(|home| project::try_canonical(home).ok()) != Some(Some(prefix.to_owned())) {
            return None;
        }
        // The real folder, not a link to somewhere else, and nothing is
        // written to decide: status and previews must leave it untouched.
        let metadata = system.symlink_metadata().ok()?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return None;
        }
        if project::try_canonical(system).ok()? != system {
            return None;
        }
        if !Self::writable(system) {
            return None;
        }
        let candidate = system.join("AgentDocker.app");
        match candidate.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(candidate),
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                let marker = candidate.join("Contents/Resources/managed-launcher.json");
                let ours = std::fs::read_to_string(marker)
                    .ok()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                    .is_some_and(|value| {
                        value["product"] == "agentdocker"
                            && value["root"].as_str()
                                == prefix.join(".local/share/agentdocker/desktop").to_str()
                    });
                ours.then_some(candidate)
            }
            // Somebody else's app, or a stray link: leave it alone and stay
            // under the prefix.
            _ => None,
        }
    }

    /// Whether this user may create entries in `directory`, asked of the
    /// kernel (group membership and ACLs included) without writing anything.
    fn writable(directory: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
                return false;
            };
            // SAFETY: `path` is a valid NUL-terminated C string for the call's
            // duration, and access(2) reads it without retaining it.
            unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
        }
        #[cfg(windows)]
        {
            // The installer does not run on Windows yet (see `run`); the
            // read-only attribute is the one answer the metadata gives.
            std::fs::metadata(directory)
                .map(|m| m.is_dir() && !m.permissions().readonly())
                .unwrap_or(false)
        }
    }

    /// Remember where the launcher went: written whole or not at all.
    fn record_application(&self) -> Result<()> {
        let record = self.root.join("launcher.json");
        let staging = tempfile::Builder::new()
            .prefix(".launcher.json.")
            .tempfile_in(&self.root)?;
        let mut file = staging.as_file();
        serde_json::to_writer_pretty(
            &mut file,
            &serde_json::json!({
                "format": 1,
                "application": self.application.to_str().context("launcher path must be UTF-8")?,
            }),
        )?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        #[cfg(unix)]
        std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o600))?;
        staging.persist(&record)?;
        std::fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    fn payload(&self, release: &Release) -> PathBuf {
        self.root
            .join("versions")
            .join(&release.id)
            .join(&release.payload)
    }

    fn binary_subdir(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "Contents/MacOS"
        } else {
            "bin"
        }
    }

    /// The command links. On macOS the application is not among them: it
    /// is an intact signed bundle copied by `prepare_launcher_bundle`,
    /// because Launchpad, Spotlight and the Dock ignore a symlinked bundle
    /// and only show one whose Info.plist they can read in place.
    fn links(&self) -> Vec<(PathBuf, PathBuf)> {
        let active = self.root.join("current/payload");
        BINARIES
            .iter()
            .map(|name| {
                (
                    self.bin.join(name),
                    active.join(self.binary_subdir()).join(name),
                )
            })
            .collect()
    }

    /// Tell Launch Services about the launcher so it appears in Launchpad
    /// and Spotlight at once instead of after the next login. Best effort,
    /// and only for the real home prefix: trial prefixes must not register
    /// throwaway bundles on the machine.
    fn register_launcher(&self) {
        self.launch_services("-f", &self.application);
    }

    fn launch_services(&self, operation: &str, application: &Path) {
        if !cfg!(target_os = "macos")
            || Some(self.prefix.as_path()) != std::env::home_dir().as_deref()
        {
            return;
        }
        let lsregister = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
        if !Path::new(lsregister).is_file() {
            return;
        }
        let _ = std::process::Command::new(lsregister)
            .arg(operation)
            .arg(application)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// Copy the signed bundle without adding markers, changing Info.plist or
    /// substituting executables. Ownership stays in launcher.json outside it.
    /// Preparation never changes the app the person can launch.
    fn prepare_launcher_bundle(&self, release: &Release) -> Result<tempfile::TempDir> {
        ensure!(
            release.launcher_redirect == 1,
            "release has no managed launcher redirect contract"
        );
        let parent = self
            .application
            .parent()
            .context("launcher has no parent")?;
        std::fs::create_dir_all(parent)?;
        let staging = tempfile::Builder::new()
            .prefix(".AgentDocker.app.")
            .tempdir_in(parent)?;
        let bundle = staging.path().join("AgentDocker.app");
        copy_payload(&self.payload(release), &bundle)?;
        ensure!(
            tree_hash(&bundle)? == release.id,
            "launcher copy differs from the inspected release"
        );
        sync_tree(staging.path())?;
        Ok(staging)
    }

    /// A rollback may select an older payload, but must retain a launcher
    /// that execs it under its immutable path and lifetime pin.
    fn check_launcher_support(&self, release: &Release) -> Result<()> {
        if !cfg!(target_os = "macos") || release.launcher_redirect == 1 {
            return Ok(());
        }
        let metadata = self.application.join("Contents/Resources/build.json");
        let value: serde_json::Value = match agentdocker_host::files::open_regular(&metadata) {
            Ok(file) if file.metadata()?.len() <= 1024 * 1024 => {
                serde_json::from_reader(file.take(1024 * 1024)).unwrap_or_default()
            }
            _ => serde_json::Value::Null,
        };
        ensure!(
            self.copied_launcher_version()?.is_some() && value["launcher_redirect"] == 1,
            "this older release needs an existing compatible launcher; install a current package first"
        );
        Ok(())
    }

    fn copied_launcher_version(&self) -> Result<Option<String>> {
        if !cfg!(target_os = "macos")
            || !self
                .application
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            || !self.application_is_replaceable()?
        {
            return Ok(None);
        }
        let Ok(hash) = tree_hash(&self.application) else {
            return Ok(None);
        };
        Ok(self
            .root
            .join("versions")
            .join(&hash)
            .join("AgentDocker.app")
            .is_dir()
            .then_some(hash))
    }

    /// Publish after activation: copied entrypoints follow current before
    /// parsing arguments. macOS exchanges existing bundles atomically, leaving
    /// the old copy in the private staging directory for cleanup.
    fn publish_launcher_bundle(&self, staging: tempfile::TempDir) -> Result<()> {
        let bundle = staging.path().join("AgentDocker.app");
        let parent = self
            .application
            .parent()
            .context("launcher has no parent")?;
        let existed = self.application.symlink_metadata().is_ok();
        self.unregister_launcher(&self.application);
        if let Err(error) = replace_application(&bundle, &self.application, existed) {
            self.register_launcher();
            return Err(error);
        }
        if let Err(error) = std::fs::File::open(parent).and_then(|file| file.sync_all()) {
            // A failed publication restores the old app before the caller
            // restores current. The staged candidate is removed on drop.
            if existed {
                replace_application(&bundle, &self.application, true)?;
            } else {
                std::fs::rename(&self.application, &bundle)?;
            }
            self.register_launcher();
            return Err(error.into());
        }
        self.register_launcher();
        Ok(())
    }

    fn unregister_launcher(&self, application: &Path) {
        self.launch_services("-u", application);
    }

    fn repair_legacy_launcher(&self) -> Result<()> {
        let Some(legacy) = &self.legacy_application else {
            return Ok(());
        };
        if legacy.read_link().ok().as_ref() == Some(&self.application) {
            return Ok(());
        }
        let parent = legacy.parent().context("launcher has no parent")?;
        std::fs::create_dir_all(parent)?;
        let staging = tempfile::tempdir_in(parent)?;
        let link = staging.path().join("AgentDocker.app");
        symlink(&self.application, &link)?;
        self.unregister_launcher(legacy);
        replace_application(&link, legacy, legacy.symlink_metadata().is_ok())?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    /// Whether the macOS application path holds something this store may
    /// replace: nothing, the symlink earlier releases installed, or our
    /// own launcher bundle.
    fn application_is_replaceable(&self) -> Result<bool> {
        self.path_is_replaceable(&self.application)
    }

    /// Whether `path` holds something this store may replace: nothing, the
    /// symlink earlier releases installed, a compatibility link to our own
    /// launcher, or our own marked launcher bundle.
    fn path_is_replaceable(&self, path: &Path) -> Result<bool> {
        match path.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = path.read_link()?;
                Ok(target == self.root.join("current/payload") || target == self.application)
            }
            Ok(metadata) if metadata.is_dir() => {
                let marker = path.join("Contents/Resources/managed-launcher.json");
                // Migrate the old neutral launcher, whose linked signed
                // executables did not match its Info.plist.
                if let Ok(text) = std::fs::read_to_string(&marker) {
                    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                    if value["product"] == "agentdocker"
                        && value["root"].as_str() == self.root.to_str()
                    {
                        return Ok(true);
                    }
                }
                let record: serde_json::Value = std::fs::read(self.root.join("launcher.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default();
                if record["format"] != 1
                    || record["application"].as_str() != self.application.to_str()
                {
                    return Ok(false);
                }
                // An externally recorded app is replaceable only while it is
                // byte-for-byte one of this store's immutable releases. An
                // unrelated or user-edited app is preserved.
                let Ok(hash) = tree_hash(path) else {
                    return Ok(false);
                };
                let retained = self
                    .root
                    .join("versions")
                    .join(&hash)
                    .join("AgentDocker.app");
                Ok(retained.is_dir() && tree_hash(&retained)? == hash)
            }
            Ok(_) => Ok(false),
        }
    }

    fn launcher(&self) -> Result<String> {
        let executable = self.root.join("current/payload/bin/agentdocker-ui");
        let icon = self
            .root
            .join("current/payload/share/icons/hicolor/scalable/apps/agentdocker.svg");
        let text = |path: &Path| -> Result<String> {
            let text = path.to_str().context("launcher path must be UTF-8")?;
            ensure!(
                !text.chars().any(char::is_control),
                "launcher paths cannot contain control characters"
            );
            Ok(text.to_owned())
        };
        let executable = text(&executable)?;
        ensure!(
            !executable.contains('='),
            "desktop executable paths cannot contain '='"
        );
        // Exec has two decoding layers: Desktop Entry strings, then quoted
        // command arguments. Icon only has the first, with literal percent signs.
        let executable = executable
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('%', "%%")
            .replace('\\', "\\\\");
        let icon = text(&icon)?.replace('\\', "\\\\");
        Ok(format!(
            "[Desktop Entry]\nType=Application\nName=AgentDocker\nComment=Orchestrate local AI agents\nExec=\"{}\"\nIcon={}\nTerminal=false\nCategories=Development;\n",
            executable, icon
        ))
    }

    fn preflight(&self) -> Result<()> {
        for path in [&self.root, &self.bin, &self.application] {
            let allowed_outside = self.legacy_application.is_some() && path == &self.application;
            if allowed_outside {
                let directory = self
                    .application
                    .parent()
                    .context("launcher has no parent")?;
                ensure!(
                    directory.symlink_metadata()?.is_dir()
                        && project::try_canonical(directory)? == directory,
                    "system Applications folder changed during installation"
                );
            }
            ensure!(
                allowed_outside || project::try_canonical(path)?.starts_with(&self.prefix),
                "installation path escapes its prefix through a symlink: {}",
                path.display()
            );
        }
        if let Some(legacy) = &self.legacy_application {
            ensure!(
                self.path_is_replaceable(legacy)?,
                "{} already exists outside this installation; preserved",
                legacy.display()
            );
        }
        for (path, target) in self.links() {
            match path.symlink_metadata() {
                Ok(metadata) => ensure!(
                    metadata.file_type().is_symlink() && path.read_link()? == target,
                    "{} already exists outside this installation; preserved",
                    path.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        if cfg!(target_os = "macos") {
            ensure!(
                self.application_is_replaceable()?,
                "{} already exists outside this installation; preserved",
                self.application.display()
            );
        } else {
            match self.application.symlink_metadata() {
                Ok(metadata) => ensure!(
                    metadata.is_file()
                        && std::fs::read_to_string(&self.application)? == self.launcher()?,
                    "{} is an existing or edited launcher; preserved",
                    self.application.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn ensure_root(&self) -> Result<()> {
        dirs::secure_state_dir(&self.root)?;
        for part in ["versions", "generations"] {
            dirs::secure_state_dir(&self.root.join(part))?;
        }
        Ok(())
    }

    /// Follow the `current` pointer, which is moving while we read it.
    ///
    /// `activate` swings this symlink with `rename`, and a `readlink`
    /// whose path is replaced mid-call can come back `EINVAL` — "not a
    /// symbolic link" — because the vnode the lookup found was unlinked
    /// and recycled before it was read. The pointer is a symlink to a
    /// complete generation on both sides of that rename, and it is one
    /// again the instant afterwards; there is nothing wrong with the
    /// installation and nothing for a reader to report. So a read that
    /// loses the race takes the next one, and only a pointer that says
    /// this several times running is a pointer worth complaining about.
    ///
    /// Measured with `readers_under_load_observe_complete_generations`:
    /// eight readers against six hundred activations failed inside the
    /// first thousand reads on every run without this, and does not with
    /// it. `readers_observe_complete_generations_during_activation` is
    /// the same shape a thousand times smaller, which is why it failed
    /// about once in seven full suite runs and looked like a flake.
    fn follow(current: &Path) -> Result<PathBuf> {
        let mut lost = None;
        for _ in 0..POINTER_ATTEMPTS {
            match current.read_link() {
                Ok(generation) => return Ok(generation),
                Err(error) if error.raw_os_error() == Some(libc::EINVAL) => lost = Some(error),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("cannot read {}", current.display()));
                }
            }
            std::thread::yield_now();
        }
        Err(lost.expect("the loop either returns or records why not"))
            .with_context(|| format!("{} is not a symlink", current.display()))
    }

    fn active(&self) -> Result<Option<Activation>> {
        let current = self.root.join("current");
        match current.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(metadata) => ensure!(
                metadata.file_type().is_symlink(),
                "installation current pointer is not a symlink"
            ),
        }
        // Each step says which file it was reading. A reader runs while
        // an activation is renaming things into place, so when one of
        // these does fail it is the interleaving that matters, and a
        // bare "Invalid argument" from an unknown call is not evidence
        // of anything.
        let generation = Self::follow(&current)?;
        ensure!(
            generation.parent() == Some(self.root.join("generations").as_path()),
            "current pointer escapes managed generations"
        );
        let metadata = generation.join("activation.json");
        let file = dirs::private_file(&metadata, false, false)
            .with_context(|| format!("cannot open {}", metadata.display()))?;
        let activation: Activation = serde_json::from_reader(file.take(64 * 1024))
            .with_context(|| format!("cannot read {}", metadata.display()))?;
        ensure!(activation.format == 1, "unknown activation format");
        validate_release(&activation.current)?;
        if let Some(previous) = &activation.previous {
            validate_release(previous)?;
        }
        let payload = generation.join("payload");
        ensure!(
            payload
                .read_link()
                .with_context(|| format!("cannot read {}", payload.display()))?
                == self.payload(&activation.current),
            "activation payload is inconsistent"
        );
        Ok(Some(activation))
    }

    fn activate(&self, current: Release, previous: Option<Release>) -> Result<()> {
        self.preflight()?;
        self.check_launcher_support(&current)?;
        // The prior release is part of the immutable generation. There is no
        // separate mutable "previous" pointer to lose in a crash.
        let generation = self
            .root
            .join("generations")
            .join(uuid::Uuid::new_v4().to_string());
        let staging = tempfile::tempdir_in(self.root.join("generations"))?;
        let activation = Activation {
            format: 1,
            current,
            previous,
        };
        let mut file = dirs::private_file(&staging.path().join("activation.json"), true, false)?;
        serde_json::to_writer_pretty(&mut file, &activation)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        symlink(
            self.payload(&activation.current),
            staging.path().join("payload"),
        )?;
        std::fs::File::open(staging.path())?.sync_all()?;
        std::fs::rename(staging.path(), &generation)?;
        std::fs::File::open(self.root.join("generations"))?.sync_all()?;
        // Fresh launchers can be temporarily dangling before initial activation;
        // existing managed launchers continue resolving the previous generation.
        for (path, target) in self.links() {
            std::fs::create_dir_all(path.parent().context("launcher has no parent")?)?;
            match symlink(&target, &path) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
                    path.read_link()? == target,
                    "launcher changed during installation"
                ),
                Err(error) => return Err(error.into()),
            }
        }
        let launcher = if cfg!(target_os = "macos") && activation.current.launcher_redirect == 1 {
            let launcher = self.prepare_launcher_bundle(&activation.current)?;
            self.record_application()?;
            self.repair_legacy_launcher()?;
            Some(launcher)
        } else {
            None
        };
        if !cfg!(target_os = "macos") && !self.application.exists() {
            std::fs::create_dir_all(
                self.application
                    .parent()
                    .context("launcher has no parent")?,
            )?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.application)?;
            file.write_all(self.launcher()?.as_bytes())?;
            file.sync_all()?;
        }
        self.preflight()?;
        let old_generation = self.root.join("current").read_link().ok();
        let pointer = tempfile::tempdir_in(&self.root)?;
        symlink(&generation, pointer.path().join("current"))?;
        std::fs::rename(pointer.path().join("current"), self.root.join("current"))?;
        let published = (|| -> Result<()> {
            std::fs::File::open(&self.root)?.sync_all()?;
            if let Some(launcher) = launcher {
                self.publish_launcher_bundle(launcher)?;
            }
            Ok(())
        })();
        if let Err(error) = published {
            if let Some(old) = old_generation {
                symlink(old, pointer.path().join("restore"))?;
                std::fs::rename(pointer.path().join("restore"), self.root.join("current"))?;
            } else {
                std::fs::remove_file(self.root.join("current"))?;
            }
            std::fs::File::open(&self.root)?.sync_all()?;
            return Err(error.context("launcher publication failed; previous activation restored"));
        }
        Ok(())
    }
}

/// Exchange unlike entries (including an old app symlink) without a missing
/// Applications path. Both entries are on the destination filesystem.
fn replace_application(staged: &Path, destination: &Path, existed: bool) -> Result<()> {
    if !existed {
        std::fs::rename(staged, destination)?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(staged.as_os_str().as_bytes())?;
        let target = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
        // SAFETY: both C strings remain valid during the syscall; RENAME_SWAP
        // atomically exchanges the entries without following their symlinks.
        if unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_SWAP) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    bail!("application bundle exchange is only supported on macOS")
}

fn validate_release(release: &Release) -> Result<()> {
    ensure!(
        release.launcher_redirect <= 1,
        "unknown launcher redirect contract"
    );
    ensure!(
        release.id.len() == 64
            && release.id.bytes().all(|byte| byte.is_ascii_hexdigit())
            && release.id == release.tree_sha256,
        "invalid release identifier"
    );
    ensure!(
        release.payload
            == if cfg!(target_os = "macos") {
                "AgentDocker.app"
            } else {
                "agentdocker-desktop"
            },
        "unexpected payload path"
    );
    ensure!(release.state_schema > 0, "missing state schema contract");
    ensure!(
        release.source_commit.len() == 40
            && release
                .source_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "invalid source commit"
    );
    Ok(())
}

fn file_hash(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn tree_hash(root: &Path) -> Result<String> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut BTreeMap<PathBuf, String>,
        bytes: &mut u64,
    ) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = path.symlink_metadata()?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "native payload contains an unexpected symlink: {}",
                path.display()
            );
            if metadata.is_dir() {
                visit(root, &path, files, bytes)?;
            } else {
                ensure!(
                    metadata.is_file(),
                    "native payload contains a non-regular file"
                );
                *bytes = bytes
                    .checked_add(metadata.len())
                    .context("payload size overflow")?;
                ensure!(
                    *bytes <= MAX_BYTES && files.len() < 10000,
                    "native payload exceeds its installation bounds"
                );
                files.insert(
                    path.strip_prefix(root)?.to_owned(),
                    format!("{:o}:{}", executable_bits(&metadata), file_hash(&path)?),
                );
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files, &mut 0)?;
    let mut hash = Sha256::new();
    for (path, digest) in files {
        hash.update(path.as_os_str().as_encoded_bytes());
        hash.update([0]);
        hash.update(digest.as_bytes());
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn sync_tree(root: &Path) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        let metadata = path.symlink_metadata()?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "staged payload changed to contain a symlink"
        );
        if metadata.is_dir() {
            sync_tree(&path)?;
        } else {
            ensure!(metadata.is_file(), "staged payload contains a special file");
            std::fs::File::open(path)?.sync_all()?;
        }
    }
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

fn checked_command(argv: &[String], timeout: Duration) -> Result<()> {
    let output = command::run(Path::new("/"), argv, timeout)?;
    ensure!(
        output.success,
        "native verification failed: {}",
        output.text.trim()
    );
    Ok(())
}

fn inspect(source: &Path, local_preview: bool) -> Result<(PathBuf, Release)> {
    let source = source
        .canonicalize()
        .context("desktop source does not exist")?;
    let payload_name = if cfg!(target_os = "macos") {
        "AgentDocker.app"
    } else {
        "agentdocker-desktop"
    };
    let payload = if source.join(payload_name).is_dir() {
        source.join(payload_name)
    } else {
        source
    };
    ensure!(
        payload.symlink_metadata()?.is_dir(),
        "payload must be a regular directory"
    );
    // Reject links and special files before opening package metadata or invoking
    // the platform verifier. The same complete hash is checked after copying.
    let tree_sha256 = tree_hash(&payload)?;
    let metadata = if cfg!(target_os = "macos") {
        payload.join("Contents/Resources/build.json")
    } else {
        payload.join("build.json")
    };
    let file = std::fs::File::open(&metadata)
        .context("missing native build metadata; use a desktop artifact")?;
    ensure!(
        file.metadata()?.len() < 1024 * 1024,
        "oversized native build metadata"
    );
    let value: serde_json::Value = serde_json::from_reader(file.take(1024 * 1024))?;
    ensure!(
        value["format"] == 1 && value["product"] == "agentdocker",
        "unknown desktop artifact format"
    );
    let target = value["target"].as_str().context("missing desktop target")?;
    let compatible = if cfg!(target_os = "macos") {
        target == "universal-apple-darwin"
            || target
                == if cfg!(target_arch = "aarch64") {
                    "aarch64-apple-darwin"
                } else {
                    "x86_64-apple-darwin"
                }
    } else {
        target
            == if cfg!(target_arch = "aarch64") {
                "aarch64-unknown-linux-gnu"
            } else {
                "x86_64-unknown-linux-gnu"
            }
    };
    ensure!(
        compatible,
        "artifact target {target} is incompatible with this desktop"
    );
    let binaries = payload.join(if cfg!(target_os = "macos") {
        "Contents/MacOS"
    } else {
        "bin"
    });
    for name in BINARIES {
        let path = binaries.join(name);
        let metadata = path.symlink_metadata()?;
        ensure!(
            metadata.is_file() && executable_bits(&metadata) != 0,
            "{name} is not a regular executable"
        );
        if !cfg!(target_os = "macos") {
            let expected = value["binary_sha256"][name].as_str().context(
                "Linux payload lacks binary checksums; rebuild with current packaging tools",
            )?;
            ensure!(
                file_hash(&path)? == expected,
                "{name} does not match its package checksum"
            );
        }
    }
    if cfg!(target_os = "macos") {
        checked_command(
            &[
                "/usr/bin/codesign".into(),
                "--verify".into(),
                "--deep".into(),
                "--strict".into(),
                payload.to_string_lossy().into_owned(),
            ],
            Duration::from_secs(60),
        )?;
        if !local_preview {
            checked_command(
                &[
                    "/usr/sbin/spctl".into(),
                    "--assess".into(),
                    "--type".into(),
                    "execute".into(),
                    payload.to_string_lossy().into_owned(),
                ],
                Duration::from_secs(60),
            )?;
        }
    }
    let release = Release {
        id: tree_sha256.clone(),
        tree_sha256,
        version: value["version"]
            .as_str()
            .context("missing desktop version")?
            .to_owned(),
        source_commit: value["source_commit"]
            .as_str()
            .context("missing source commit")?
            .to_owned(),
        state_schema: value["state_schema"]
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .context("missing state schema")?,
        target: target.to_owned(),
        installation_lock: value["installation_lock"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        launcher_redirect: match &value["launcher_redirect"] {
            serde_json::Value::Null => 0,
            value => value
                .as_u64()
                .filter(|n| *n <= 1)
                .context("unknown launcher redirect contract")? as u32,
        },
        payload: payload_name.to_owned(),
    };
    validate_release(&release)?;
    Ok((payload, release))
}

fn copy_payload(source: &Path, destination: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        // ditto preserves bundle resource metadata and notarization tickets.
        checked_command(
            &[
                "/usr/bin/ditto".into(),
                source.to_string_lossy().into_owned(),
                destination.to_string_lossy().into_owned(),
            ],
            Duration::from_secs(300),
        )
    } else {
        fn copy(source: &Path, destination: &Path) -> Result<()> {
            std::fs::create_dir(destination)?;
            for entry in std::fs::read_dir(source)? {
                let path = entry?.path();
                let target =
                    destination.join(path.file_name().context("payload entry has no name")?);
                let metadata = path.symlink_metadata()?;
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "payload changed to contain a symlink"
                );
                if metadata.is_dir() {
                    copy(&path, &target)?;
                } else {
                    ensure!(metadata.is_file(), "payload contains a special file");
                    std::fs::copy(path, target)?;
                }
            }
            Ok(())
        }
        copy(source, destination)
    }
}

/// `socket` is the top-level `--socket`, when given: the daemon a status
/// asks and an activation reloads is the one selected, not the default.
/// The executable bits a file carries: its Unix mode's, and on Windows,
/// where there are none, `1` for a regular file so a payload listing
/// keeps one shape on both platforms.
fn executable_bits(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111
    }
    #[cfg(windows)]
    {
        u32::from(metadata.is_file())
    }
}

pub fn run(args: DesktopArgs, socket: Option<PathBuf>) -> Result<()> {
    // Per-user installation, retained versions, launchers and rollback
    // are written for macOS and Linux; Windows gets them in a later slice.
    #[cfg(windows)]
    {
        let _ = (&args, &socket);
        bail!(
            "the desktop installer is not available on Windows yet; run the daemon and CLI from the archive"
        );
    }
    #[cfg(unix)]
    run_unix(args, socket)
}

#[cfg(unix)]
fn run_unix(args: DesktopArgs, socket: Option<PathBuf>) -> Result<()> {
    let prefix = args
        .prefix
        .or_else(std::env::home_dir)
        .context("cannot locate home; provide --prefix")?;
    let layout = Layout::new(prefix)?;
    let active = layout.active()?;
    let (source, candidate, preview, local_preview, expect_release, expect_current) = match args
        .command
    {
        DesktopCommand::Uninstall {
            preview,
            expect_plan,
        } => {
            return maintenance::run(&layout, None, preview, expect_plan.as_deref());
        }
        DesktopCommand::Prune {
            keep,
            preview,
            expect_plan,
        } => {
            return maintenance::run(&layout, Some(keep), preview, expect_plan.as_deref());
        }
        DesktopCommand::Status => {
            let homebrew = homebrew_owner(active.as_ref(), &homebrew_caskrooms());
            let daemon = serving_daemon(socket);
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"prefix":layout.prefix,"application":layout.application,"installation":active,"homebrew":homebrew,"daemon":daemon})
                )?
            );
            return Ok(());
        }
        DesktopCommand::Update {
            feed,
            check,
            apply,
            local_preview,
            socket,
        } => {
            refuse_homebrew_copy(active.as_ref())?;
            return update::run(
                &layout,
                active.as_ref(),
                update::Options {
                    feed,
                    check,
                    apply,
                    local_preview,
                    socket,
                },
            );
        }
        DesktopCommand::Install {
            from,
            preview,
            local_preview,
            expect_release,
            expect_current,
        } => {
            refuse_homebrew_copy(active.as_ref())?;
            let (source, candidate) = inspect(&from, local_preview)?;
            (
                source,
                candidate,
                preview,
                local_preview,
                expect_release,
                expect_current,
            )
        }
        DesktopCommand::Rollback {
            preview,
            local_preview,
            expect_release,
            expect_current,
        } => {
            let active = active.as_ref().context("no active desktop installation")?;
            let previous = active
                .previous
                .as_ref()
                .context("no previous desktop version")?;
            ensure!(
                previous.state_schema == active.current.state_schema,
                "state schema differs; binary rollback cannot roll back the database"
            );
            let (source, candidate) = inspect(&layout.payload(previous), local_preview)?;
            ensure!(
                candidate.id == previous.id,
                "retained rollback version was modified"
            );
            (
                source,
                candidate,
                preview,
                local_preview,
                expect_release,
                expect_current,
            )
        }
    };
    let report = perform(
        &layout,
        active,
        source,
        candidate,
        preview,
        local_preview,
        expect_release,
        expect_current,
        socket,
    )?;
    if !preview {
        layout.register_launcher();
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn required_state_schema(active: Option<&Activation>, home: &Path) -> Result<u32> {
    Ok(agentd::STATE_SCHEMA_VERSION
        .max(active.map_or(0, |active| active.current.state_schema))
        .max(agentd::stored_state_schema(home)?.unwrap_or(0)))
}

/// Check, preview and, unless `preview`, install one inspected candidate:
/// the part of an install that does not care where the payload came from.
/// Returns the report the command prints.
#[allow(clippy::too_many_arguments)]
fn perform(
    layout: &Layout,
    active: Option<Activation>,
    source: PathBuf,
    candidate: Release,
    preview: bool,
    local_preview: bool,
    expect_release: Option<String>,
    expect_current: Option<String>,
    socket: Option<PathBuf>,
) -> Result<serde_json::Value> {
    if let Some(expected) = expect_release {
        ensure!(
            expected == candidate.id,
            "artifact changed since preview; review again"
        );
    }
    if let Some(expected) = expect_current {
        ensure!(
            expected
                == active
                    .as_ref()
                    .map_or("none", |active| active.current.id.as_str()),
            "active installation changed since preview; review again"
        );
    }
    ensure!(
        candidate.state_schema >= required_state_schema(active.as_ref(), &dirs::home())?,
        "artifact uses an older state schema; binary replacement cannot roll back the database"
    );
    layout.preflight()?;
    layout.check_launcher_support(&candidate)?;
    let mut report = json!({"source":source, "candidate":candidate, "previous":active.as_ref().map(|active| &active.current),
        "application":layout.application, "bin":layout.bin, "versions":layout.root.join("versions"),
        "preview":preview, "local_preview":local_preview,
        "activation":"next app/CLI launch; a running daemon is asked to reload to this release once it is activated"});
    if preview {
        return Ok(report);
    }
    layout.ensure_root()?;
    let lock = layout.root.join("install.lock");
    dirs::private_file(&lock, true, false)?;
    let _held = agentdocker_host::lock::try_exclusive(&lock)?
        .context("another desktop installation is in progress")?;
    let current = layout.active()?;
    ensure!(
        candidate.state_schema >= required_state_schema(current.as_ref(), &dirs::home())?,
        "stored state changed since preview; review the installation again"
    );
    ensure!(
        current == active,
        "active installation changed; review again"
    );
    let version = layout.root.join("versions").join(&candidate.id);
    if version.exists() {
        let (_, retained) = inspect(&layout.payload(&candidate), local_preview)?;
        ensure!(retained.id == candidate.id, "retained release was modified");
    } else {
        let stage = tempfile::tempdir_in(layout.root.join("versions"))?;
        let staged = stage.path().join(&candidate.payload);
        copy_payload(&source, &staged)?;
        let (_, copied) = inspect(&staged, local_preview)?;
        ensure!(
            copied.id == candidate.id,
            "artifact changed while copying; current installation preserved"
        );
        sync_tree(stage.path())?;
        std::fs::rename(stage.path(), &version)?;
        std::fs::File::open(layout.root.join("versions"))?.sync_all()?;
    }
    let changed = current
        .as_ref()
        .is_none_or(|active| active.current.id != candidate.id);
    if changed {
        layout.activate(candidate, current.map(|active| active.current))?;
    } else if cfg!(target_os = "macos") && candidate.launcher_redirect == 1 {
        // Reinstalling the active release also repairs an older managed
        // launcher, without changing the retained rollback generation.
        let launcher = layout.prepare_launcher_bundle(&candidate)?;
        layout.record_application()?;
        layout.repair_legacy_launcher()?;
        layout.publish_launcher_bundle(launcher)?;
    }
    // Activated: a daemon that is running was started from an earlier
    // release and keeps serving it until it hands over. Ask it to, and say
    // what serves either way; the answer is the daemon's, never assumed.
    // An unchanged activation asks nothing: there is no other release to
    // hand over to, and a reload would only replace the daemon with the
    // same binary.
    let daemon = if changed {
        daemon_after_activation(socket)
    } else {
        let serving = serving_daemon(socket);
        let summary = if serving.is_null() {
            "unchanged: this release was already active and no daemon answered".to_owned()
        } else {
            format!(
                "unchanged: this release was already active; agentd {} serves from {} (pid {})",
                serving["version"].as_str().unwrap_or("?"),
                serving["executable"].as_str().unwrap_or("?"),
                serving["pid"]
            )
        };
        json!({"answered": !serving.is_null(), "reloaded": false, "serving": serving, "summary": summary})
    };
    report["activation"] = json!(daemon["summary"]);
    report["daemon"] = daemon;
    if changed {
        report["retention"] = maintenance::after_activation(layout, &_held);
    }
    Ok(report)
}

/// What actually serves right now, from the daemon itself: its version,
/// pid and executable, or `null` when no daemon answers. Nothing is
/// started.
fn serving_daemon(socket: Option<PathBuf>) -> serde_json::Value {
    use agentdocker_core::{Request, Response};
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return serde_json::Value::Null;
        };
        runtime.block_on(async {
            let client = crate::client::Client::new(socket).with_start_timeout(None);
            match client.call(&Request::Ping).await {
                Ok(Response::Pong {
                    version,
                    pid,
                    executable,
                    ..
                }) => json!({"version": version, "pid": pid, "executable": executable}),
                _ => serde_json::Value::Null,
            }
        })
    })
    .join()
    .unwrap_or(serde_json::Value::Null)
}

/// Ask a running daemon to reload to the release just activated, and
/// report what serves afterwards. Nothing is started here: with no daemon
/// answering, the next launch starts the installed release. A refusal is
/// reported with the daemon's reason, since a daemon that keeps serving
/// the previous release is the safe outcome, not a failure of the
/// installation.
fn daemon_after_activation(socket: Option<PathBuf>) -> serde_json::Value {
    use agentdocker_core::{Request, Response};
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return json!({"answered": false, "summary": "next app/CLI launch; no daemon was asked"});
        };
        runtime.block_on(async {
            let client = crate::client::Client::new(socket).with_start_timeout(None);
            let serving = |response: Result<Response>| match response {
                Ok(Response::Pong {
                    version,
                    pid,
                    executable,
                    ..
                }) => Some(json!({"version": version, "pid": pid, "executable": executable})),
                _ => None,
            };
            let Some(before) = serving(client.call(&Request::Ping).await) else {
                return json!({
                    "answered": false,
                    "summary": "next app/CLI launch; no daemon answered, so none needed reloading",
                });
            };
            match client.call_raw(&Request::Reload).await {
                Ok(Response::Ok) => {
                    let after = serving(client.call(&Request::Ping).await);
                    let summary = match &after {
                        Some(now) => format!(
                            "reloaded: agentd {} serves from {} (pid {})",
                            now["version"].as_str().unwrap_or("?"),
                            now["executable"].as_str().unwrap_or("?"),
                            now["pid"]
                        ),
                        None => "reloaded, but no daemon answered afterwards; check `agentdocker daemon status`".to_owned(),
                    };
                    json!({"answered": true, "reloaded": true, "before": before, "serving": after, "summary": summary})
                }
                Ok(Response::Error { code, message, .. }) => json!({
                    "answered": true, "reloaded": false, "serving": before,
                    "refusal": {"code": code, "message": message},
                    "summary": format!(
                        "not reloaded ({message}); agentd {} keeps serving from {} until `agentdocker daemon reload` succeeds or it is restarted",
                        before["version"].as_str().unwrap_or("?"),
                        before["executable"].as_str().unwrap_or("?")
                    ),
                }),
                outcome => {
                    // A lost or unexpected reply does not prove refusal. The
                    // successor may already serve; never replay Reload or
                    // report the predecessor as though it were still observed.
                    let reason = match outcome {
                        Ok(other) => format!("reload answered {other:?}"),
                        Err(error) => format!("reload reply could not be confirmed ({error:#})"),
                    };
                    let after = serving(client.call(&Request::Ping).await);
                    let observed = match &after {
                        Some(now) => format!(
                            "agentd {} is now observed at {} (pid {})",
                            now["version"].as_str().unwrap_or("?"),
                            now["executable"].as_str().unwrap_or("?"),
                            now["pid"]
                        ),
                        None => "the serving daemon is unknown".to_owned(),
                    };
                    json!({
                        "answered": true, "reloaded": serde_json::Value::Null,
                        "before": before, "serving": after,
                        "summary": format!("{reason}; {observed}; check `agentdocker daemon status`"),
                    })
                }
            }
        })
    })
    .join()
    .unwrap_or_else(|_| json!({"answered": false, "summary": "next app/CLI launch; the daemon could not be asked"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn activation_rechecks_serving_daemon_after_a_lost_reload_reply_without_replay() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::time::{Duration, Instant};

        for observed_pid in [Some(202), Some(101), None] {
            let tmp = tempfile::tempdir().unwrap();
            let socket = tmp.path().join("reload.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                for (operation, pid) in [
                    ("ping", Some(101)),
                    ("reload", None),
                    ("ping", observed_pid),
                ] {
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(Instant::now() < deadline, "missing {operation} request");
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("accept: {error}"),
                        }
                    };
                    // macOS can inherit O_NONBLOCK from the listening
                    // socket. A read timeout does not make such a stream
                    // blocking; an early read otherwise races the client.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut line = String::new();
                    BufReader::new(&mut stream).read_line(&mut line).unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(request["op"], operation, "Reload must never be replayed");
                    if let Some(pid) = pid {
                        let response = json!({
                            "type": "pong", "version": "fixture", "uptime_secs": 1,
                            "pid": pid, "executable": format!("/release-{pid}/agentd")
                        });
                        writeln!(stream, "{response}").unwrap();
                    }
                    // Reload is read, then the connection closes without a
                    // reply; the last Ping may likewise have no answer.
                }
            });
            let report = daemon_after_activation(Some(socket));
            server.join().unwrap();
            assert_eq!(report["before"]["pid"], 101);
            assert!(report["reloaded"].is_null(), "{report}");
            match observed_pid {
                Some(pid) => assert_eq!(report["serving"]["pid"], pid),
                None => assert!(report["serving"].is_null(), "{report}"),
            }
        }
    }

    pub(super) fn release(layout: &Layout, marker: &str) -> Release {
        let id = format!("{:x}", Sha256::digest(marker.as_bytes()));
        let release = Release {
            id: id.clone(),
            version: "0.1.0".into(),
            source_commit: "a".repeat(40),
            state_schema: agentd::STATE_SCHEMA_VERSION,
            target: "fixture".into(),
            tree_sha256: id,
            installation_lock: agentdocker_host::installation::LOCK_FORMAT,
            launcher_redirect: agentdocker_host::installation::LAUNCHER_REDIRECT_FORMAT,
            payload: if cfg!(target_os = "macos") {
                "AgentDocker.app"
            } else {
                "agentdocker-desktop"
            }
            .into(),
        };
        let payload = layout.payload(&release);
        std::fs::create_dir_all(payload.join(layout.binary_subdir())).unwrap();
        for name in BINARIES {
            std::fs::write(payload.join(layout.binary_subdir()).join(name), marker).unwrap();
        }
        if cfg!(target_os = "macos") {
            std::fs::create_dir_all(payload.join("Contents/Resources")).unwrap();
            std::fs::write(
                payload.join("Contents/Resources/build.json"),
                json!({
                    "format": 1, "product": "agentdocker", "launcher_redirect": 1,
                })
                .to_string(),
            )
            .unwrap();
            std::fs::write(
                payload.join("Contents/Info.plist"),
                concat!(
                    "<plist><dict><key>CFBundleName</key><string>AgentDocker</string>",
                    "<key>CFBundleExecutable</key><string>agentdocker-ui</string>",
                    "<key>CFBundleIdentifier</key><string>dev.agentdocker.desktop</string>",
                    "<key>CFBundleVersion</key><string>0.1.0</string></dict></plist>"
                ),
            )
            .unwrap();
            std::fs::write(payload.join("Contents/Resources/AgentDocker.icns"), marker).unwrap();
        }
        rehash(layout, release)
    }

    fn rehash(layout: &Layout, mut release: Release) -> Release {
        let old = layout.root.join("versions").join(&release.id);
        release.id = tree_hash(&layout.payload(&release)).unwrap();
        release.tree_sha256 = release.id.clone();
        let new = layout.root.join("versions").join(&release.id);
        if old != new {
            std::fs::rename(old, new).unwrap();
        }
        release
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_application_is_a_real_bundle_named_agentdocker_that_runs_the_active_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        // Earlier releases installed a symlink here; it is ours to replace.
        std::fs::create_dir_all(layout.application.parent().unwrap()).unwrap();
        symlink(layout.root.join("current/payload"), &layout.application).unwrap();
        layout.activate(first.clone(), None).unwrap();

        let metadata = layout.application.symlink_metadata().unwrap();
        assert!(metadata.is_dir(), "a bundle Launchpad can see, not a link");
        let plist =
            std::fs::read_to_string(layout.application.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains("<key>CFBundleName</key><string>AgentDocker</string>"));
        assert!(plist.contains("<key>CFBundleExecutable</key><string>agentdocker-ui</string>"));
        assert!(plist.contains("<key>CFBundleVersion</key><string>0.1.0</string>"));
        assert_eq!(tree_hash(&layout.application).unwrap(), first.id);
        for name in BINARIES {
            assert!(
                layout
                    .application
                    .join("Contents/MacOS")
                    .join(name)
                    .symlink_metadata()
                    .unwrap()
                    .is_file(),
                "signed executables stay intact"
            );
        }
        assert!(layout.application_is_replaceable().unwrap());

        // A second activation replaces the bundle in place and keeps it ours.
        let second = release(&layout, "second");
        layout.activate(second, Some(first)).unwrap();
        assert!(layout.application.join("Contents/Info.plist").is_file());
        assert!(layout.application_is_replaceable().unwrap());
        assert_eq!(
            std::fs::read_dir(layout.application.parent().unwrap())
                .unwrap()
                .count(),
            1,
            "no staging or retired bundles are left beside it"
        );

        // Somebody else's app at that path is never replaced.
        std::fs::remove_dir_all(&layout.application).unwrap();
        std::fs::create_dir_all(layout.application.join("Contents")).unwrap();
        let error = layout.preflight().unwrap_err().to_string();
        assert!(
            error.contains("already exists outside this installation"),
            "{error}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn preparing_a_launcher_keeps_the_active_icon_and_version_until_pointer_switch() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        let mut second = release(&layout, "second");
        second.version = "9.9.9".into();
        for (release, icon) in [(&first, "first icon"), (&second, "second icon")] {
            let resources = layout.payload(release).join("Contents/Resources");
            std::fs::create_dir_all(&resources).unwrap();
            std::fs::write(resources.join("AgentDocker.icns"), icon).unwrap();
        }
        let first = rehash(&layout, first);
        std::fs::write(
            layout.payload(&second).join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleVersion</key><string>9.9.9</string></dict></plist>",
        )
        .unwrap();
        let second = rehash(&layout, second);
        layout.activate(first.clone(), None).unwrap();
        let contents = layout.application.join("Contents");
        let plist = std::fs::read(contents.join("Info.plist")).unwrap();

        // A prepared candidate remains private until the pointer switch.
        let prepared = layout.prepare_launcher_bundle(&second).unwrap();
        assert_eq!(
            tree_hash(&prepared.path().join("AgentDocker.app")).unwrap(),
            second.id
        );
        assert_eq!(layout.active().unwrap().unwrap().current, first);
        assert_eq!(std::fs::read(contents.join("Info.plist")).unwrap(), plist);
        assert_eq!(
            std::fs::read_to_string(contents.join("Resources/AgentDocker.icns")).unwrap(),
            "first icon"
        );
        assert_eq!(
            std::fs::read_to_string(contents.join("MacOS/agentdocker")).unwrap(),
            "first"
        );

        layout.activate(second, Some(first)).unwrap();
        assert_ne!(std::fs::read(contents.join("Info.plist")).unwrap(), plist);
        assert_eq!(
            std::fs::read_to_string(contents.join("Resources/AgentDocker.icns")).unwrap(),
            "second icon"
        );
        assert_eq!(
            std::fs::read_to_string(contents.join("MacOS/agentdocker")).unwrap(),
            "second"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn migration_preserves_signed_files_and_failed_publication_preserves_the_old_app() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        let second = release(&layout, "second");
        let resources = layout.application.join("Contents/Resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(
            resources.join("managed-launcher.json"),
            json!({
                "format": 1, "product": "agentdocker", "root": layout.root,
            })
            .to_string(),
        )
        .unwrap();
        layout.activate(first.clone(), None).unwrap();
        assert_eq!(tree_hash(&layout.application).unwrap(), first.id);
        assert!(!resources.join("managed-launcher.json").exists());
        let staging = layout.prepare_launcher_bundle(&second).unwrap();
        std::fs::remove_dir_all(staging.path().join("AgentDocker.app")).unwrap();
        assert!(layout.publish_launcher_bundle(staging).is_err());
        assert_eq!(tree_hash(&layout.application).unwrap(), first.id);
        assert_eq!(layout.active().unwrap().unwrap().current, first);
        std::fs::write(layout.application.join("Contents/Info.plist"), "user edit").unwrap();
        assert!(!layout.application_is_replaceable().unwrap());
        assert!(layout.activate(second, None).is_err());
        assert_eq!(
            std::fs::read_to_string(layout.application.join("Contents/Info.plist")).unwrap(),
            "user edit"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn older_payloads_keep_a_compatible_launcher_and_cannot_bootstrap_one() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let modern = release(&layout, "modern");
        let mut legacy = release(&layout, "legacy");
        legacy.launcher_redirect = 0;
        std::fs::write(
            layout
                .payload(&legacy)
                .join("Contents/Resources/build.json"),
            json!({
                "format": 1, "product": "agentdocker", "installation_lock": 1,
            })
            .to_string(),
        )
        .unwrap();
        let legacy = rehash(&layout, legacy);
        assert!(layout.activate(legacy.clone(), None).is_err());
        assert!(layout.active().unwrap().is_none());
        layout.activate(modern.clone(), None).unwrap();
        layout
            .activate(legacy.clone(), Some(modern.clone()))
            .unwrap();
        assert_eq!(layout.active().unwrap().unwrap().current, legacy);
        assert_eq!(tree_hash(&layout.application).unwrap(), modern.id);
        assert_eq!(layout.copied_launcher_version().unwrap(), Some(modern.id));
        assert_eq!(
            std::fs::read_to_string(layout.bin.join("agentdocker")).unwrap(),
            "legacy"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn legacy_bundle_commands_and_gui_keep_their_roles_across_activation_and_rollback() {
        let tmp = tempfile::Builder::new()
            .prefix("ad launcher ")
            .tempdir()
            .unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        let second = release(&layout, "second");
        for (release, generation) in [(&first, "first"), (&second, "second")] {
            for name in BINARIES {
                let path = layout
                    .payload(release)
                    .join(layout.binary_subdir())
                    .join(name);
                std::fs::write(
                    &path,
                    format!("#!/bin/sh\nprintf '%s\\n' '{generation}:{name}' \"$@\"\n"),
                )
                .unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let first = rehash(&layout, first);
        let second = rehash(&layout, second);
        // Simulate the old installation; its absolute provider paths must
        // still execute the correct binary after replacing the app symlink.
        std::fs::create_dir_all(layout.application.parent().unwrap()).unwrap();
        symlink(layout.root.join("current/payload"), &layout.application).unwrap();
        for (current, previous, generation) in [
            (first.clone(), None, "first"),
            (second.clone(), Some(first.clone()), "second"),
            (first, Some(second), "first"),
        ] {
            layout.activate(current, previous).unwrap();
            for (entry, binary, args) in [
                ("agentdocker", "agentdocker", vec!["hook", "claude-code"]),
                ("agentdocker", "agentdocker", vec!["hook", "codex"]),
                (
                    "agentdocker",
                    "agentdocker",
                    vec!["mcp", "--runtime", "claude-code"],
                ),
                ("agentd", "agentd", vec!["--help"]),
                ("agentdocker-ui", "agentdocker-ui", vec!["--help"]),
                ("agentdocker-ui", "agentdocker-ui", vec![]),
                (
                    "agentdocker-ui",
                    "agentdocker-ui",
                    vec!["--open-url", "agentdocker://inbox?draft=a b"],
                ),
            ] {
                let output = std::process::Command::new(
                    layout.application.join("Contents/MacOS").join(entry),
                )
                .args(&args)
                .output()
                .unwrap();
                assert!(output.status.success(), "{entry}: {output:?}");
                let expected = std::iter::once(format!("{generation}:{binary}"))
                    .chain(args.into_iter().map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n";
                assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_applications_folder_is_chosen_only_for_the_home_prefix_when_free_or_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let home = project::try_canonical(tmp.path()).unwrap();
        let system = home.join("Applications");
        std::fs::create_dir(&system).unwrap();
        // Home prefix, writable folder, nothing there: use it.
        assert_eq!(
            Layout::default_system_application(&home, Some(&home), &system),
            Some(system.join("AgentDocker.app"))
        );
        // A trial prefix never leaves its own tree.
        let trial = tmp.path().join("trial");
        std::fs::create_dir(&trial).unwrap();
        assert_eq!(
            Layout::default_system_application(
                &project::try_canonical(&trial).unwrap(),
                Some(&home),
                &system
            ),
            None
        );
        // Somebody else's AgentDocker.app: leave it alone.
        std::fs::create_dir_all(system.join("AgentDocker.app/Contents")).unwrap();
        assert_eq!(
            Layout::default_system_application(&home, Some(&home), &system),
            None
        );
        // Our own marked launcher there: keep using it.
        let resources = system.join("AgentDocker.app/Contents/Resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(
            resources.join("managed-launcher.json"),
            serde_json::json!({
                "format": 1,
                "product": "agentdocker",
                "root": home.join(".local/share/agentdocker/desktop").to_str().unwrap(),
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            Layout::default_system_application(&home, Some(&home), &system),
            Some(system.join("AgentDocker.app"))
        );
        // A folder we cannot write to: stay under the prefix.
        std::fs::remove_dir_all(system.join("AgentDocker.app")).unwrap();
        std::fs::set_permissions(&system, std::fs::Permissions::from_mode(0o555)).unwrap();
        let chosen = Layout::default_system_application(&home, Some(&home), &system);
        std::fs::set_permissions(&system, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(chosen, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_launcher_record_only_names_the_two_managed_places() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = project::try_canonical(tmp.path()).unwrap();
        let root = prefix.join(".local/share/agentdocker/desktop");
        std::fs::create_dir_all(&root).unwrap();
        let user = prefix.join("Applications/AgentDocker.app");
        let record = |value: &str| std::fs::write(root.join("launcher.json"), value).unwrap();
        record(
            &serde_json::json!({"format": 1, "application": user.to_str().unwrap()}).to_string(),
        );
        assert_eq!(
            Layout::recorded_or_default_application(&root, &prefix, &user).unwrap(),
            None
        );
        record(
            &serde_json::json!({"format": 1, "application": "/Applications/AgentDocker.app"})
                .to_string(),
        );
        assert!(
            Layout::recorded_or_default_application(&root, &prefix, &user).is_err(),
            "a recorded path cannot let a trial prefix write outside its tree"
        );
        for bad in [
            serde_json::json!({"format": 1, "application": "/tmp/elsewhere/AgentDocker.app"})
                .to_string(),
            serde_json::json!({"format": 2, "application": "/Applications/AgentDocker.app"})
                .to_string(),
            "not json".to_owned(),
        ] {
            record(&bad);
            assert!(
                Layout::recorded_or_default_application(&root, &prefix, &user).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn homebrew_owns_the_app_when_its_record_exists_and_nothing_is_activated() {
        let tmp = tempfile::tempdir().unwrap();
        let room = tmp.path().join("Caskroom/agentdocker-app");
        let rooms = std::slice::from_ref(&room);
        // No cask record: not Homebrew's, wherever an app may sit.
        assert_eq!(homebrew_owner(None, rooms), None);
        std::fs::create_dir_all(&room).unwrap();
        // A cask record with no activation is Homebrew's, whatever
        // --appdir put the app; a managed activation wins.
        assert_eq!(homebrew_owner(None, rooms), Some(room.clone()));
        let active: Activation = serde_json::from_value(serde_json::json!({
            "format": 1,
            "current": {"id": "a".repeat(64), "version": "0.1.0", "source_commit": "b".repeat(40),
                         "state_schema": 16, "target": "aarch64-apple-darwin", "payload": "AgentDocker.app",
                         "tree_sha256": "a".repeat(64), "installation_lock": 1},
            "previous": null
        }))
        .unwrap();
        assert_eq!(homebrew_owner(Some(&active), rooms), None);
    }

    #[test]
    fn status_and_preflight_do_not_create_an_installation() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path().join("absent");
        let layout = Layout::new(prefix.clone()).unwrap();
        assert!(layout.active().unwrap().is_none());
        layout.preflight().unwrap();
        assert!(!prefix.exists());
    }

    #[test]
    fn activation_switches_every_managed_command_and_keeps_the_previous_version() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        let second = release(&layout, "second");
        layout.activate(first.clone(), None).unwrap();
        assert_eq!(
            std::fs::read_to_string(layout.bin.join("agentdocker")).unwrap(),
            "first"
        );
        layout
            .activate(second.clone(), Some(first.clone()))
            .unwrap();
        let active = layout.active().unwrap().unwrap();
        assert_eq!(active.current, second);
        assert_eq!(active.previous, Some(first.clone()));
        for name in BINARIES {
            assert_eq!(
                std::fs::read_to_string(layout.bin.join(name)).unwrap(),
                "second"
            );
        }
        layout
            .activate(first.clone(), Some(second.clone()))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(layout.bin.join("agentdocker-ui")).unwrap(),
            "first"
        );
        assert!(
            layout.payload(&second).exists(),
            "rollback never deletes the newer version"
        );
        assert_eq!(
            layout.root.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            layout
                .root
                .join("current/activation.json")
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn desktop_entry_paths_preserve_literal_backslashes_and_percent_without_key_injection() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().join("space \\ folder%$\"`")).unwrap();
        let launcher = layout.launcher().unwrap();
        let exec = launcher
            .lines()
            .find(|line| line.starts_with("Exec="))
            .unwrap();
        let icon = launcher
            .lines()
            .find(|line| line.starts_with("Icon="))
            .unwrap();
        // These are serialized Desktop Entry values, before its two Exec
        // decoding layers. Icon has one layer and does not expand field codes.
        assert!(exec.contains(r#"space \\\\ folder%%\\$\\"\\`"#), "{exec}");
        assert!(icon.contains(r#"space \\ folder%$"`"#), "{icon}");
        assert_eq!(
            launcher
                .lines()
                .filter(|line| line.starts_with("Icon="))
                .count(),
            1
        );
        for suffix in [
            "new\nExec=other",
            "carriage\rreturn",
            "tab\there",
            "equal=sign",
        ] {
            assert!(
                Layout::new(tmp.path().join(suffix))
                    .unwrap()
                    .launcher()
                    .is_err()
            );
        }
    }

    #[test]
    fn existing_commands_and_edited_launchers_are_never_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        let second = release(&layout, "second");
        layout.activate(first.clone(), None).unwrap();
        let cli = layout.bin.join("agentdocker");
        std::fs::remove_file(&cli).unwrap();
        std::fs::write(&cli, "user command").unwrap();
        assert!(layout.activate(second, Some(first.clone())).is_err());
        assert_eq!(std::fs::read_to_string(cli).unwrap(), "user command");
        assert_eq!(layout.active().unwrap().unwrap().current, first);
    }

    #[test]
    fn prepared_orphan_generation_does_not_change_the_active_release() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        layout.activate(first.clone(), None).unwrap();
        // Simulate a process dying after preparing a generation, before its
        // atomic pointer switch. Readers only inspect the pointed generation.
        let orphan = layout.root.join("generations/incomplete");
        std::fs::create_dir(&orphan).unwrap();
        std::fs::write(orphan.join("activation.json"), "partial json").unwrap();
        assert_eq!(layout.active().unwrap().unwrap().current, first);
    }

    #[test]
    fn escaping_pointers_and_prefix_aliases_fail_before_installation_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().join("prefix")).unwrap();
        layout.ensure_root().unwrap();
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, layout.root.join("current")).unwrap();
        assert!(layout.active().is_err());
        std::fs::remove_file(layout.root.join("current")).unwrap();
        std::fs::create_dir_all(layout.bin.parent().unwrap()).unwrap();
        symlink(&outside, &layout.bin).unwrap();
        assert!(layout.preflight().is_err());
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn payload_hash_detects_content_and_execute_permission_changes_and_rejects_links() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("worker");
        std::fs::write(&file, "first").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let first = tree_hash(tmp.path()).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(tree_hash(tmp.path()).unwrap(), first);
        std::fs::write(&file, "second").unwrap();
        let changed = tree_hash(tmp.path()).unwrap();
        assert_ne!(changed, first);
        symlink(&file, tmp.path().join("alias")).unwrap();
        assert!(tree_hash(tmp.path()).is_err());
    }

    #[test]
    fn setup_uses_a_stable_link_and_refuses_an_obsolete_running_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        let second = release(&layout, "second");
        layout.activate(first.clone(), None).unwrap();
        let first_cli = layout
            .payload(&first)
            .join(layout.binary_subdir())
            .join("agentdocker");
        assert_eq!(
            stable_executable(&first_cli).unwrap(),
            layout.bin.join("agentdocker")
        );
        layout
            .activate(second.clone(), Some(first.clone()))
            .unwrap();
        assert!(stable_executable(&first_cli).is_err());
        let second_cli = layout
            .payload(&second)
            .join(layout.binary_subdir())
            .join("agentdocker");
        assert_eq!(
            stable_executable(&second_cli).unwrap(),
            layout.bin.join("agentdocker")
        );
        let development = tmp.path().join("checkout/target/debug/agentdocker");
        assert_eq!(stable_executable(&development).unwrap(), development);
    }

    /// The stress version of the test below, kept out of the suite
    /// because it takes seconds rather than milliseconds. Run it by name
    /// when the ordinary one has failed:
    /// `cargo test -p agentdocker -- --ignored readers_under_load`.
    #[test]
    #[ignore = "seconds, not milliseconds; run it by name after a flake"]
    fn readers_under_load_observe_complete_generations() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        let second = release(&layout, "second");
        layout.activate(first.clone(), None).unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            let readers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut seen = 0u64;
                        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                            let active = layout
                                .active()
                                .unwrap_or_else(|error| panic!("after {seen} reads: {error:#}"))
                                .unwrap();
                            assert!(active.current == first || active.current == second);
                            seen += 1;
                        }
                        seen
                    })
                })
                .collect();
            for _ in 0..300 {
                layout
                    .activate(second.clone(), Some(first.clone()))
                    .unwrap();
                layout
                    .activate(first.clone(), Some(second.clone()))
                    .unwrap();
            }
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            for reader in readers {
                assert!(reader.join().unwrap() > 0);
            }
        });
    }

    #[test]
    fn readers_observe_complete_generations_during_activation() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        let first = release(&layout, "first");
        let second = release(&layout, "second");
        layout.activate(first.clone(), None).unwrap();
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                for _ in 0..100 {
                    let active = layout.active().unwrap().unwrap();
                    if active.current == first {
                        assert!(
                            active.previous.is_none() || active.previous == Some(second.clone())
                        );
                    } else {
                        assert_eq!(active.current, second);
                        assert_eq!(active.previous, Some(first.clone()));
                    }
                }
            });
            for _ in 0..3 {
                layout
                    .activate(second.clone(), Some(first.clone()))
                    .unwrap();
                layout
                    .activate(first.clone(), Some(second.clone()))
                    .unwrap();
            }
            reader.join().unwrap();
        });
    }
}
