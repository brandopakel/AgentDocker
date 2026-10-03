//! Native per-user installation without symlinks or replacement of loaded
//! executables. One private record selects the current and rollback payloads.
use super::*;
use agentdocker_host::{files, installation, lock};
use installation::windows as native;

mod maintenance;
mod retirement;

pub(super) struct Layout {
    prefix: PathBuf,
    pub(super) root: PathBuf,
    bin: PathBuf,
    application: PathBuf,
}

fn present(path: &Path) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn record<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let file = match dirs::open_private_snapshot(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        file.metadata()?.len() <= 64 * 1024,
        "oversized installation record"
    );
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "installation record grew beyond its bound"
    );
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn publish<T: Serialize>(root: &Path, name: &str, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    ensure!(bytes.len() <= 64 * 1024, "oversized installation record");
    let staged = root.join(format!(".record-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = dirs::create_private_file(&staged)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        files::publish_snapshot(&staged, &root.join(name))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    result
}

fn validate(release: &Release) -> Result<()> {
    validate_release(release)?;
    ensure!(
        release
            .id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && release.target == "x86_64-pc-windows-msvc"
            && release.installation_lock == 1
            && release.launcher_redirect == 2,
        "Windows installation requires an x64 payload with lifetime pins and native launcher contract 2; rebuild the package"
    );
    Ok(())
}

impl Layout {
    fn new(prefix: PathBuf) -> Result<Self> {
        let prefix = project::try_canonical(&prefix)?;
        let root = prefix.join(native::STORE_SUFFIX);
        let bin = root.join("bin");
        Ok(Self {
            prefix,
            root,
            application: bin.join("agentdocker-ui.exe"),
            bin,
        })
    }

    fn active(&self) -> Result<Option<Activation>> {
        if !present(&self.root)? {
            return Ok(None);
        }
        dirs::check_private_dir(&self.root)?;
        let value: Option<serde_json::Value> = record(&self.root.join("activation.json"))?;
        if let Some(value) = &value
            && value["inactive"] == true
        {
            ensure!(
                value["format"] == 1 && value.get("current") == Some(&serde_json::Value::Null),
                "invalid inactive installation record"
            );
            return Ok(None);
        }
        let active: Option<Activation> = value.map(serde_json::from_value).transpose()?;
        if let Some(active) = &active {
            ensure!(active.format == 1, "unknown installation activation format");
            validate(&active.current)?;
            if let Some(previous) = &active.previous {
                validate(previous)?;
            }
        }
        Ok(active)
    }

    fn payload(&self, release: &Release) -> PathBuf {
        self.root
            .join("versions")
            .join(&release.id)
            .join("AgentDocker")
    }

    fn preflight(&self) -> Result<()> {
        if !present(&self.root)? {
            return Ok(());
        }
        dirs::check_private_dir(&self.root)?;
        for directory in ["versions", "pins", "bin"] {
            let path = self.root.join(directory);
            if present(&path)? {
                dirs::check_private_dir(&path)?;
            }
        }
        let receipt: Option<serde_json::Value> = record(&self.root.join("launcher.json"))?;
        if let Some(receipt) = &receipt {
            let hashes = receipt["binary_sha256"]
                .as_object()
                .context("invalid launcher receipt")?;
            ensure!(
                receipt["format"] == 1
                    && hashes.len() == BINARIES.len()
                    && BINARIES.iter().all(|name| hashes
                        .get(*name)
                        .and_then(|v| v.as_str())
                        .is_some_and(|hash| hash.len() == 64
                            && hash
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))),
                "unrecognized launcher receipt; preserved"
            );
        }
        if present(&self.bin)? {
            ensure!(
                receipt.is_some(),
                "launcher directory has no ownership receipt; preserved"
            );
            let names: std::collections::BTreeSet<_> = std::fs::read_dir(&self.bin)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<std::io::Result<_>>()?;
            let expected = BINARIES
                .iter()
                .map(|name| std::ffi::OsString::from(*name))
                .collect();
            ensure!(
                names.is_subset(&expected),
                "launcher directory has unrecognized contents; preserved"
            );
            ensure!(
                self.active()?.is_none() || names == expected,
                "active installation has incomplete launchers"
            );
            for name in &names {
                ensure!(
                    native::launcher_root(&self.bin.join(name))?.as_deref()
                        == Some(self.root.canonicalize()?.as_path()),
                    "launcher ownership could not be verified"
                );
            }
        } else {
            ensure!(
                self.active()?.is_none(),
                "active installation is missing its launchers"
            );
        }
        Ok(())
    }

    pub(super) fn ensure_root(&self) -> Result<()> {
        self.preflight()?;
        dirs::secure_state_dir(&self.root)?;
        for directory in ["versions", "pins"] {
            dirs::secure_state_dir(&self.root.join(directory))?;
        }
        Ok(())
    }

    fn install_lock(&self) -> Result<lock::Lock> {
        self.ensure_root()?;
        let path = self.root.join("install.lock");
        dirs::private_file(&path, true, false)?;
        lock::try_exclusive(&path)?.context("another desktop installation is in progress")
    }

    fn prepare_launchers(&self, release: &Release) -> Result<()> {
        self.preflight()?;
        if present(&self.bin)? {
            ensure!(
                BINARIES.iter().all(|name| self.bin.join(name).is_file()),
                "finish the interrupted desktop uninstall before installing again"
            );
            return Ok(());
        }
        let payload = self.payload(release);
        let hashes: BTreeMap<_, _> = BINARIES
            .iter()
            .map(|name| Ok((*name, file_hash(&payload.join(name))?)))
            .collect::<Result<_>>()?;
        let receipt = json!({"format":1,"binary_sha256":hashes});
        // A prepared but unpublished bootstrap can be recovered only with
        // the same bytes. Never replace an unrelated pre-existing receipt.
        if let Some(existing) = record::<serde_json::Value>(&self.root.join("launcher.json"))? {
            ensure!(
                existing == receipt,
                "interrupted launcher setup requires its original package"
            );
        }
        let staged = self
            .root
            .join(format!(".launchers-{}", uuid::Uuid::new_v4()));
        dirs::secure_state_dir(&staged)?;
        for name in BINARIES {
            let mut source = files::open_regular(&payload.join(name))?;
            let mut destination = dirs::create_private_file(&staged.join(name))?;
            std::io::copy(&mut source, &mut destination)?;
            destination.sync_all()?;
            ensure!(
                file_hash(&staged.join(name))? == hashes[name],
                "launcher changed while copying"
            );
        }
        publish(&self.root, "launcher.json", &receipt)?;
        std::fs::rename(&staged, &self.bin)?;
        self.preflight()
    }
}

/// Flush copied files using write-capable handles. Windows does not support
/// Unix directory fsync or FlushFileBuffers on a read-only file handle.
fn flush_payload(root: &Path) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        let metadata = payload_metadata(&path)?;
        if metadata.is_dir() {
            flush_payload(&path)?;
        } else {
            files::open_regular(&path)?;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)?
                .sync_all()?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn perform(
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
    validate(&candidate)?;
    ensure!(
        local_preview,
        "Windows packages currently require --local-preview"
    );
    if let Some(expected) = expect_release {
        ensure!(
            expected == candidate.id,
            "artifact changed since preview; review again"
        );
    }
    if let Some(expected) = expect_current {
        ensure!(
            expected == active.as_ref().map_or("none", |a| &a.current.id),
            "active installation changed since preview; review again"
        );
    }
    ensure!(
        candidate.state_schema >= required_state_schema(active.as_ref(), &dirs::home())?,
        "artifact uses an older state schema; binary replacement cannot roll back the database"
    );
    layout.preflight()?;
    let mut report = json!({
        "source":source,"candidate":candidate,"previous":active.as_ref().map(|a| &a.current),
        "application":layout.application,"bin":layout.bin,"versions":layout.root.join("versions"),
        "preview":preview,"local_preview":local_preview,
        "activation":"next app/CLI launch; running sessions and daemon continue unchanged"
    });
    if preview {
        return Ok(report);
    }
    let _held = layout.install_lock()?;
    layout.preflight()?;
    let current = layout.active()?;
    ensure!(
        current == active,
        "active installation changed; review again"
    );
    ensure!(
        candidate.state_schema >= required_state_schema(current.as_ref(), &dirs::home())?,
        "stored state changed since preview; review again"
    );
    let version = layout.root.join("versions").join(&candidate.id);
    if present(&version)? {
        dirs::check_private_dir(&version)?;
        let (_, retained) = inspect(&layout.payload(&candidate), local_preview)?;
        ensure!(retained == candidate, "retained release was modified");
    } else {
        let stage = layout
            .root
            .join("versions")
            .join(format!(".stage-{}", uuid::Uuid::new_v4()));
        dirs::secure_state_dir(&stage)?;
        let staged = stage.join("AgentDocker");
        copy_payload(&source, &staged)?;
        let (_, copied) = inspect(&staged, local_preview)?;
        ensure!(
            copied == candidate,
            "artifact changed while copying; current installation preserved"
        );
        flush_payload(&staged)?;
        std::fs::rename(&stage, &version)?;
    }
    layout.prepare_launchers(&candidate)?;
    let changed = current
        .as_ref()
        .is_none_or(|a| a.current.id != candidate.id);
    if changed {
        publish(
            &layout.root,
            "activation.json",
            &Activation {
                format: 1,
                current: candidate,
                previous: current.map(|a| a.current),
            },
        )?;
    }
    let serving = serving_daemon(socket);
    report["daemon"] = json!({"answered":!serving.is_null(),"reloaded":false,"serving":serving,
        "summary":"activated for the next launch; running sessions and daemon continue unchanged"});
    if changed {
        report["retention"] = maintenance::after_activation(layout, &_held);
    }
    Ok(report)
}

pub(super) fn run(args: DesktopArgs, socket: Option<PathBuf>) -> Result<()> {
    let prefix = args
        .prefix
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .context("cannot locate LOCALAPPDATA; provide --prefix")?;
    let layout = Layout::new(prefix)?;
    let active = layout.active()?;
    let (source, candidate, preview, local_preview, expect_release, expect_current) =
        match args.command {
            DesktopCommand::Status => {
                layout.preflight()?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({"prefix":layout.prefix,
                "application":layout.application,"installation":active,"homebrew":null,
                "daemon":serving_daemon(socket)}))?
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
                    candidate == *previous,
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
            DesktopCommand::Update {
                feed,
                check,
                apply,
                local_preview,
                socket: update_socket,
            } => {
                return update::run(
                    &layout,
                    active.as_ref(),
                    update::Options {
                        feed,
                        check,
                        apply,
                        local_preview,
                        socket: update_socket.or(socket),
                    },
                );
            }
            DesktopCommand::Prune {
                keep,
                preview,
                expect_plan,
            } => {
                return maintenance::run(&layout, Some(keep), preview, expect_plan.as_deref());
            }
            DesktopCommand::Uninstall {
                preview,
                expect_plan,
            } => {
                return maintenance::run(&layout, None, preview, expect_plan.as_deref());
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
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
