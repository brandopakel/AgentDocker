//! `agentdocker setup`: wire AgentDocker into the agent tools installed on
//! this machine — the MCP server registered with each runtime that takes
//! one, hooks installed for Claude Code — idempotently, with a backup of
//! every file it changes.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use agentdocker_core::runtime::{McpWiring, RuntimeInfo, Wiring, spec};
use agentdocker_core::{Request, Response};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::client::Client;
use crate::hooks::{Host, InstallArgs, install_hooks};

/// What one step of setup did, or would do.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Added,
    Present,
    Planned,
}

pub async fn run(client: &Client, names: &[String], dry_run: bool) -> Result<()> {
    let inventory = match client.call(&Request::Runtimes).await? {
        Response::Runtimes { runtimes } => runtimes,
        other => bail!("unexpected reply to runtimes: {other:?}"),
    };
    let targets: Vec<&RuntimeInfo> = if names.is_empty() {
        inventory.iter().filter(|r| r.installed()).collect()
    } else {
        names
            .iter()
            .map(|name| {
                inventory.iter().find(|r| r.name == *name).with_context(|| {
                    format!("unknown runtime `{name}`; see `agentdocker runtimes`")
                })
            })
            .collect::<Result<_>>()?
    };
    if targets.is_empty() {
        println!("no agent tools found on this machine; see `agentdocker runtimes`");
        return Ok(());
    }
    let exe = std::env::current_exe().context("cannot locate the agentdocker binary")?;
    let roots = agentdocker_host::runtimes::Roots::from_env();
    let home = &roots.home;
    for runtime in targets {
        let Some(spec) = spec(&runtime.name) else {
            continue;
        };
        match (
            spec.mcp,
            agentdocker_host::runtimes::mcp_wiring(spec, &roots, "agentdocker"),
        ) {
            (_, Wiring::Wired) => println!("{}: MCP server already registered", runtime.name),
            (McpWiring::None, _) => println!(
                "{}: no MCP registration known for it; point it at `{} mcp --runtime {}` by hand",
                runtime.name,
                exe.display(),
                runtime.name
            ),
            (McpWiring::JsonServers { .. }, _) => {
                let path =
                    agentdocker_host::runtimes::mcp_config_path(spec, &roots).expect("JSON path");
                let outcome = if runtime.name == "claude-code" && runtime.cli.is_some() {
                    // Claude Code keeps its own state in that file; let it
                    // write the entry itself rather than rewrite the file.
                    register_with_claude_cli(runtime.cli.as_deref().unwrap(), &exe, dry_run)?
                } else {
                    register_json(&path, &exe, &runtime.name, dry_run)?
                };
                report(&runtime.name, "MCP server", &path, outcome);
            }
            (McpWiring::TomlServers { .. }, _) => {
                let path =
                    agentdocker_host::runtimes::mcp_config_path(spec, &roots).expect("TOML path");
                let outcome = register_toml(&path, &exe, &runtime.name, dry_run)?;
                report(&runtime.name, "MCP server", &path, outcome);
            }
        }
        if spec.hooks {
            match runtime.hooks {
                Wiring::Wired => println!("{}: hooks already installed", runtime.name),
                _ if dry_run => println!(
                    "{}: would install hooks in {}",
                    runtime.name,
                    home.join(".claude/settings.json").display()
                ),
                _ => {
                    back_up(&home.join(".claude/settings.json"))?;
                    install_hooks(&InstallArgs {
                        host: Host::ClaudeCode,
                        user: true,
                    })?;
                }
            }
        }
    }
    Ok(())
}

fn report(runtime: &str, what: &str, path: &Path, outcome: Outcome) {
    match outcome {
        Outcome::Added => println!("{runtime}: registered the {what} in {}", path.display()),
        Outcome::Present => println!("{runtime}: {what} already registered in {}", path.display()),
        Outcome::Planned => println!("{runtime}: would register the {what} in {}", path.display()),
    }
}

/// The MCP entry every runtime gets: this binary, as its own runtime.
fn entry(exe: &Path, runtime: &str) -> Value {
    json!({
        "command": exe.to_string_lossy(),
        "args": ["mcp", "--runtime", runtime],
    })
}

/// Whether a JSON MCP server entry runs this binary: by the command it
/// runs or an argument it passes, never by text that merely names us.
fn runs_agentdocker(server: &Value) -> bool {
    let command = server.get("command").and_then(Value::as_str);
    let args: Vec<_> = server
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    agentdocker_host::runtimes::mcp_command_matches(command, &args, "agentdocker")
        && server.get("disabled").and_then(Value::as_bool) != Some(true)
        && server.get("enabled").and_then(Value::as_bool) != Some(false)
}

/// Keep the previous version of a file we are about to rewrite.
fn back_up(path: &Path) -> Result<()> {
    let contents = match std::fs::read(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e).with_context(|| format!("cannot read {} for backup", path.display()));
        }
    };
    for index in 0..1000 {
        let extension = if index == 0 {
            "agentdocker-backup".into()
        } else {
            format!("agentdocker-backup.{index}")
        };
        let backup = path.with_extension(extension);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup)
        {
            Ok(mut file) => {
                file.write_all(&contents)?;
                file.sync_all()?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("cannot create backup {}", backup.display()));
            }
        }
    }
    bail!("too many setup backups for {}", path.display())
}

/// Replace a complete configuration atomically, retaining symlink targets and
/// refusing a file changed since it was read for this setup operation.
fn write_config(path: &Path, expected: Option<&str>, contents: &str) -> Result<()> {
    let unchanged = || -> Result<()> {
        let current = match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if current.as_deref() != expected {
            bail!(
                "{} changed during setup; retry after reviewing it",
                path.display()
            );
        }
        Ok(())
    };
    unchanged()?;
    let target = agentdocker_host::project::try_canonical(path)?;
    let parent = target.parent().context("configuration has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(metadata) = std::fs::metadata(&target) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file().sync_all()?;
    back_up(path)?;
    unchanged()?;
    if agentdocker_host::project::try_canonical(path)? != target {
        bail!("configuration symlink changed during setup");
    }
    temporary.persist(&target).map_err(|e| e.error)?;
    Ok(())
}

/// `mcpServers.agentdocker` in a JSON configuration file, created when
/// missing, left alone when present.
pub fn register_json(path: &Path, exe: &Path, runtime: &str, dry_run: bool) -> Result<Outcome> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut document: Value = match existing.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(raw) => serde_json::from_str(raw)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        None => json!({}),
    };
    let Some(root) = document.as_object_mut() else {
        bail!("{} is not a JSON object", path.display());
    };
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        bail!("{}: mcpServers is not an object", path.display());
    };
    // Ours by key, or by what it runs: either way there is nothing to add,
    // and a second entry must never clobber a key that exists.
    if let Some(existing) = servers.get("agentdocker") {
        if !runs_agentdocker(existing) {
            bail!(
                "{}: reserved agentdocker entry is disabled or unverified; left unchanged",
                path.display()
            );
        }
    }
    if servers.values().any(runs_agentdocker) {
        return Ok(Outcome::Present);
    }
    if dry_run {
        return Ok(Outcome::Planned);
    }
    servers.insert("agentdocker".to_owned(), entry(exe, runtime));
    write_config(
        path,
        existing.as_deref(),
        &format!("{}\n", serde_json::to_string_pretty(&document)?),
    )
    .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(Outcome::Added)
}

/// `[mcp_servers.agentdocker]` appended to a TOML configuration file, so
/// the rest of the file stays byte for byte as it was.
pub fn register_toml(path: &Path, exe: &Path, runtime: &str, dry_run: bool) -> Result<Outcome> {
    let original = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let existing = original.as_deref().unwrap_or("");
    let table: toml::Table = existing
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;
    if table.get("mcp_servers").is_some_and(|v| !v.is_table()) {
        bail!("{}: mcp_servers is not a table", path.display());
    }
    if let Some(servers) = table.get("mcp_servers").and_then(|v| v.as_table()) {
        let runs = |server: &toml::Value| {
            let command = server.get("command").and_then(|v| v.as_str());
            let args: Vec<_> = server
                .get("args")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
                .collect();
            server.get("enabled").and_then(|v| v.as_bool()) != Some(false)
                && agentdocker_host::runtimes::mcp_command_matches(command, &args, "agentdocker")
        };
        if servers
            .get("agentdocker")
            .is_some_and(|server| !runs(server))
        {
            bail!(
                "{}: reserved agentdocker entry is disabled or unverified; left unchanged",
                path.display()
            );
        }
        if servers.values().any(runs) {
            return Ok(Outcome::Present);
        }
    }
    let mut block = toml::Table::new();
    block.insert(
        "command".into(),
        toml::Value::String(exe.to_string_lossy().into_owned()),
    );
    block.insert(
        "args".into(),
        toml::Value::Array(
            ["mcp", "--runtime", runtime]
                .iter()
                .map(|s| toml::Value::String((*s).to_owned()))
                .collect(),
        ),
    );
    let mut appended = existing.to_owned();
    if !appended.is_empty() && !appended.ends_with('\n') {
        appended.push('\n');
    }
    if !appended.is_empty() {
        appended.push('\n');
    }
    appended.push_str("[mcp_servers.agentdocker]\n");
    appended.push_str(&toml::to_string(&block)?);
    appended
        .parse::<toml::Table>()
        .context("MCP registration would produce invalid TOML; configuration unchanged")?;
    if dry_run {
        return Ok(Outcome::Planned);
    }
    write_config(path, original.as_deref(), &appended)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(Outcome::Added)
}

/// `claude mcp add`, user scope: Claude Code writes its own configuration.
fn register_with_claude_cli(cli: &Path, exe: &Path, dry_run: bool) -> Result<Outcome> {
    if dry_run {
        return Ok(Outcome::Planned);
    }
    let output = std::process::Command::new(cli)
        .args(["mcp", "add", "--scope", "user", "agentdocker", "--"])
        .arg(exe)
        .args(["mcp", "--runtime", "claude-code"])
        .output()
        .with_context(|| format!("cannot run {}", cli.display()))?;
    if output.status.success() {
        return Ok(Outcome::Added);
    }
    let text = String::from_utf8_lossy(&output.stderr);
    if text.contains("already exists") {
        return Ok(Outcome::Present);
    }
    bail!("`claude mcp add` failed: {}", text.trim());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_edits_detect_changes_preserve_links_and_keep_private_backups() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        let real = tmp.path().join("real.json");
        std::fs::write(&real, "original").unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        write_config(&path, Some("original"), "updated").unwrap();
        assert!(path.is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "updated");
        let backup = path.with_extension("agentdocker-backup");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "original");
        assert_eq!(
            std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_config(&path, Some("original"), "stale update").is_err());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "updated");
        write_config(&path, Some("updated"), "second").unwrap();
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "original",
            "first backup is never overwritten"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_extension("agentdocker-backup.1")).unwrap(),
            "updated"
        );
    }

    #[test]
    fn disabled_or_malformed_mcp_entries_are_preserved_with_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        for original in [
            "mcp_servers = 7\n",
            "[mcp_servers.agentdocker]\ncommand = \"agentdocker\"\nargs = [\"mcp\"]\nenabled = false\n",
        ] {
            std::fs::write(&path, original).unwrap();
            for dry_run in [true, false] {
                assert!(
                    register_toml(&path, Path::new("/opt/agentdocker"), "codex", dry_run).is_err()
                );
                assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            }
        }
    }

    #[test]
    fn json_registration_is_idempotent_backed_up_and_dry_runnable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/mcp.json");
        let exe = Path::new("/opt/agentdocker");
        assert_eq!(
            register_json(&path, exe, "cursor", true).unwrap(),
            Outcome::Planned
        );
        assert!(!path.exists(), "dry run writes nothing");
        assert_eq!(
            register_json(&path, exe, "cursor", false).unwrap(),
            Outcome::Added
        );
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc["mcpServers"]["agentdocker"]["command"],
            "/opt/agentdocker"
        );
        assert_eq!(doc["mcpServers"]["agentdocker"]["args"][2], "cursor");
        assert_eq!(
            register_json(&path, exe, "cursor", false).unwrap(),
            Outcome::Present
        );

        let other = tmp.path().join("settings.json");
        std::fs::write(
            &other,
            r#"{"theme":"dark","mcpServers":{"x":{"command":"y"}}}"#,
        )
        .unwrap();
        assert_eq!(
            register_json(&other, exe, "gemini-cli", false).unwrap(),
            Outcome::Added
        );
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&other).unwrap()).unwrap();
        assert_eq!(doc["theme"], "dark", "the rest of the file survives");
        assert_eq!(doc["mcpServers"]["x"]["command"], "y");
        assert!(other.with_extension("agentdocker-backup").exists());
        // An entry under our key that runs something else is still the
        // user's: leave it, rather than overwrite it.
        let keyed = tmp.path().join("keyed.json");
        std::fs::write(
            &keyed,
            r#"{"mcpServers":{"agentdocker":{"command":"/my/wrapper"}}}"#,
        )
        .unwrap();
        assert!(register_json(&keyed, exe, "cursor", false).is_err());
        assert_eq!(
            std::fs::read_to_string(&keyed).unwrap(),
            r#"{"mcpServers":{"agentdocker":{"command":"/my/wrapper"}}}"#
        );
        // A name in some other field is a mention, not a registration.
        let mention = tmp.path().join("mention.json");
        std::fs::write(
            &mention,
            r#"{"mcpServers":{"notes":{"command":"cat","env":{"NOTE":"agentdocker"}}}}"#,
        )
        .unwrap();
        assert_eq!(
            register_json(&mention, exe, "cursor", false).unwrap(),
            Outcome::Added
        );
        assert!(register_json(&tmp.path().join("bad.json"), exe, "x", false).is_ok());
        std::fs::write(tmp.path().join("list.json"), "[]").unwrap();
        assert!(register_json(&tmp.path().join("list.json"), exe, "x", false).is_err());
    }

    #[test]
    fn toml_registration_appends_and_keeps_the_file_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let original = "[projects.\"/x\"]\ntrust_level = \"trusted\"\n";
        std::fs::write(&path, original).unwrap();
        let exe = Path::new("/opt/agentdocker");
        assert_eq!(
            register_toml(&path, exe, "codex", true).unwrap(),
            Outcome::Planned
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(
            register_toml(&path, exe, "codex", false).unwrap(),
            Outcome::Added
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with(original),
            "existing text untouched:\n{text}"
        );
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(
            table["mcp_servers"]["agentdocker"]["command"].as_str(),
            Some("/opt/agentdocker")
        );
        assert_eq!(
            table["mcp_servers"]["agentdocker"]["args"][1].as_str(),
            Some("--runtime")
        );
        assert_eq!(
            register_toml(&path, exe, "codex", false).unwrap(),
            Outcome::Present
        );
        assert!(path.with_extension("agentdocker-backup").exists());
        // A table under our key is never appended twice: that is not even
        // valid TOML.
        let keyed = tmp.path().join("keyed.toml");
        std::fs::write(
            &keyed,
            "[mcp_servers.agentdocker]\ncommand = \"/my/wrapper\"\n",
        )
        .unwrap();
        assert!(register_toml(&keyed, exe, "codex", false).is_err());
        assert_eq!(
            std::fs::read_to_string(&keyed).unwrap(),
            "[mcp_servers.agentdocker]\ncommand = \"/my/wrapper\"\n"
        );
        let mention = tmp.path().join("mention.toml");
        std::fs::write(
            &mention,
            "[mcp_servers.notes]\ncommand = \"cat\"\nenv = { NOTE = \"agentdocker\" }\n",
        )
        .unwrap();
        assert_eq!(
            register_toml(&mention, exe, "codex", false).unwrap(),
            Outcome::Added
        );
        let fresh = tmp.path().join("new/config.toml");
        assert_eq!(
            register_toml(&fresh, exe, "codex", false).unwrap(),
            Outcome::Added
        );
        assert!(fresh.exists());
    }
}
