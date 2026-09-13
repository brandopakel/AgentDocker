//! Per-user native desktop installation. Immutable version and activation
//! directories make the current release and rollback target one atomic switch.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
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
        /// Feed URL (https://, or file:// with --local-preview). Defaults to the latest GitHub release asset, or AGENTDOCKER_UPDATE_FEED.
        #[arg(long, env = update::FEED_ENV, default_value = update::DEFAULT_FEED)]
        feed: String,
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
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `path` is a valid NUL-terminated C string for the call's
        // duration, and access(2) reads it without retaining it.
        unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
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
    /// is a real launcher bundle written by `write_launcher_bundle`,
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
            .arg("-f")
            .arg(&self.application)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// The marker that says a launcher bundle is ours and which store it
    /// points at, so preflight can tell it from an app somebody else put
    /// at the same path.
    fn launcher_marker(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "format": 1,
            "product": "agentdocker",
            "root": self.root.to_str().context("store path must be UTF-8")?,
        }))? + "\n")
    }

    #[cfg(all(test, target_os = "macos"))]
    fn launcher_is_ours(&self) -> Result<bool> {
        let marker = self
            .application
            .join("Contents/Resources/managed-launcher.json");
        let Ok(text) = std::fs::read_to_string(&marker) else {
            return Ok(false);
        };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        Ok(value["product"] == "agentdocker" && value["root"].as_str() == self.root.to_str())
    }

    /// Write the macOS launcher bundle: a real `AgentDocker.app` directory
    /// with an Info.plist naming the product, the icon of the release
    /// being activated, and stable links to all three active executables.
    /// The GUI and old absolute hook/MCP commands keep distinct entry points.
    /// Built beside its destination and swapped in with renames.
    fn write_launcher_bundle(&self, release: &Release) -> Result<()> {
        let parent = self
            .application
            .parent()
            .context("launcher has no parent")?;
        std::fs::create_dir_all(parent)?;
        let staging = tempfile::Builder::new()
            .prefix(".AgentDocker.app.")
            .tempdir_in(parent)?;
        let contents = staging.path().join("Contents");
        std::fs::create_dir_all(contents.join("MacOS"))?;
        std::fs::create_dir_all(contents.join("Resources"))?;
        let version = release.version.split(['-', '+']).next().unwrap_or("0.0.0");
        let plist = format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
                "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
                "<plist version=\"1.0\">\n<dict>\n",
                "\t<key>CFBundleName</key><string>AgentDocker</string>\n",
                "\t<key>CFBundleDisplayName</key><string>AgentDocker</string>\n",
                "\t<key>CFBundleExecutable</key><string>agentdocker-ui</string>\n",
                "\t<key>CFBundleIdentifier</key><string>dev.agentdocker.launcher</string>\n",
                "\t<key>CFBundleIconFile</key><string>AgentDocker</string>\n",
                "\t<key>CFBundlePackageType</key><string>APPL</string>\n",
                "\t<key>CFBundleShortVersionString</key><string>{version}</string>\n",
                "\t<key>CFBundleVersion</key><string>{version}</string>\n",
                "\t<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>\n",
                "\t<key>LSMinimumSystemVersion</key><string>11.0</string>\n",
                "\t<key>LSApplicationCategoryType</key>",
                "<string>public.app-category.developer-tools</string>\n",
                "\t<key>NSHighResolutionCapable</key><true/>\n",
                "</dict>\n</plist>\n"
            ),
            version = version
        );
        std::fs::write(contents.join("Info.plist"), plist)?;
        std::fs::write(contents.join("PkgInfo"), "APPL????")?;
        let icon = self
            .payload(release)
            .join("Contents/Resources/AgentDocker.icns");
        if icon.is_file() {
            std::fs::copy(&icon, contents.join("Resources/AgentDocker.icns"))?;
        }
        std::fs::write(
            contents.join("Resources/managed-launcher.json"),
            self.launcher_marker()?,
        )?;
        sync_tree(staging.path())?;
        // Keep payload validation strict: only these installer-created links
        // are allowed in the launcher, after syncing its regular files. Sync
        // the directory entries without following a possibly dangling current
        // pointer on first install.
        for name in BINARIES {
            symlink(
                self.root.join("current/payload/Contents/MacOS").join(name),
                contents.join("MacOS").join(name),
            )?;
        }
        std::fs::File::open(contents.join("MacOS"))?.sync_all()?;
        // Whatever is there is ours (preflight said so): a launcher from an
        // earlier activation, or the symlink earlier releases installed.
        let retired = parent.join(format!(".AgentDocker.app.retired-{}", uuid::Uuid::new_v4()));
        match self.application.symlink_metadata() {
            Ok(_) => std::fs::rename(&self.application, &retired)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        std::fs::rename(staging.keep(), &self.application)?;
        std::fs::File::open(parent)?.sync_all()?;
        match retired.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => std::fs::remove_file(&retired)?,
            Ok(_) => std::fs::remove_dir_all(&retired)?,
            Err(_) => (),
        }
        self.record_application()?;
        if let Some(legacy) = &self.legacy_application {
            // Preflight checked this path is ours. Replace whatever earlier
            // release left with a link to the launcher, so old absolute
            // hook and MCP command paths still run the active release.
            std::fs::create_dir_all(legacy.parent().context("launcher has no parent")?)?;
            let retired = legacy.with_extension(format!("app.retired-{}", uuid::Uuid::new_v4()));
            match legacy.symlink_metadata() {
                Ok(metadata)
                    if metadata.file_type().is_symlink()
                        && legacy.read_link()? == self.application => {}
                Ok(_) => {
                    std::fs::rename(legacy, &retired)?;
                    symlink(&self.application, legacy)?;
                    match retired.symlink_metadata() {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            std::fs::remove_file(&retired)?
                        }
                        Ok(_) => std::fs::remove_dir_all(&retired)?,
                        Err(_) => (),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    symlink(&self.application, legacy)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
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
                let Ok(text) = std::fs::read_to_string(&marker) else {
                    return Ok(false);
                };
                let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                Ok(value["product"] == "agentdocker"
                    && value["root"].as_str() == self.root.to_str())
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
        if cfg!(target_os = "macos") {
            self.write_launcher_bundle(&activation.current)?;
        }
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
        let pointer = tempfile::tempdir_in(&self.root)?;
        symlink(&generation, pointer.path().join("current"))?;
        std::fs::rename(pointer.path().join("current"), self.root.join("current"))?;
        std::fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}

fn validate_release(release: &Release) -> Result<()> {
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
                    format!(
                        "{:o}:{}",
                        metadata.permissions().mode() & 0o111,
                        file_hash(&path)?
                    ),
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
            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
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

pub fn run(args: DesktopArgs) -> Result<()> {
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
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"prefix":layout.prefix,"application":layout.application,"installation":active})
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
    let report = json!({"source":source, "candidate":candidate, "previous":active.as_ref().map(|active| &active.current),
        "application":layout.application, "bin":layout.bin, "versions":layout.root.join("versions"),
        "preview":preview, "local_preview":local_preview,
        "activation":"next app/CLI launch; an already-running daemon continues until explicitly restarted or reloaded"});
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
    if current
        .as_ref()
        .is_none_or(|active| active.current.id != candidate.id)
    {
        layout.activate(candidate, current.map(|active| active.current))?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(plist.contains("<key>CFBundleDisplayName</key><string>AgentDocker</string>"));
        assert!(plist.contains("<key>CFBundleExecutable</key><string>agentdocker-ui</string>"));
        assert!(plist.contains("<string>0.1.0</string>"));
        for name in BINARIES {
            assert_eq!(
                layout
                    .application
                    .join("Contents/MacOS")
                    .join(name)
                    .read_link()
                    .unwrap(),
                layout
                    .root
                    .join("current/payload/Contents/MacOS")
                    .join(name),
                "each entry follows activation without case-based dispatch"
            );
        }
        assert!(layout.launcher_is_ours().unwrap());

        // A second activation replaces the bundle in place and keeps it ours.
        let second = release(&layout, "second");
        layout.activate(second, Some(first)).unwrap();
        assert!(layout.application.join("Contents/Info.plist").is_file());
        assert!(layout.launcher_is_ours().unwrap());
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
