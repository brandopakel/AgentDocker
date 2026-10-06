//! Remove only verified inactive releases and owned native launchers. Running
//! versions hold lifetime pins; stopped services also retain their references.
use super::*;
use base64::Engine;

#[derive(Default)]
struct ServiceReferences {
    any: bool,
    retained_versions: bool,
}

#[derive(Deserialize)]
struct Action {
    executable: String,
    arguments: String,
}

fn normalized(text: &str) -> String {
    let mut value = text.replace(r"\\?\", "").replace('/', "\\");
    for quote in ['\'', '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'] {
        value = value.replace(&format!("{quote}{quote}"), &quote.to_string());
    }
    value.to_lowercase()
}

fn references(root: &Path) -> Result<ServiceReferences> {
    let system = std::env::var_os("SystemRoot").context("Windows SystemRoot is not set")?;
    let executable = PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    ensure!(
        executable.is_absolute() && executable.is_file(),
        "cannot locate Windows PowerShell"
    );
    // Query all AgentDocker task homes, including stopped tasks. Names select
    // the inventory only; no task is edited or treated as owned by this query.
    let script = r#"$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';[Console]::OutputEncoding=[Text.UTF8Encoding]::new();$rows=@(Get-ScheduledTask -ErrorAction Stop | Where-Object {$_.TaskPath -eq '\' -and $_.TaskName -like 'AgentDocker-*'} | ForEach-Object {foreach($a in $_.Actions){[pscustomobject]@{executable=[string]$a.Execute;arguments=[string]$a.Arguments}}});ConvertTo-Json -InputObject $rows -Depth 4 -Compress"#;
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let output = command::run(
        &std::env::current_dir()?,
        &[
            executable.to_string_lossy().into_owned(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            encoded,
        ],
        Duration::from_secs(20),
    )?;
    ensure!(
        output.success,
        "cannot inspect Windows service references; installation preserved"
    );
    let actions: Vec<Action> = serde_json::from_str(output.text.trim())
        .context("cannot decode Windows service references")?;
    ensure!(
        actions.len() <= 1000,
        "Windows service inventory exceeds its bound"
    );
    let root = format!("{}\\", normalized(&root.to_string_lossy()));
    let mut found = ServiceReferences::default();
    for action in actions {
        let words: Vec<_> = action.arguments.split_whitespace().collect();
        let decoded = if let Some(at) = words
            .iter()
            .position(|w| w.eq_ignore_ascii_case("-EncodedCommand"))
        {
            let parsed = (|| -> Result<String> {
                ensure!(
                    at + 2 == words.len() && words[at + 1].len() <= 64 * 1024,
                    "unrecognized encoded service action"
                );
                let bytes = base64::engine::general_purpose::STANDARD.decode(words[at + 1])?;
                ensure!(bytes.len() % 2 == 0, "invalid encoded service action");
                let units: Vec<_> = bytes
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                Ok(String::from_utf16(&units)?)
            })();
            match parsed {
                Ok(text) => text,
                // An opaque app task may still refer to this store. Preserve
                // everything rather than guessing that it references nothing.
                Err(_) => {
                    return Ok(ServiceReferences {
                        any: true,
                        retained_versions: true,
                    });
                }
            }
        } else {
            action.arguments
        };
        let text = normalized(&format!("{} {decoded}", action.executable));
        if text.contains(&root) {
            found.any = true;
            found.retained_versions |= text.contains(&format!("{root}versions\\"))
                || !text.contains(&format!("{root}bin\\"));
        }
    }
    Ok(found)
}

#[derive(Serialize)]
struct Retained {
    path: PathBuf,
    reason: &'static str,
}

#[derive(Serialize)]
struct Plan {
    operation: &'static str,
    current: Option<String>,
    remove: Vec<PathBuf>,
    retained: Vec<Retained>,
    keep: Option<usize>,
}

impl Plan {
    fn id(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}

fn checked_version(layout: &Layout, directory: &Path) -> Result<()> {
    dirs::check_private_dir(directory)?;
    let id = directory
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid release directory")?;
    installation::pin_path(&layout.root, id)?;
    let entries = std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        entries.len() == 1 && entries[0].file_name() == "AgentDocker",
        "unrecognized release contents"
    );
    let (_, release) = inspect(&directory.join("AgentDocker"), true)?;
    validate(&release)?;
    ensure!(release.id == id, "retained release was modified");
    Ok(())
}

fn plan(
    layout: &Layout,
    keep: Option<usize>,
    applying: bool,
    inspect_services: impl FnOnce() -> Result<ServiceReferences>,
) -> Result<(Plan, installation::VersionInventoryGuard)> {
    layout.preflight()?;
    let active = layout.active()?;
    let mut plan = Plan {
        operation: if keep.is_some() { "prune" } else { "uninstall" },
        current: active.as_ref().map(|a| a.current.id.clone()),
        remove: Vec::new(),
        retained: Vec::new(),
        keep,
    };
    let pins = installation::VersionInventoryGuard::default();
    let Some(keep) = keep else {
        ensure!(
            !inspect_services()?.any,
            "a Windows service references this installation; uninstall its service registration first"
        );
        if present(&layout.bin)? {
            for name in BINARIES {
                let path = layout.bin.join(name);
                if present(&path)? {
                    plan.remove.push(path);
                }
            }
            plan.remove.push(layout.bin.clone());
        }
        if present(&layout.root.join("launcher.json"))? {
            plan.remove.push(layout.root.join("launcher.json"));
        }
        return Ok((plan, pins));
    };
    ensure!(keep <= 1000, "keep count exceeds 1000 versions");
    let mut entries = match std::fs::read_dir(layout.root.join("versions")) {
        Ok(entries) => entries.take(1001).collect::<std::io::Result<Vec<_>>>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((plan, pins)),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        entries.len() <= 1000,
        "retained version inventory exceeds 1000 entries"
    );
    entries.sort_by_key(|entry| {
        std::cmp::Reverse((
            entry.metadata().and_then(|m| m.modified()).ok(),
            entry.file_name(),
        ))
    });
    let mut extra = 0;
    let candidates: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.file_name().to_str()?.to_owned();
            let protected = active.as_ref().is_some_and(|a| {
                a.current.id == id || a.previous.as_ref().is_some_and(|p| p.id == id)
            });
            (id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) && !protected)
                .then_some(id)
        })
        .collect();
    let pins = installation::reserve_versions_for_inventory(&layout.root, &candidates, applying)?;
    let services = inspect_services()?;
    for entry in entries {
        let path = entry.path();
        let id = entry.file_name().to_string_lossy().into_owned();
        let reason = if active
            .as_ref()
            .is_some_and(|a| a.current.id == id || a.previous.as_ref().is_some_and(|p| p.id == id))
        {
            Some("active or rollback version")
        } else if services.retained_versions {
            Some("a stopped service may reference retained binaries")
        } else if checked_version(layout, &path).is_err() {
            Some("modified or unrecognized release; preserved")
        } else if extra < keep {
            extra += 1;
            Some("additional retained version")
        } else if pins.is_busy(&id) {
            Some("running process uses this version")
        } else {
            plan.remove.push(path.clone());
            None
        };
        if let Some(reason) = reason {
            plan.retained.push(Retained { path, reason });
        }
    }
    plan.remove.sort();
    plan.retained.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((plan, pins))
}

fn apply(layout: &Layout, plan: &Plan) -> Result<()> {
    layout.preflight()?;
    if plan.operation == "uninstall" {
        // Deactivation is atomic even if a later filesystem operation fails.
        // A subsequent portable CLI can finish removing any remaining verified
        // launchers; existing immutable processes and state stay untouched.
        let launchers: Vec<_> = plan
            .remove
            .iter()
            .filter(|path| path.parent() == Some(layout.bin.as_path()))
            .cloned()
            .collect();
        let retired = retirement::prepare(layout, &launchers)?;
        publish(
            &layout.root,
            "activation.json",
            &json!({"format":1,"inactive":true,"current":null,"previous":null}),
        )?;
        for path in &plan.remove {
            if path == &layout.bin {
                std::fs::remove_dir(path).with_context(|| {
                    format!("remove empty launcher directory {}", path.display())
                })?;
            } else {
                if path.parent() == Some(layout.bin.as_path()) {
                    ensure!(
                        native::launcher_root(path)?.as_deref()
                            == Some(layout.root.canonicalize()?.as_path()),
                        "launcher ownership changed; file preserved"
                    );
                    let destination = retired
                        .as_ref()
                        .context("retirement destination is missing")?
                        .join(path.file_name().context("launcher name is missing")?);
                    files::retire_open_regular(path, &destination)?;
                } else {
                    dirs::open_private_snapshot(path)?;
                    std::fs::remove_file(path)?;
                }
            }
        }
        retirement::cleanup(layout)?;
    } else {
        retirement::cleanup(layout)?;
        for path in &plan.remove {
            checked_version(layout, path)?;
            let retired = layout.root.join("retired");
            dirs::secure_state_dir(&retired)?;
            let stage = retired.join(uuid::Uuid::new_v4().to_string());
            dirs::secure_state_dir(&stage)?;
            let moved = stage.join(path.file_name().context("release lacks a name")?);
            std::fs::rename(path, &moved)?;
            checked_version(layout, &moved)
                .with_context(|| format!("changed release retained at {}", moved.display()))?;
            std::fs::remove_dir_all(&moved)?;
            std::fs::remove_dir(stage)?;
        }
    }
    Ok(())
}

pub(super) fn after_activation(layout: &Layout, _install: &lock::Lock) -> serde_json::Value {
    let result = (|| -> Result<(usize, usize)> {
        let _services = installation::service_inventory_guard(&layout.root, true)?;
        let (plan, _pins) = plan(layout, Some(0), true, || references(&layout.root))?;
        apply(layout, &plan)?;
        Ok((plan.remove.len(), plan.retained.len()))
    })();
    match result {
        Ok((removed, retained)) => {
            json!({"completed":true,"removed_versions":removed,"retained_versions":retained})
        }
        Err(error) => json!({"completed":false,"error":format!("{error:#}"),
            "summary":"activation succeeded; unused-build cleanup did not finish"}),
    }
}

pub(super) fn run(
    layout: &Layout,
    keep: Option<usize>,
    preview: bool,
    expected: Option<&str>,
) -> Result<()> {
    layout.preflight()?;
    let _install = if preview {
        None
    } else {
        Some(layout.install_lock()?)
    };
    let _services = installation::service_inventory_guard(&layout.root, !preview)?;
    let (plan, _pins) = plan(layout, keep, !preview, || references(&layout.root))?;
    let id = plan.id()?;
    if let Some(expected) = expected {
        ensure!(id == expected, "maintenance plan changed; review again");
    }
    if !preview {
        apply(layout, &plan)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"maintenance":plan,"plan_id":id,"preview":preview,
        "sessions":"running sessions continue; pinned versions are retained",
        "settings":"daemon state and provider configuration are preserved"}))?
    );
    Ok(())
}
