//! Saved setup plans contain private before/after snapshots, never printed.
//! Each file is replaced atomically. A multi-file plan can be resumed after
//! interruption; it never claims a filesystem-wide transaction.
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::runtime::{McpWiring, RUNTIMES, RuntimeInfo, RuntimeSpec, Wiring};
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

/// A registration only the provider's own tool can safely make.
///
/// Claude Code keeps its MCP servers in `~/.claude.json`, which is also
/// where it keeps its live application state — every project it has
/// opened, the account it is signed in as, what it has already told you
/// — and it rewrites that file throughout a session. Planning a
/// byte-for-byte replacement of it would mean a preflight that fails
/// whenever Claude Code has written since the preview, which is nearly
/// always, and because preflight covers every file before any edit it
/// would take the hooks change down with it.
///
/// So the plan carries the command instead of the bytes: `claude mcp
/// add` makes the entry and `claude mcp remove` takes it away, and
/// Claude Code stays the only writer of its own file. It is still
/// previewable and still undoable; what it is not is transactional with
/// the file edits, and a note in the plan says so.
#[derive(Debug, Serialize, Deserialize)]
struct Delegated {
    runtime: String,
    channel: String,
    /// The file the provider will write. Shown, never rewritten by us.
    path: PathBuf,
    /// Argv that makes the registration, and argv that removes it.
    add: Vec<String>,
    remove: Vec<String>,
    /// Whether the apply is what made this registration, and so whether
    /// an undo may take it back. Written when the step runs; `false`
    /// until then, and `false` for a registration that was already there
    /// — which belongs to whoever made it, not to this plan.
    #[serde(default)]
    created: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Plan {
    format: u32,
    id: String,
    phase: String,
    executable: PathBuf,
    changes: Vec<Change>,
    /// Absent from receipts written before delegated steps existed.
    #[serde(default)]
    delegated: Vec<Delegated>,
    notes: Vec<String>,
}

impl Plan {
    /// Deliberately separate from serialization of the private snapshots.
    fn view(&self) -> Value {
        let changes = self
            .changes
            .iter()
            .map(|change| {
                json!({
                    "runtime":change.runtime, "channel":change.channel, "path":change.path,
                    "action": if change.before.is_some() {"add to existing configuration"} else {"create configuration"}
                })
            })
            // Listed among the file changes because they are the same
            // thing to the reader — one more registration this plan
            // makes and can take back — and because a window that only
            // counted file changes would grey out Apply on a plan whose
            // one remaining step is this.
            .chain(self.delegated.iter().map(|step| {
                json!({
                    "runtime":step.runtime, "channel":step.channel, "path":step.path,
                    "action": format!("register through `{}`", step.add.join(" "))
                })
            }))
            .collect::<Vec<_>>();
        json!({"id": self.id, "phase": self.phase, "executable": self.executable,
            "changes": changes, "notes": self.notes})
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

fn selected_inventory(roots: &Roots, names: &[String]) -> Result<Vec<RuntimeInfo>> {
    if names.is_empty() {
        return Ok(runtimes::inventory(roots, "agentdocker")?);
    }
    names
        .iter()
        .map(|name| {
            let spec = agentdocker_core::runtime::spec(name)
                .with_context(|| format!("unknown runtime `{name}`"))?;
            Ok(runtimes::inspect(spec, roots, "agentdocker")?)
        })
        .collect()
}

/// How long one delegated registration may take.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);

/// The MCP registration a runtime has to make for itself, if any.
///
/// Only Claude Code, for the reason [`Delegated`] gives, and only when
/// there is something to do and something to do it with:
///
/// - no `claude` on the machine and there is nobody to delegate to, so
///   the ordinary file edit stands;
/// - already wired and there is nothing to add;
/// - anything other than a plain absence — a registration under our name
///   that runs something else, or a file we could not parse — is for a
///   person to look at, not for `claude mcp add` to walk into.
fn delegated_mcp(
    spec: &RuntimeSpec,
    runtime: &RuntimeInfo,
    roots: &Roots,
    executable: &Path,
) -> Result<Option<Delegated>> {
    if spec.name != "claude-code" || runtime.mcp != Wiring::Missing {
        return Ok(None);
    }
    let Some(cli) = runtime.cli.as_deref() else {
        return Ok(None);
    };
    let path = runtimes::mcp_config_path(spec, roots)
        .context("Claude Code has no MCP configuration path")?;
    let cli = cli
        .to_str()
        .context("the Claude Code CLI path is not UTF-8")?
        .to_owned();
    let executable = executable
        .to_str()
        .context("the agentdocker path is not UTF-8")?
        .to_owned();
    let mut add: Vec<String> = vec![cli.clone()];
    add.extend(["mcp", "add", "--scope", "user", "agentdocker", "--"].map(str::to_owned));
    add.push(executable);
    add.extend(["mcp", "--runtime", "claude-code"].map(str::to_owned));
    let mut remove: Vec<String> = vec![cli];
    remove.extend(["mcp", "remove", "--scope", "user", "agentdocker"].map(str::to_owned));
    Ok(Some(Delegated {
        runtime: runtime.name.clone(),
        channel: "mcp".into(),
        path,
        add,
        remove,
        created: false,
    }))
}

/// Whether the provider's configuration registers AgentDocker right now.
fn registers_us(step: &Delegated) -> Result<bool> {
    Ok(read_config(&step.path)?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|config| config.get("mcpServers")?.get("agentdocker").cloned())
        .is_some_and(|server| super::runs_agentdocker(&server, &step.runtime)))
}

fn run_step(argv: &[String]) -> Result<agentdocker_host::command::Output> {
    agentdocker_host::command::run(&std::env::current_dir()?, argv, REGISTER_TIMEOUT)
        .with_context(|| format!("cannot run {}", argv.first().map_or("", String::as_str)))
}

/// Make the registration, and say whether *this plan* is what made it.
///
/// The answer matters more than it looks. A registration that was
/// already there when the apply ran belongs to whoever put it there —
/// the person, or Claude Code itself, between the preview and the apply
/// — and an undo that removed it would be exactly the mistake the file
/// steps refuse to make when a file has changed under them. So the plan
/// records what it created and takes back only that.
///
/// `claude mcp add` exits non-zero when the entry is already present,
/// which is not a failure of the apply: the configuration is what the
/// plan asked for. The file, not the exit status, says whether the step
/// arrived; the exit status only decides whose it is.
fn register(step: &Delegated) -> Result<bool> {
    if registers_us(step)? {
        return Ok(false); // already there before we ran: not ours to undo
    }
    let output = run_step(&step.add)?;
    ensure!(
        registers_us(step)?,
        "`{}` failed and {} still does not register the AgentDocker MCP server: {}",
        step.add.join(" "),
        step.path.display(),
        output.text.trim()
    );
    // Registered now and not before, so this plan did it — unless the
    // command failed and something else registered it in the meantime,
    // which is not ours either.
    Ok(output.success)
}

/// Take back a registration this plan made.
fn deregister(step: &Delegated) -> Result<()> {
    let output = run_step(&step.remove)?;
    ensure!(
        !registers_us(step)?,
        "`{}` failed and {} still registers the AgentDocker MCP server: {}",
        step.remove.join(" "),
        step.path.display(),
        output.text.trim()
    );
    Ok(())
}

/// Plan edits from injectable provider roots, without changing their files.
fn prepare(roots: &Roots, names: &[String], executable: &Path) -> Result<Plan> {
    let inventory = selected_inventory(roots, names)?;
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
        delegated: Vec::new(),
        notes: Vec::new(),
    };
    for runtime in targets {
        let spec = RUNTIMES
            .iter()
            .find(|spec| spec.name == runtime.name)
            .context("missing runtime specification")?;
        if let Some(step) = delegated_mcp(spec, runtime, roots, executable)? {
            plan.delegated.push(step);
        }
        // Hooks cover the Claude Code lifecycle without rewriting its mutable
        // .claude.json application state or installing a duplicate MCP identity;
        // `delegated_mcp` above is what registers the server there instead.
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
    for step in &plan.delegated {
        plan.notes.push(format!(
            "{}: the {} registration is made by the provider's own tool, because {} is its live \
             application state and only it should write there. That step runs after the file \
             changes and is undone by the matching remove, not by restoring bytes.",
            step.runtime,
            step.channel,
            step.path.display()
        ));
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
    // After the file changes, so a plan that cannot pass its own
    // preflight never reaches the provider's tool. Undo takes back only
    // what the apply recorded itself as having made: a registration that
    // was already there when we ran is somebody else's.
    for step in &mut plan.delegated {
        if undo {
            if step.created {
                deregister(step)?;
                step.created = false;
            }
        } else {
            step.created = register(step)?;
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
        let inventory = selected_inventory(&roots, names)?;
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
                , "checks":agentdocker_core::runtime::RUNTIMES.iter()
                    .find(|spec| spec.name == runtime.name)
                    .map(|spec| runtimes::health::inspect(spec, &roots, "agentdocker"))
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
            prepare(&roots, names, &crate::desktop::setup_executable()?)?
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
            install_dirs: vec![],
            desktop_dirs: vec![],
            versions: false,
        }
    }

    /// A `claude` on the injected PATH, so the planner has something to
    /// delegate to without a real Claude Code on the machine.
    fn fake_claude(home: &Path) -> (Roots, PathBuf) {
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut roots = roots(home);
        roots.path.push(bin);
        (roots, claude)
    }

    /// Claude Code's MCP registration is delegated to its own CLI, and
    /// `.claude.json` is never one of the files the plan rewrites.
    ///
    /// The window used to report that registration missing — correctly,
    /// `--health` reads the same file the provider does — while the
    /// guided preview it offered beside the fault planned only hooks and
    /// so could never clear it. A screen that names a fault it has no
    /// way to fix is worse than one that names nothing.
    #[test]
    fn claude_code_mcp_is_delegated_to_its_own_cli_and_never_rewrites_claude_json() {
        let temp = tempfile::tempdir().unwrap();
        let (roots, claude) = fake_claude(temp.path());
        let plan = prepare(
            &roots,
            &["claude-code".into()],
            &std::env::current_exe().unwrap(),
        )
        .unwrap();

        assert_eq!(plan.delegated.len(), 1);
        let step = &plan.delegated[0];
        assert_eq!(
            (step.channel.as_str(), &step.path),
            ("mcp", &temp.path().join(".claude.json"))
        );
        let add = step.add.join(" ");
        assert!(add.starts_with(claude.to_str().unwrap()), "{add}");
        assert!(add.contains("mcp add --scope user agentdocker --"), "{add}");
        assert!(add.ends_with("mcp --runtime claude-code"), "{add}");
        assert!(
            step.remove
                .join(" ")
                .ends_with("mcp remove --scope user agentdocker")
        );

        // Whatever else the plan does, it does not write that file.
        assert!(!plan.changes.is_empty(), "the hooks are still planned");
        for change in &plan.changes {
            assert_eq!(change.channel, "hooks");
            assert_ne!(change.path, step.path);
        }
        // The reader sees it among the changes all the same, which is
        // also what keeps Apply live on a plan whose only step is this.
        let view = plan.view();
        let changes = view["changes"].as_array().unwrap();
        assert_eq!(changes.len(), plan.changes.len() + 1);
        assert!(
            changes.iter().any(|change| change["channel"] == "mcp"
                && change["action"]
                    .as_str()
                    .is_some_and(|action| action.contains("mcp add"))),
            "{changes:?}"
        );
        assert!(view["notes"].to_string().contains("live application state"));
    }

    /// Nothing is delegated when there is nothing to add, and a provider
    /// tool that refuses because the work is already done has not failed.
    #[test]
    fn a_delegated_step_is_skipped_when_wired_and_settles_for_the_state_it_wanted() {
        let temp = tempfile::tempdir().unwrap();
        let (roots, _) = fake_claude(temp.path());
        let registered = temp.path().join(".claude.json");
        std::fs::write(
            &registered,
            r#"{"mcpServers":{"agentdocker":{"command":"/opt/agentdocker","args":["mcp","--runtime","claude-code"]}}}"#,
        )
        .unwrap();
        let plan = prepare(
            &roots,
            &["claude-code".into()],
            &std::env::current_exe().unwrap(),
        )
        .unwrap();
        assert!(plan.delegated.is_empty(), "already wired; nothing to add");

        // `claude mcp add` exits non-zero when the entry is already
        // there. That is not a failure of the apply — the configuration
        // is what the plan asked for — but it is also not this plan's
        // registration, and saying otherwise is how an undo comes to
        // delete somebody else's.
        let refuses = ["/bin/sh", "-c", "exit 1"].map(str::to_owned).to_vec();
        let mut step = Delegated {
            runtime: "claude-code".into(),
            channel: "mcp".into(),
            path: registered.clone(),
            add: refuses.clone(),
            remove: refuses,
            created: false,
        };
        assert!(
            !register(&step).unwrap(),
            "a registration that was already there is not ours"
        );
        // Removing one is a real failure when the entry survives it.
        let error = deregister(&step).unwrap_err().to_string();
        assert!(error.contains("still registers"), "{error}");
        // And an add that leaves nothing registered is a real failure.
        step.path = temp.path().join("absent.json");
        let error = register(&step).unwrap_err().to_string();
        assert!(error.contains("does not register"), "{error}");
        // A removal that leaves the entry gone has done its job,
        // whatever the provider's tool made of it.
        step.path = registered.clone();
        std::fs::write(&registered, "{}").unwrap();
        deregister(&step).unwrap();
    }

    /// An undo takes back only the registration the apply itself made.
    ///
    /// The file steps refuse to overwrite a file that changed under
    /// them; a delegated step has no bytes to compare, so it records
    /// whose registration it is instead. Without that, somebody adding
    /// the same entry between the preview and the apply loses it to the
    /// undo of a plan that never created it.
    #[test]
    fn an_undo_removes_only_what_the_apply_registered() {
        let temp = tempfile::tempdir().unwrap();
        let (roots, _) = fake_claude(temp.path());
        let claude_json = temp.path().join(".claude.json");
        // A stand-in for `claude mcp add|remove`, writing the file the
        // real one writes.
        let script = temp.path().join("bin/claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh
case \"$2\" in
  add) printf '{{\"mcpServers\":{{\"agentdocker\":                 {{\"command\":\"agentdocker\",\"args\":[\"mcp\",\"--runtime\",                 \"claude-code\"]}}}}}}' > {json} ;;
  remove) printf '{{}}' > {json} ;;
                 esac
",
                json = claude_json.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let directory = directory(&temp.path().join("state")).unwrap();
        let exe = std::env::current_exe().unwrap();

        // The ordinary case: we make it, so we may take it back.
        let mut ours = prepare(&roots, &["claude-code".into()], &exe).unwrap();
        save(&directory, &ours).unwrap();
        apply(&directory, &mut ours, false).unwrap();
        assert!(ours.delegated[0].created, "the apply made this one");
        assert!(registers_us(&ours.delegated[0]).unwrap());
        apply(&directory, &mut ours, true).unwrap();
        assert!(
            !registers_us(&ours.delegated[0]).unwrap(),
            "and took it back"
        );

        // The case that used to lose somebody's work: the registration
        // appears between the preview and the apply.
        let mut theirs = prepare(&roots, &["claude-code".into()], &exe).unwrap();
        assert_eq!(theirs.delegated.len(), 1);
        run_step(&theirs.delegated[0].add.clone()).unwrap();
        assert!(registers_us(&theirs.delegated[0]).unwrap());
        save(&directory, &theirs).unwrap();
        apply(&directory, &mut theirs, false).unwrap();
        assert!(
            !theirs.delegated[0].created,
            "it was there before the apply ran"
        );
        apply(&directory, &mut theirs, true).unwrap();
        assert!(
            registers_us(&theirs.delegated[0]).unwrap(),
            "the undo left a registration this plan never made"
        );
    }

    #[test]
    fn explicit_setup_is_not_blocked_by_an_unrelated_invalid_desktop_entry() {
        let temp = tempfile::tempdir().unwrap();
        let mut roots = roots(temp.path());
        let applications = temp.path().join("applications");
        std::fs::create_dir(&applications).unwrap();
        std::fs::write(applications.join("code.desktop"), "invalid launcher").unwrap();
        roots.desktop_dirs.push(applications);
        assert!(selected_inventory(&roots, &[]).is_err());
        let plan = prepare(&roots, &["codex".into()], &std::env::current_exe().unwrap()).unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].runtime, "codex");
        assert!(selected_inventory(&roots, &["unknown-provider".into()]).is_err());
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
