//! Per-user native desktop installation. Immutable version and activation
//! directories make the current release and rollback target one atomic switch.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use agentdocker_host::{command, dirs, project};

mod maintenance;

const BINARIES: &[&str] = &["agentdocker", "agentd", "agentdocker-ui"];
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
        #[arg(long)]
        preview: bool,
        #[arg(long)]
        expect_plan: Option<String>,
    },
    /// Show the active and previous retained versions without starting the daemon.
    Status,
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

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Activation {
    format: u32,
    current: Release,
    previous: Option<Release>,
}

struct Layout {
    prefix: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    application: PathBuf,
}

impl Layout {
    fn new(prefix: PathBuf) -> Result<Self> {
        let prefix = project::try_canonical(&prefix)?;
        ensure!(prefix.is_absolute(), "installation prefix must be absolute");
        let application = if cfg!(target_os = "macos") {
            prefix.join("Applications/agentdocker.app")
        } else {
            prefix.join(".local/share/applications/agentdocker.desktop")
        };
        Ok(Self {
            root: prefix.join(".local/share/agentdocker/desktop"),
            bin: prefix.join(".local/bin"),
            prefix,
            application,
        })
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

    fn links(&self) -> Vec<(PathBuf, PathBuf)> {
        let active = self.root.join("current/payload");
        let mut links = BINARIES
            .iter()
            .map(|name| {
                (
                    self.bin.join(name),
                    active.join(self.binary_subdir()).join(name),
                )
            })
            .collect::<Vec<_>>();
        if cfg!(target_os = "macos") {
            links.push((self.application.clone(), active));
        }
        links
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
            "[Desktop Entry]\nType=Application\nName=agentdocker\nComment=Orchestrate local AI agents\nExec=\"{}\"\nIcon={}\nTerminal=false\nCategories=Development;\n",
            executable, icon
        ))
    }

    fn preflight(&self) -> Result<()> {
        for path in [&self.root, &self.bin, &self.application] {
            ensure!(
                project::try_canonical(path)?.starts_with(&self.prefix),
                "installation path escapes its prefix through a symlink: {}",
                path.display()
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
        if !cfg!(target_os = "macos") {
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
        let generation = current.read_link()?;
        ensure!(
            generation.parent() == Some(self.root.join("generations").as_path()),
            "current pointer escapes managed generations"
        );
        let metadata = generation.join("activation.json");
        let file = dirs::private_file(&metadata, false, false)?;
        let activation: Activation = serde_json::from_reader(file.take(64 * 1024))?;
        ensure!(activation.format == 1, "unknown activation format");
        validate_release(&activation.current)?;
        if let Some(previous) = &activation.previous {
            validate_release(previous)?;
        }
        ensure!(
            generation.join("payload").read_link()? == self.payload(&activation.current),
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
                "agentdocker.app"
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
        "agentdocker.app"
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
    let (source, candidate, preview, local_preview, expect_release, expect_current) =
        match args.command {
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
                        &json!({"prefix":layout.prefix,"installation":active})
                    )?
                );
                return Ok(());
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
    if let Some(active) = &active {
        ensure!(
            candidate.state_schema >= active.current.state_schema,
            "artifact uses an older state schema; binary replacement cannot roll back the database"
        );
    }
    layout.preflight()?;
    let report = json!({"source":source, "candidate":candidate, "previous":active.as_ref().map(|active| &active.current),
        "application":layout.application, "bin":layout.bin, "versions":layout.root.join("versions"),
        "preview":preview, "local_preview":local_preview,
        "activation":"next app/CLI launch; an already-running daemon continues until explicitly restarted or reloaded"});
    if preview {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    layout.ensure_root()?;
    let lock = layout.root.join("install.lock");
    dirs::private_file(&lock, true, false)?;
    let _held = agentdocker_host::lock::try_exclusive(&lock)?
        .context("another desktop installation is in progress")?;
    let current = layout.active()?;
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
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
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
            state_schema: 8,
            target: "fixture".into(),
            tree_sha256: id,
            installation_lock: agentdocker_host::installation::LOCK_FORMAT,
            payload: if cfg!(target_os = "macos") {
                "agentdocker.app"
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
