//! Reviewed cleanup of owned launchers and unused pinned releases.
use super::*;
use agentdocker_host::{installation, lock};

#[derive(Serialize)]
struct Entry {
    path: PathBuf,
    reason: &'static str,
}

#[derive(Serialize)]
struct Plan {
    operation: &'static str,
    current: Option<String>,
    remove: Vec<PathBuf>,
    retained: Vec<Entry>,
    keep: Option<usize>,
}

impl Plan {
    fn id(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}

/// Validating an inactive payload uses its recorded content identity, not a
/// new signing/notarization decision. Changed or unrecognized files stay put.
fn checked_version(layout: &Layout, directory: &Path) -> Result<u32> {
    let id = directory
        .file_name()
        .and_then(|n| n.to_str())
        .context("invalid release name")?;
    installation::pin_path(&layout.root, id)?;
    ensure!(
        directory.symlink_metadata()?.is_dir(),
        "release directory is not a regular directory"
    );
    let payload = directory.join(if cfg!(target_os = "macos") {
        "AgentDocker.app"
    } else {
        "agentdocker-desktop"
    });
    let entries = std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        entries.len() == 1 && entries[0].path() == payload,
        "release directory has unrecognized contents; preserved"
    );
    ensure!(
        payload.symlink_metadata()?.is_dir(),
        "release payload is not a regular directory"
    );
    ensure!(
        tree_hash(&payload)? == id,
        "retained release was changed; preserved"
    );
    let metadata = payload.join(if cfg!(target_os = "macos") {
        "Contents/Resources/build.json"
    } else {
        "build.json"
    });
    let file = agentdocker_host::files::open_regular(&metadata)?;
    ensure!(
        file.metadata()?.len() <= 1024 * 1024,
        "oversized release metadata"
    );
    let value: serde_json::Value = serde_json::from_reader(file.take(1024 * 1024))?;
    ensure!(
        value["format"] == 1 && value["product"] == "agentdocker",
        "unknown release metadata; preserved"
    );
    Ok(value["installation_lock"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0))
}

fn plan(
    layout: &Layout,
    keep: Option<usize>,
    applying: bool,
    service_installed: bool,
) -> Result<(Plan, Vec<lock::Lock>)> {
    layout.preflight()?;
    let active = layout.active()?;
    let mut plan = Plan {
        operation: if keep.is_some() { "prune" } else { "uninstall" },
        current: active.as_ref().map(|a| a.current.id.clone()),
        remove: Vec::new(),
        retained: Vec::new(),
        keep,
    };
    let mut pins = Vec::new();
    let Some(keep) = keep else {
        ensure!(
            !service_installed,
            "a user service is installed; preview its removal with agentdocker daemon uninstall --dry-run before removing desktop launchers"
        );
        for (path, _) in layout.links() {
            if path.symlink_metadata().is_ok() {
                plan.remove.push(path);
            }
        }
        // The Linux launcher file, or the macOS launcher bundle (or the
        // symlink earlier releases installed); preflight has already
        // refused anything at that path that is not ours.
        if layout.application.symlink_metadata().is_ok() {
            plan.remove.push(layout.application.clone());
        }
        if active.is_some() {
            plan.remove.push(layout.root.join("current"));
        }
        return Ok((plan, pins));
    };
    ensure!(keep <= 1000, "keep count exceeds 1000 versions");
    let versions = layout.root.join("versions");
    let mut directories = match std::fs::read_dir(&versions) {
        Ok(entries) => entries.take(1001).collect::<std::io::Result<Vec<_>>>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((plan, pins)),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        directories.len() <= 1000,
        "retained version inventory exceeds 1000 entries"
    );
    // Directory modification time only chooses additional retention order;
    // content hashes and lifetime pins, never timestamps, authorize removal.
    directories.sort_by_key(|entry| {
        std::cmp::Reverse((
            entry.metadata().and_then(|m| m.modified()).ok(),
            entry.file_name(),
        ))
    });
    let mut extra = 0;
    for entry in directories {
        let path = entry.path();
        let id = entry.file_name().to_string_lossy().into_owned();
        let protected = active
            .as_ref()
            .is_some_and(|a| a.current.id == id || a.previous.as_ref().is_some_and(|p| p.id == id));
        let reason = if protected {
            Some("active or rollback version")
        } else if service_installed {
            Some("installed user service may reference retained binaries")
        } else if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some("unrecognized directory; preserved")
        } else if checked_version(layout, &path)? != installation::LOCK_FORMAT {
            Some("older release has no lifetime pin contract")
        } else if extra < keep {
            extra += 1;
            Some("additional retained version")
        } else {
            let pin = installation::pin_path(&layout.root, &id)?;
            if applying {
                dirs::secure_state_dir(&layout.root.join("pins"))?;
                dirs::private_file(&pin, true, false)?;
            }
            // Pin files are permanent. A preview never creates a missing pin.
            let held = if pin.exists() {
                lock::try_exclusive_existing(&pin)?
            } else {
                None
            };
            if pin.exists() && held.is_none() {
                Some("running process uses this version")
            } else {
                if applying && let Some(held) = held {
                    pins.push(held);
                }
                plan.remove.push(path.clone());
                None
            }
        };
        if let Some(reason) = reason {
            plan.retained.push(Entry { path, reason });
        }
    }
    plan.remove.sort();
    plan.retained.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((plan, pins))
}

fn apply(layout: &Layout, plan: &Plan) -> Result<()> {
    layout.preflight()?;
    if plan.operation == "uninstall" {
        for path in &plan.remove {
            // preflight verifies every owned link and the exact Linux launcher.
            // Recheck while each path is still present before unlinking it.
            layout.preflight()?;
            if path.symlink_metadata()?.is_dir() {
                // Only our own launcher bundle reaches here (preflight).
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
            std::fs::File::open(path.parent().context("launcher has no parent")?)?.sync_all()?;
        }
    } else {
        dirs::secure_state_dir(&layout.root.join("retired"))?;
        for path in &plan.remove {
            checked_version(layout, path)?;
            // Keep the staging directory on any verification/cleanup failure.
            // Never let a TempDir destructor erase an unexpectedly changed tree.
            let stage = tempfile::tempdir_in(layout.root.join("retired"))?.keep();
            let moved = stage.join(path.file_name().context("release lacks a name")?);
            std::fs::rename(path, &moved)?;
            std::fs::File::open(layout.root.join("versions"))?.sync_all()?;
            checked_version(layout, &moved)
                .with_context(|| format!("changed release retained at {}", moved.display()))?;
            std::fs::remove_dir_all(&moved)?;
            std::fs::remove_dir(&stage)?;
        }
    }
    Ok(())
}

fn service_installed(layout: &Layout) -> Result<bool> {
    let mut homes = vec![layout.prefix.clone()];
    if let Some(home) = std::env::home_dir() {
        homes.push(home);
    }
    for home in homes {
        let path = home.join(if cfg!(target_os = "macos") {
            "Library/LaunchAgents/dev.agentdocker.agentd.plist"
        } else {
            ".config/systemd/user/agentd.service"
        });
        match path.symlink_metadata() {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

pub(super) fn run(
    layout: &Layout,
    keep: Option<usize>,
    preview: bool,
    expected: Option<&str>,
) -> Result<()> {
    // Refuse redirected or foreign installation paths before creating a lock.
    layout.preflight()?;
    let _install_lock = if !preview {
        layout.ensure_root()?;
        dirs::private_file(&layout.root.join("install.lock"), true, false)?;
        Some(
            lock::try_exclusive(&layout.root.join("install.lock"))?
                .context("another desktop installation is in progress")?,
        )
    } else {
        None
    };
    layout.preflight()?;
    let (plan, _pins) = plan(layout, keep, !preview, service_installed(layout)?)?;
    let id = plan.id()?;
    if let Some(expected) = expected {
        ensure!(id == expected, "maintenance plan changed; review again");
    }
    if !preview && !plan.remove.is_empty() {
        apply(layout, &plan)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "maintenance":plan, "plan_id":id, "preview":preview,
            "sessions":"running sessions continue; versions with lifetime pins are retained",
            "settings":"daemon state, provider configuration and service registration are preserved",
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Layout) {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().to_owned()).unwrap();
        layout.ensure_root().unwrap();
        (temp, layout)
    }

    fn retained(layout: &Layout, marker: &str, pin: u32) -> Release {
        let mut release = super::super::tests::release(layout, marker);
        let payload = layout.payload(&release);
        let metadata = payload.join(if cfg!(target_os = "macos") {
            "Contents/Resources/build.json"
        } else {
            "build.json"
        });
        std::fs::create_dir_all(metadata.parent().unwrap()).unwrap();
        std::fs::write(
            metadata,
            json!({"format":1,"product":"agentdocker","installation_lock":pin}).to_string(),
        )
        .unwrap();
        let old = payload.parent().unwrap().to_owned();
        release.id = tree_hash(&payload).unwrap();
        release.tree_sha256 = release.id.clone();
        release.installation_lock = pin;
        std::fs::rename(old, layout.root.join("versions").join(&release.id)).unwrap();
        release
    }

    #[test]
    fn pruning_keeps_active_previous_running_and_legacy_releases() {
        let (_temp, layout) = fixture();
        let old = retained(&layout, "old", 1);
        let busy = retained(&layout, "busy", 1);
        let legacy = retained(&layout, "legacy", 0);
        let previous = retained(&layout, "previous", 1);
        let current = retained(&layout, "current", 1);
        layout
            .activate(current.clone(), Some(previous.clone()))
            .unwrap();
        dirs::secure_state_dir(&layout.root.join("pins")).unwrap();
        let pinned = lock::try_shared(&installation::pin_path(&layout.root, &busy.id).unwrap())
            .unwrap()
            .unwrap();
        let (review, _) = plan(&layout, Some(0), false, false).unwrap();
        assert_eq!(review.remove, [layout.root.join("versions").join(&old.id)]);
        assert_eq!(review.retained.len(), 4);
        assert!(
            !installation::pin_path(&layout.root, &old.id)
                .unwrap()
                .exists(),
            "preview must not create pins"
        );
        let (action, locks) = plan(&layout, Some(0), true, false).unwrap();
        assert_eq!(review.id().unwrap(), action.id().unwrap());
        apply(&layout, &action).unwrap();
        drop(locks);
        assert!(!layout.payload(&old).exists());
        for release in [&busy, &legacy, &previous, &current] {
            assert!(layout.payload(release).exists());
        }
        assert!(
            installation::pin_path(&layout.root, &old.id)
                .unwrap()
                .exists()
        );
        drop(pinned);
        // Other tests may have forked with the pin descriptor before drop;
        // their exec closes it. Wait for that bounded release, as lock's own
        // tests do, while still requiring precisely the former busy version.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let (next, _) = plan(&layout, Some(0), false, false).unwrap();
            if !next.remove.is_empty() {
                assert_eq!(next.remove, [layout.root.join("versions").join(&busy.id)]);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "release pin never cleared"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn uninstall_removes_only_owned_launchers_and_is_resumable() {
        let (_temp, layout) = fixture();
        let release = retained(&layout, "current", 1);
        layout.activate(release.clone(), None).unwrap();
        let settings = layout.prefix.join("provider-settings.json");
        std::fs::write(&settings, "keep settings").unwrap();
        let (review, _) = plan(&layout, None, false, false).unwrap();
        assert!(layout.active().unwrap().is_some());
        apply(&layout, &review).unwrap();
        assert!(layout.active().unwrap().is_none());
        assert!(layout.payload(&release).exists());
        assert_eq!(std::fs::read_to_string(settings).unwrap(), "keep settings");
        assert!(
            plan(&layout, None, false, false)
                .unwrap()
                .0
                .remove
                .is_empty()
        );
        for (link, _) in layout.links() {
            assert!(link.symlink_metadata().is_err());
        }
    }

    #[test]
    fn an_absent_root_is_locked_before_non_preview_maintenance() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().to_owned()).unwrap();
        run(&layout, Some(0), true, None).unwrap();
        assert!(!layout.root.exists());
        run(&layout, Some(0), false, None).unwrap();
        let held = lock::try_exclusive(&layout.root.join("install.lock"))
            .unwrap()
            .unwrap();
        assert!(
            run(&layout, None, false, None)
                .unwrap_err()
                .to_string()
                .contains("in progress")
        );
        drop(held);
    }

    #[test]
    fn escaped_installation_is_refused_before_creating_maintenance_state() {
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("user");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(prefix.join(".local/share/agentdocker")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, prefix.join(".local/share/agentdocker/desktop"))
            .unwrap();
        let layout = Layout::new(prefix).unwrap();
        assert!(run(&layout, Some(0), false, None).is_err());
        assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn changed_payload_or_foreign_launcher_is_preserved() {
        let (_temp, layout) = fixture();
        let release = retained(&layout, "changed", 1);
        std::fs::write(layout.payload(&release).join("foreign-file"), "preserve").unwrap();
        assert!(plan(&layout, Some(0), false, false).is_err());
        assert!(layout.payload(&release).join("foreign-file").exists());
        std::fs::create_dir_all(&layout.bin).unwrap();
        std::fs::write(layout.bin.join("agentdocker"), "foreign command").unwrap();
        assert!(plan(&layout, None, false, false).is_err());
        assert_eq!(
            std::fs::read_to_string(layout.bin.join("agentdocker")).unwrap(),
            "foreign command"
        );
    }

    #[test]
    fn activation_changes_plan_and_user_services_prevent_orphaning_references() {
        let (_temp, layout) = fixture();
        let first = retained(&layout, "first", 1);
        let second = retained(&layout, "second", 1);
        let old = retained(&layout, "old", 1);
        layout.activate(first.clone(), None).unwrap();
        let (before, _) = plan(&layout, None, false, false).unwrap();
        layout.activate(second, Some(first)).unwrap();
        let (after, _) = plan(&layout, None, false, false).unwrap();
        assert_ne!(before.id().unwrap(), after.id().unwrap());
        assert!(plan(&layout, None, false, true).is_err());
        let (prune, _) = plan(&layout, Some(0), false, true).unwrap();
        assert!(prune.remove.is_empty());
        assert!(layout.payload(&old).exists());
        let (keep_extra, _) = plan(&layout, Some(1), false, false).unwrap();
        assert!(keep_extra.remove.is_empty());
    }
}
