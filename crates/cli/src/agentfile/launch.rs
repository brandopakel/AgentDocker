//! The command an Agentfile entry asks for when it says what it wants
//! (`prompt`, `model`, `effort`) rather than how to type it, and the
//! coordination it carries: the AgentDocker MCP server and, for Claude Code,
//! the hooks, added to this agent's launch only when the person's own
//! configuration does not already provide them.
//!
//! What each runtime gets is written here once, so a file says
//! `runtime = "codex"` and `prompt = "…"` instead of every flag.

use std::path::{Path, PathBuf};

use agentdocker_core::AgentSpec;
use serde_json::json;

use super::AgentEntry;

/// Runtimes whose command an entry can leave to `prompt`.
pub const BUILT: &[&str] = &["claude-code", "codex", "docker-agent", "gemini-cli"];

/// Claude Code's `--effort` levels.
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// Codex's `model_reasoning_effort` levels.
const CODEX_EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];

/// What building a command needs from the machine, so tests can supply a
/// machine of their own.
pub trait Host {
    /// An environment variable, for `${VAR}`.
    fn var(&self, name: &str) -> Option<String>;
    /// The runtime's command, absolute when it was found.
    fn program(&self, runtime: &str) -> Option<PathBuf>;
    /// This `agentdocker`, which the coordination adapters run.
    fn agentdocker(&self) -> anyhow::Result<PathBuf>;
    /// The runtime's own configuration already runs our MCP server.
    fn mcp_wired(&self, runtime: &str) -> bool;
    /// Some Claude Code settings a session in `workdir` loads already run
    /// our hooks: adding them again would run each one twice.
    fn claude_hooks_present(&self, workdir: &Path) -> bool;
}

/// What is wrong with an entry's launch fields, judged from the file alone.
pub fn check(name: &str, entry: &AgentEntry) -> Result<(), String> {
    let built = entry.command.is_empty();
    if !built {
        for (field, set) in [
            ("prompt", entry.prompt.is_some()),
            ("effort", entry.effort.is_some()),
            ("args", !entry.args.is_empty()),
            ("config", entry.config.is_some()),
            ("coordinate", entry.coordinate.is_some()),
        ] {
            if set {
                return Err(format!(
                    "agent `{name}` sets both `command` and `{field}`; `{field}` is for a command agentdocker builds, so remove `command` or put everything in it"
                ));
            }
        }
        if entry.idle_messages && !matches!(entry.runtime.as_str(), "claude-code" | "codex") {
            return Err(idle_unsupported(name, &entry.runtime));
        }
        return Ok(());
    }
    if !BUILT.contains(&entry.runtime.as_str()) {
        return Err(if entry.prompt.is_some() {
            format!(
                "agent `{name}`: agentdocker cannot build a command for runtime `{}` (it can for {}); write `command`",
                entry.runtime,
                BUILT.join(", ")
            )
        } else {
            format!(
                "agent `{name}` has no `command`; write one, or give `prompt` to a runtime agentdocker can launch ({})",
                BUILT.join(", ")
            )
        });
    }
    let terminal = entry.tty || entry.in_pane;
    let needs_prompt = |what: &str| {
        format!(
            "agent `{name}`: {what} without a terminal needs a `prompt`; give one, or set `tty = true` for an interactive session"
        )
    };
    match entry.runtime.as_str() {
        "claude-code" => {
            effort_in(name, entry, CLAUDE_EFFORTS)?;
            if !terminal && entry.prompt.is_none() {
                return Err(needs_prompt("Claude Code"));
            }
            if entry.idle_messages && !terminal {
                return Err(format!(
                    "agent `{name}`: idle messages reach an interactive Claude Code session; set `tty = true` or `in_pane = true`"
                ));
            }
        }
        "codex" => {
            effort_in(name, entry, CODEX_EFFORTS)?;
            if entry.idle_messages {
                if entry.prompt.is_some() {
                    return Err(format!(
                        "agent `{name}`: Codex with idle messages starts without a prompt on its command line; leave `prompt` out and send the first message with `agentdocker send --to {name} …` once it is up"
                    ));
                }
            } else if !terminal && entry.prompt.is_none() {
                return Err(needs_prompt("Codex"));
            }
        }
        "docker-agent" => {
            if entry.config.is_none() {
                return Err(format!(
                    "agent `{name}`: docker-agent needs `config`, the agent's YAML file or its registry reference"
                ));
            }
            if entry.effort.is_some() {
                return Err(format!(
                    "agent `{name}`: docker-agent takes effort in the agent's YAML (a harness's `effort`, a model's thinking budget), not on its command line"
                ));
            }
            if entry.idle_messages {
                return Err(idle_unsupported(name, &entry.runtime));
            }
            if !terminal && entry.prompt.is_none() {
                return Err(needs_prompt("a docker-agent run"));
            }
        }
        "gemini-cli" => {
            if entry.effort.is_some() {
                return Err(format!(
                    "agent `{name}`: Gemini CLI has no effort option on its command line"
                ));
            }
            if entry.idle_messages {
                return Err(idle_unsupported(name, &entry.runtime));
            }
            if !terminal && entry.prompt.is_none() {
                return Err(needs_prompt("Gemini CLI"));
            }
        }
        _ => unreachable!("BUILT is checked above"),
    }
    if entry.config.is_some() && entry.runtime != "docker-agent" {
        return Err(format!(
            "agent `{name}`: `config` names a docker-agent YAML file; runtime `{}` has none",
            entry.runtime
        ));
    }
    Ok(())
}

fn idle_unsupported(name: &str, runtime: &str) -> String {
    format!(
        "agent `{name}`: idle messages need an input adapter, which AgentDocker has for claude-code and codex, not `{runtime}`"
    )
}

fn effort_in(name: &str, entry: &AgentEntry, levels: &[&str]) -> Result<(), String> {
    match &entry.effort {
        Some(effort) if !levels.contains(&effort.as_str()) => Err(format!(
            "agent `{name}` has effort = \"{effort}\"; {} takes {}",
            entry.runtime,
            levels.join(", ")
        )),
        _ => Ok(()),
    }
}

/// Fill in `spec.command` (and its environment) for an entry that was
/// checked by [`check`] and expanded. `base` is the Agentfile's directory.
/// Returns notes for the person: what was not wired, and why.
pub fn apply(
    spec: &mut AgentSpec,
    entry: &AgentEntry,
    base: &Path,
    host: &dyn Host,
) -> anyhow::Result<Vec<String>> {
    let mut notes = Vec::new();
    let coordinate = entry.coordinate.unwrap_or(true);
    if entry.command.is_empty() {
        let program = |runtime: &str, fallback: &str| {
            host.program(runtime)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| fallback.to_owned())
        };
        let terminal = entry.tty || entry.in_pane;
        let mut command;
        match entry.runtime.as_str() {
            "claude-code" => {
                command = vec![program("claude-code", "claude")];
                if !terminal {
                    command.push("-p".into());
                }
                push_flag(&mut command, "--model", entry.model.as_deref());
                push_flag(&mut command, "--effort", entry.effort.as_deref());
                if coordinate {
                    let exe = host.agentdocker()?;
                    let exe_text = exe.to_string_lossy();
                    // The channel enabler adds its own MCP entry below.
                    if !entry.idle_messages && !host.mcp_wired("claude-code") {
                        command.push("--mcp-config".into());
                        command.push(
                            json!({"mcpServers": {"agentdocker": {
                                "type": "stdio", "command": exe_text,
                                "args": ["mcp", "--runtime", "claude-code"]
                            }}})
                            .to_string(),
                        );
                    }
                    let workdir = spec.workdir.clone().unwrap_or_else(|| base.to_owned());
                    if !host.claude_hooks_present(&workdir) {
                        let mut settings = json!({});
                        crate::hooks::merge_claude_code_hooks(
                            &mut settings,
                            &agentdocker_host::runtimes::claude_hook_command(&exe)?,
                        )?;
                        command.push("--settings".into());
                        command.push(settings.to_string());
                    }
                }
                command.extend(entry.args.iter().cloned());
                if let Some(prompt) = &entry.prompt {
                    command.extend(["--".to_owned(), prompt.clone()]);
                }
            }
            "codex" => {
                command = vec![program("codex", "codex")];
                if !terminal && !entry.idle_messages {
                    command.push("exec".into());
                }
                if let Some(model) = &entry.model {
                    command.extend(["-c".to_owned(), format!("model={}", json!(model))]);
                }
                if let Some(effort) = &entry.effort {
                    command.extend([
                        "-c".to_owned(),
                        format!("model_reasoning_effort={}", json!(effort)),
                    ]);
                }
                // The idle-input bridge serves its own MCP connection.
                if coordinate && !entry.idle_messages && !host.mcp_wired("codex") {
                    let exe = host.agentdocker()?;
                    command.extend([
                        "-c".to_owned(),
                        format!("mcp_servers.agentdocker.command={}", json!(exe)),
                        "-c".to_owned(),
                        r#"mcp_servers.agentdocker.args=["mcp","--runtime","codex"]"#.to_owned(),
                    ]);
                }
                command.extend(entry.args.iter().cloned());
                if let Some(prompt) = &entry.prompt {
                    command.extend(["--".to_owned(), prompt.clone()]);
                }
            }
            "docker-agent" => {
                let config = entry.config.as_deref().expect("checked");
                command = vec![
                    program("docker-agent", "docker-agent"),
                    "run".into(),
                    local_config(config, base),
                ];
                push_flag(&mut command, "--model", entry.model.as_deref());
                command.extend(entry.args.iter().cloned());
                if coordinate && let Some(note) = docker_agent_note(&spec.name, config, base) {
                    notes.push(note);
                }
                if let Some(prompt) = &entry.prompt {
                    command.extend(["--".to_owned(), prompt.clone()]);
                }
            }
            "gemini-cli" => {
                command = vec![program("gemini-cli", "gemini")];
                push_flag(&mut command, "--model", entry.model.as_deref());
                command.extend(entry.args.iter().cloned());
                if let Some(prompt) = &entry.prompt {
                    // `=` keeps a prompt that begins with `-` a value.
                    command.push(format!(
                        "--{}={prompt}",
                        if terminal {
                            "prompt-interactive"
                        } else {
                            "prompt"
                        }
                    ));
                }
                if coordinate && !host.mcp_wired("gemini-cli") {
                    notes.push(format!(
                        "{}: Gemini CLI reads MCP servers only from its settings file; `agentdocker setup gemini-cli` registers AgentDocker there",
                        spec.name
                    ));
                }
            }
            other => anyhow::bail!("no command builder for runtime `{other}`"),
        }
        spec.command = command;
    }
    if entry.idle_messages {
        let exe = host.agentdocker()?;
        match entry.runtime.as_str() {
            "claude-code" => agentdocker_host::provider_input::enable_claude_channel(spec, &exe)?,
            "codex" => agentdocker_host::provider_input::enable_codex_input(spec, &exe)?,
            other => anyhow::bail!(idle_unsupported(&spec.name, other)),
        }
    }
    Ok(notes)
}

fn push_flag(command: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        command.extend([flag.to_owned(), value.to_owned()]);
    }
}

/// A docker-agent `config` that names a file is made absolute against the
/// Agentfile's directory, as `workdir` is; a registry reference
/// (`docker.io/me/agent:1`, `myorg/agent`) is passed as written.
fn local_config(config: &str, base: &Path) -> String {
    let path = Path::new(config);
    let looks_local = config.ends_with(".yaml")
        || config.ends_with(".yml")
        || config.ends_with(".hcl")
        || config.starts_with('.')
        || path.is_absolute()
        || base.join(path).is_file();
    if looks_local && !path.is_absolute() {
        let joined = base.join(path);
        joined
            .canonicalize()
            .unwrap_or(joined)
            .to_string_lossy()
            .into_owned()
    } else {
        config.to_owned()
    }
}

/// docker-agent reads its MCP servers from the agent's YAML; a file that
/// never mentions `agentdocker` cannot be coordinating through it.
fn docker_agent_note(name: &str, config: &str, base: &Path) -> Option<String> {
    let path = PathBuf::from(local_config(config, base));
    let text = std::fs::read_to_string(&path).ok()?;
    (!text.contains("agentdocker")).then(|| {
        format!(
            "{name}: {} lists no `agentdocker mcp` toolset, so the run is supervised but cannot claim or message; `agentdocker setup docker-agent` prints the toolset to add",
            path.display()
        )
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A machine where nothing is wired and every CLI is on PATH.
    pub struct Fake {
        pub vars: BTreeMap<&'static str, &'static str>,
        pub wired: bool,
        /// A real file, where an input enabler insists on one.
        pub exe: Option<PathBuf>,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                vars: BTreeMap::from([("MODEL", "opus"), ("TASK", "the parser")]),
                wired: false,
                exe: None,
            }
        }
    }

    impl Host for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).map(|v| (*v).to_owned())
        }
        fn program(&self, runtime: &str) -> Option<PathBuf> {
            Some(PathBuf::from(format!("/bin/{runtime}")))
        }
        fn agentdocker(&self) -> anyhow::Result<PathBuf> {
            Ok(self
                .exe
                .clone()
                .unwrap_or_else(|| PathBuf::from("/opt/ad/agentdocker")))
        }
        fn mcp_wired(&self, _: &str) -> bool {
            self.wired
        }
        fn claude_hooks_present(&self, _: &Path) -> bool {
            self.wired
        }
    }
}
