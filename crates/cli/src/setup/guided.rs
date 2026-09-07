//! Saved setup plans contain private before/after snapshots, never printed.
//! Each file is replaced atomically. A multi-file plan can be resumed after
//! interruption; it never claims a filesystem-wide transaction.
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::runtime::{McpWiring, RUNTIMES, RuntimeInfo};
use agentdocker_core::{Request, Response};
use agentdocker_host::{
    dirs, project,
    runtimes::{self, Roots},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::Client;

const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct Change {
    runtime: String,
    channel: String,
    path: PathBuf,
    target: PathBuf,
    before: Option<String>,
    after: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Plan {
    format: u32,
    id: String,
    phase: String,
    executable: PathBuf,
    changes: Vec<Change>,
    notes: Vec<String>,
}

impl Plan {
    /// Deliberately separate from serialization of the private snapshots.
    fn view(&self) -> Value {
        json!({"id": self.id, "phase": self.phase, "executable": self.executable,
            "changes": self.changes.iter().map(|change| json!({
                "runtime":change.runtime, "channel":change.channel, "path":change.path,
                "action": if change.before.is_some() {"add to existing configuration"} else {"create configuration"}
            })).collect::<Vec<_>>(), "notes": self.notes})
    }
}

/// Read bounded UTF-8 configuration without hanging on a special file.
fn read_config(path: &Path) -> Result<Option<String>> {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    ensure!(
        file.metadata()?.is_file(),
        "{} is not a regular configuration file",
        path.display()
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG_BYTES,
        "configuration exceeds 8 MiB"
    );
    Ok(Some(
        String::from_utf8(bytes).context("configuration is not UTF-8")?,
    ))
}

/// Plan edits from injectable provider roots, without changing their files.
fn prepare(roots: &Roots, names: &[String], executable: &Path) -> Result<Plan> {
    let inventory = runtimes::inventory(roots, "agentdocker");
    let targets: Vec<&RuntimeInfo> = if names.is_empty() {
        inventory
            .iter()
            .filter(|runtime| runtime.installed())
            .collect()
    } else {
        names
            .iter()
            .map(|name| {
                inventory
                    .iter()
                    .find(|runtime| runtime.name == *name)
                    .with_context(|| format!("unknown runtime `{name}`"))
            })
            .collect::<Result<_>>()?
    };
    let mut plan = Plan {
        format: 1,
        id: uuid::Uuid::new_v4().to_string(),
        phase: "prepared".into(),
        executable: executable.to_owned(),
        changes: Vec::new(),
        notes: Vec::new(),
    };
    for runtime in targets {
        let spec = RUNTIMES
            .iter()
            .find(|spec| spec.name == runtime.name)
            .context("missing runtime specification")?;
        // Hooks cover the Claude Code lifecycle without rewriting its mutable
        // .claude.json application state or installing a duplicate MCP identity.
        let (path, channel) = if spec.hooks {
            (roots.home.join(".claude/settings.json"), "hooks")
        } else if let Some(path) = runtimes::mcp_config_path(spec, roots) {
            (path, "mcp")
        } else {
            plan.notes.push(format!(
                "{}: no supported setup adapter; discovery alone does not enable coordination",
                runtime.label
            ));
            continue;
        };
        if plan.changes.iter().any(|change| change.path == path) {
            continue;
        }
        let before = read_config(&path)?;
        let after = if spec.hooks {
            let mut settings: Value = match before.as_deref() {
                Some(raw) => {
                    serde_json::from_str(raw).context("invalid Claude Code settings JSON")?
                }
                None => json!({}),
            };
            let command = runtimes::claude_hook_command(executable)?;
            if crate::hooks::merge_claude_code_hooks(&mut settings, &command)? == 0 {
                None
            } else {
                Some(format!("{}\n", serde_json::to_string_pretty(&settings)?))
            }
        } else {
            match spec.mcp {
                McpWiring::JsonServers { .. } => {
                    super::json_edit(&path, before.as_deref(), executable, spec.name)?
                }
                McpWiring::TomlServers { .. } => {
                    super::toml_edit(&path, before.as_deref(), executable, spec.name)?
                }
                McpWiring::None => unreachable!(),
            }
        };
        if let Some(after) = after {
            let target = project::try_canonical(&path)?;
            plan.changes.push(Change {
                runtime: runtime.name.clone(),
                channel: channel.into(),
                path,
                target,
                before,
                after,
            });
        } else {
            plan.notes.push(format!(
                "{}: {channel} already configured; connection is not yet verified",
                runtime.label
            ));
        }
    }
    plan.notes.push("Restart the selected provider session after applying. Existing sessions are not reconfigured.".into());
    plan.notes.push("Your provider may ask you to approve MCP tools. Setup preserves provider approval settings.".into());
    Ok(plan)
}

/// Prepare private receipt storage beneath the selected AgentDocker home.
fn directory(home: &Path) -> Result<PathBuf> {
    dirs::secure_state_dir(home)?;
    let directory = home.join("setup");
    dirs::secure_state_dir(&directory)?;
    Ok(directory)
}

/// Restrict receipt selection to canonical UUID filenames.
fn receipt(directory: &Path, id: &str) -> Result<PathBuf> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id),
        "invalid setup plan id"
    );
    Ok(directory.join(format!("{id}.json")))
}

/// Atomically sync the complete recovery receipt before configuration writes.
fn save(directory: &Path, plan: &Plan) -> Result<()> {
    let path = receipt(directory, &plan.id)?;
    if path.symlink_metadata().is_ok() {
        dirs::private_file(&path, false, false)?;
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    let contents = serde_json::to_vec_pretty(plan)?;
    ensure!(
        contents.len() <= 64 * 1024 * 1024,
        "setup receipt exceeds 64 MiB"
    );
    temporary.write_all(&contents)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// Load a private receipt with explicit format and size validation.
fn load(directory: &Path, id: &str) -> Result<Plan> {
    let file = dirs::private_file(&receipt(directory, id)?, false, false)?;
    ensure!(
        file.metadata()?.len() <= 64 * 1024 * 1024,
        "setup receipt exceeds 64 MiB"
    );
    let plan: Plan = serde_json::from_reader(file.take(64 * 1024 * 1024))?;
    ensure!(
        plan.format == 1 && plan.id == id,
        "unsupported or mismatched setup receipt"
    );
    Ok(plan)
}

/// Accept only the original physical target and its recorded before/after bytes.
fn check(change: &Change) -> Result<Option<String>> {
    ensure!(
        project::try_canonical(&change.path)? == change.target,
        "{} now points to a different file; configuration left unchanged",
        change.path.display()
    );
    let current = read_config(&change.path)?;
    ensure!(
        current == change.before || current.as_deref() == Some(&change.after),
        "{} changed since preview/apply; preserve the user's edits and create a fresh plan",
        change.path.display()
    );
    Ok(current)
}

/// Apply or undo a saved plan, allowing mixed states only during recovery.
fn apply(directory: &Path, plan: &mut Plan, undo: bool) -> Result<()> {
    let allowed = if undo {
        ["prepared", "applying", "applied", "undoing", "undone"].as_slice()
    } else {
        ["prepared", "applying", "applied"].as_slice()
    };
    ensure!(
        allowed.contains(&plan.phase.as_str()),
        "this plan was undone; create a fresh preview to apply again"
    );
    if !undo {
        let executable = std::fs::metadata(&plan.executable)
            .context("the previewed agentdocker executable is no longer available")?;
        ensure!(
            executable.is_file() && executable.permissions().mode() & 0o111 != 0,
            "the previewed agentdocker path is not executable"
        );
    }
    // Prepared/applied/completed receipts have one exact expected state.
    // Only a durable in-progress receipt can explain a mix of before/after.
    for change in &plan.changes {
        let current = check(change)?;
        let expected = match plan.phase.as_str() {
            "prepared" | "undone" => Some(change.before.as_deref()),
            "applied" => Some(Some(change.after.as_str())),
            _ => None,
        };
        if let Some(expected) = expected {
            ensure!(
                current.as_deref() == expected,
                "{} changed after the last setup step",
                change.path.display()
            );
        }
    }
    if (!undo && plan.phase == "applied") || (undo && plan.phase == "undone") {
        return Ok(());
    }
    if undo && plan.phase == "prepared" {
        plan.phase = "undone".into();
        return save(directory, plan);
    }
    plan.phase = if undo { "undoing" } else { "applying" }.into();
    save(directory, plan)?; // durable recovery/undo snapshots precede configuration writes
    for change in &plan.changes {
        let current = check(change)?;
        let desired = if undo {
            change.before.as_deref()
        } else {
            Some(change.after.as_str())
        };
        if current.as_deref() == desired {
            continue;
        }
        if let Some(contents) = desired {
            super::write_config(&change.path, current.as_deref(), contents)?;
        } else {
            // Remove only the new file this receipt created; never its directory.
            check(change)?;
            std::fs::remove_file(&change.target)?;
        }
        if let Some(parent) = change.target.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
    }
    plan.phase = if undo { "undone" } else { "applied" }.into();
    save(directory, plan)
}

/// List healthy receipts even when an interrupted write, manual edit or
/// incompatible receipt makes another entry unreadable. Never remove evidence.
fn list_plans(directory: &Path) -> Result<Value> {
    let mut skipped = 0usize;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        match entry {
            Ok(entry) => paths.push(entry),
            Err(_) => skipped += 1,
        }
    }
    paths.sort_by_key(|entry| {
        std::cmp::Reverse(
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok(),
        )
    });
    let mut plans = Vec::new();
    for entry in paths {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let loaded = path
            .file_stem()
            .and_then(|name| name.to_str())
            .context("invalid setup receipt filename")
            .and_then(|id| load(directory, id));
        match loaded {
            Ok(plan) => {
                plans.push(plan.view());
                if plans.len() == 100 {
                    break;
                }
            }
            Err(_) => skipped += 1,
        }
    }
    Ok(json!({"plans":plans, "limit":100, "skipped_receipts":skipped}))
}

/// One mutually exclusive guided setup operation.
pub enum Action<'a> {
    Preview,
    Apply(&'a str),
    Undo(&'a str),
    Health,
    List,
    Show(&'a str),
}

/// Dispatch a guided operation and print only its redacted public description.
pub async fn run(
    socket: Option<PathBuf>,
    names: &[String],
    action: Action<'_>,
    json_output: bool,
) -> Result<()> {
    let apply_id = if let Action::Apply(id) = action {
        Some(id)
    } else {
        None
    };
    let undo_id = if let Action::Undo(id) = action {
        Some(id)
    } else {
        None
    };
    let show_id = if let Action::Show(id) = action {
        Some(id)
    } else {
        None
    };
    let health = matches!(action, Action::Health);
    let list = matches!(action, Action::List);
    let mut roots = Roots::from_env();
    roots.versions = false;
    let value = if list || show_id.is_some() {
        let home = dirs::home();
        let directory_path = home.join("setup");
        if list && !directory_path.exists() {
            json!({"plans": []})
        } else {
            ensure!(directory_path.is_dir(), "no saved setup plans");
            let directory = directory(&home)?;
            if let Some(id) = show_id {
                load(&directory, id)?.view()
            } else {
                list_plans(&directory)?
            }
        }
    } else if health {
        let client = Client::new(socket).with_start_timeout(None);
        let (reachable, diagnostic) =
            match tokio::time::timeout(Duration::from_secs(3), client.call(&Request::Ping)).await {
                Ok(Ok(Response::Pong { version, .. })) => (true, format!("agentd {version}")),
                Ok(Ok(_)) => (false, "unexpected daemon response".to_owned()),
                Ok(Err(error)) => (false, error.to_string()),
                Err(_) => (false, "daemon connection timed out".to_owned()),
            };
        let inventory = runtimes::inventory(&roots, "agentdocker");
        for name in names {
            ensure!(
                inventory.iter().any(|runtime| runtime.name == *name),
                "unknown runtime `{name}`"
            );
        }
        json!({"daemon_reachable": reachable, "daemon": diagnostic,
            "runtimes": inventory.iter().filter(|runtime| names.is_empty() || names.contains(&runtime.name)).map(|runtime| json!({
                "name":runtime.name, "installed":runtime.installed(), "mcp_configuration":runtime.mcp,
                "hooks_configuration":runtime.hooks, "provider_round_trip":"not_tested"
            })).collect::<Vec<_>>(),
            "notes":["Configuration detection and daemon connectivity are separate checks. A fresh provider session must prove tool/message consumption."]})
    } else {
        let directory = directory(&dirs::home())?;
        let lock_path = directory.join("setup.lock");
        dirs::private_file(&lock_path, true, false)?;
        let _lock = agentdocker_host::lock::try_exclusive(&lock_path)?
            .context("another setup operation is in progress")?;
        let mut plan = if let Some(id) = apply_id.or(undo_id) {
            load(&directory, id)?
        } else {
            prepare(&roots, names, &std::env::current_exe()?)?
        };
        if apply_id.is_some() || undo_id.is_some() {
            if let Err(error) = apply(&directory, &mut plan, undo_id.is_some()) {
                bail!(
                    "{error:#}; plan {} is retained for review/resume or undo",
                    plan.id
                );
            }
        } else {
            save(&directory, &plan)?;
        }
        plan.view()
    };
    if json_output {
        println!("{}", serde_json::to_string(&value)?);
    } else if let Some(id) = value["id"].as_str() {
        eprintln!("{}", serde_json::to_string_pretty(&value)?);
        println!("{id}");
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(path: &Path) -> Roots {
        Roots {
            home: path.to_owned(),
            codex_home: None,
            path: vec![],
            app_dirs: vec![],
            versions: false,
        }
    }

    fn plan(root: &Path, names: &[&str]) -> Plan {
        prepare(
            &roots(root),
            &names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
            &std::env::current_exe().unwrap(),
        )
        .unwrap()
    }

    fn old_codex(root: &Path) -> PathBuf {
        let path = root.join(".codex/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# preserve this comment\nfixture_secret = 'PRIVATE-FIXTURE-ONLY'\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn preview_is_read_only_and_public_view_never_contains_configuration_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let config = old_codex(tmp.path());
        let before = std::fs::read(&config).unwrap();
        let prepared = plan(tmp.path(), &["codex", "claude-code"]);
        assert_eq!(prepared.changes.len(), 2);
        assert_eq!(std::fs::read(&config).unwrap(), before);
        assert!(!tmp.path().join(".claude").exists());
        assert!(!prepared.view().to_string().contains("PRIVATE-FIXTURE-ONLY"));
        assert!(!prepared.view().to_string().contains("fixture_secret"));
        assert!(
            serde_json::to_string(&prepared)
                .unwrap()
                .contains("PRIVATE-FIXTURE-ONLY")
        );
    }

    #[test]
    fn apply_then_undo_restores_bytes_preserves_symlinks_and_private_receipts() {
        let tmp = tempfile::tempdir().unwrap();
        let config = old_codex(tmp.path());
        let original = std::fs::read(&config).unwrap();
        let real = tmp.path().join("actual-config.toml");
        std::fs::rename(&config, &real).unwrap();
        std::os::unix::fs::symlink(&real, &config).unwrap();
        let mut prepared = plan(tmp.path(), &["codex", "claude-code"]);
        let directory = directory(&tmp.path().join("state")).unwrap();
        save(&directory, &prepared).unwrap();
        apply(&directory, &mut prepared, false).unwrap();
        assert_eq!(prepared.phase, "applied");
        assert!(config.is_symlink());
        assert!(
            std::fs::read_to_string(&config)
                .unwrap()
                .starts_with(std::str::from_utf8(&original).unwrap())
        );
        assert!(tmp.path().join(".claude/settings.json").exists());
        assert!(
            !tmp.path().join(".claude.json").exists(),
            "guided hooks do not rewrite Claude's mutable application state"
        );
        let receipt = receipt(&directory, &prepared.id).unwrap();
        assert_eq!(
            receipt.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            directory.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        let mut loaded = load(&directory, &prepared.id).unwrap();
        apply(&directory, &mut loaded, true).unwrap();
        assert_eq!(loaded.phase, "undone");
        assert_eq!(std::fs::read(&config).unwrap(), original);
        assert!(config.is_symlink());
        assert!(!tmp.path().join(".claude/settings.json").exists());
        assert!(tmp.path().join(".claude").is_dir());
        assert!(
            apply(&directory, &mut loaded, false).is_err(),
            "undone receipts cannot reapply stale snapshots"
        );
    }

    #[test]
    fn changed_second_file_blocks_all_writes_and_undo_preserves_later_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let config = old_codex(tmp.path());
        let mut prepared = plan(tmp.path(), &["claude-code", "codex"]);
        let directory = directory(&tmp.path().join("state")).unwrap();
        std::fs::write(&config, "user changed config\n").unwrap();
        assert!(apply(&directory, &mut prepared, false).is_err());
        assert!(
            !tmp.path().join(".claude").exists(),
            "preflight of all files precedes any edits"
        );
        std::fs::write(&config, prepared.changes[1].before.as_ref().unwrap()).unwrap();
        apply(&directory, &mut prepared, false).unwrap();
        std::fs::write(&config, "new user changes\n").unwrap();
        assert!(apply(&directory, &mut prepared, true).is_err());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "new user changes\n"
        );
        assert!(tmp.path().join(".claude/settings.json").exists());
    }

    #[test]
    fn interrupted_apply_and_undo_resume_from_durable_before_after_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let config = old_codex(tmp.path());
        let original = std::fs::read(&config).unwrap();
        let mut prepared = plan(tmp.path(), &["codex", "claude-code"]);
        let directory = directory(&tmp.path().join("state")).unwrap();
        prepared.phase = "applying".into();
        save(&directory, &prepared).unwrap();
        let first = &prepared.changes[0];
        super::super::write_config(&first.path, first.before.as_deref(), &first.after).unwrap();
        let mut resumed = load(&directory, &prepared.id).unwrap();
        apply(&directory, &mut resumed, false).unwrap();
        assert_eq!(resumed.phase, "applied");
        resumed.phase = "undoing".into();
        save(&directory, &resumed).unwrap();
        let first = &resumed.changes[0];
        super::super::write_config(
            &first.path,
            Some(&first.after),
            first.before.as_ref().unwrap(),
        )
        .unwrap();
        let mut resumed = load(&directory, &prepared.id).unwrap();
        apply(&directory, &mut resumed, true).unwrap();
        assert_eq!(std::fs::read(&config).unwrap(), original);
        assert!(!tmp.path().join(".claude/settings.json").exists());
    }

    #[test]
    fn failed_receipt_write_and_missing_executable_prevent_configuration_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut prepared = plan(tmp.path(), &["claude-code"]);
        let bad_directory = tmp.path().join("not-a-directory");
        std::fs::write(&bad_directory, "obstruction").unwrap();
        assert!(apply(&bad_directory, &mut prepared, false).is_err());
        assert!(!tmp.path().join(".claude").exists());
        prepared.executable = tmp.path().join("removed-agentdocker");
        let directory = directory(&tmp.path().join("state")).unwrap();
        assert!(apply(&directory, &mut prepared, false).is_err());
        assert!(!tmp.path().join(".claude").exists());
    }

    #[test]
    fn symlink_retargeting_and_receipt_traversal_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let config = old_codex(tmp.path());
        let mut prepared = plan(tmp.path(), &["codex"]);
        let alternate = tmp.path().join("alternate.toml");
        std::fs::rename(&config, &alternate).unwrap();
        std::os::unix::fs::symlink(&alternate, &config).unwrap();
        let directory = directory(&tmp.path().join("state")).unwrap();
        assert!(apply(&directory, &mut prepared, false).is_err());
        assert!(receipt(&directory, "../../config").is_err());
        let id = uuid::Uuid::new_v4().to_string();
        std::os::unix::fs::symlink(&alternate, receipt(&directory, &id).unwrap()).unwrap();
        assert!(load(&directory, &id).is_err());
    }

    #[test]
    fn damaged_receipts_do_not_hide_healthy_saved_plans() {
        let tmp = tempfile::tempdir().unwrap();
        let prepared = plan(tmp.path(), &["claude-code"]);
        let directory = directory(&tmp.path().join("state")).unwrap();
        save(&directory, &prepared).unwrap();
        let malformed = receipt(&directory, &uuid::Uuid::new_v4().to_string()).unwrap();
        std::fs::write(&malformed, "partial json").unwrap();
        let incompatible = receipt(&directory, &uuid::Uuid::new_v4().to_string()).unwrap();
        let mut future = serde_json::to_value(&prepared).unwrap();
        future["format"] = json!(999);
        std::fs::write(&incompatible, future.to_string()).unwrap();
        let invalid_name = directory.join("not-a-plan-id.json");
        std::fs::write(&invalid_name, "{}").unwrap();
        let listed = list_plans(&directory).unwrap();
        assert_eq!(listed["plans"].as_array().unwrap().len(), 1);
        assert_eq!(listed["plans"][0]["id"], prepared.id);
        assert_eq!(listed["skipped_receipts"], 3);
        assert!(malformed.exists() && incompatible.exists() && invalid_name.exists());
    }

    #[test]
    fn prepared_and_completed_plans_do_not_claim_unrecorded_external_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut prepared = plan(tmp.path(), &["claude-code"]);
        let directory = directory(&tmp.path().join("state")).unwrap();
        let path = prepared.changes[0].path.clone();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &prepared.changes[0].after).unwrap();
        assert!(
            apply(&directory, &mut prepared, true).is_err(),
            "prepared plan never wrote this file"
        );
        std::fs::remove_file(&path).unwrap();
        apply(&directory, &mut prepared, false).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(
            apply(&directory, &mut prepared, false).is_err(),
            "a completed plan cannot reapply after external undo"
        );
    }
}
