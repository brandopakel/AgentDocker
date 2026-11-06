//! The command an Agentfile entry asks for when it says what it wants
//! (`prompt`, `model`, `effort`, `config`) rather than how to type it, and
//! the coordination it carries: the AgentDocker MCP server and, for Claude
//! Code, the hooks, added to this one launch only where the person's own
//! configuration does not already provide them.
//!
//! What each runtime gets is written here once, so a file says
//! `runtime = "codex"` and `prompt = "…"` instead of every flag. Judging a
//! file ([`check`]) needs nothing from the machine; building the command
//! ([`apply`]) reads it through a [`Host`], so a test supplies a machine of
//! its own and asserts the exact argument vector. What the machine is asked
//! is asked of the entry's own [`Destination`]: the environment and the
//! checkout the agent will run in, not only the one `up` runs in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agentdocker_core::AgentSpec;
use agentdocker_core::model_choice::{CLAUDE_EFFORTS, CODEX_EFFORTS, EFFORT_LABEL, choice_args};
use serde_json::json;

use super::AgentEntry;

/// Runtimes whose command an entry can leave to `prompt`.
pub const BUILT: &[&str] = &["claude-code", "codex", "gemini-cli"];

/// What identifies AgentDocker in a runtime's configuration: the binary's
/// name, as the inventory looks for it.
pub const MARKER: &str = "agentdocker";

/// Where a built command will run, which is what the machine is asked
/// about: a CLI on the `PATH` the agent gets, the profile its
/// `CLAUDE_CONFIG_DIR` or `CODEX_HOME` names, the settings its checkout
/// holds.
#[derive(Clone, Copy, Debug)]
pub struct Destination<'a> {
    /// The entry's own `env`, expanded, laid over the environment `up`
    /// runs in.
    pub env: &'a BTreeMap<String, String>,
    /// The working directory the file gives the entry, resolved.
    pub workdir: &'a Path,
    /// `isolate = true`: the daemon runs the agent in a worktree it makes
    /// from the checkout's `HEAD`, at the checkout's top level, rather
    /// than in `workdir` itself.
    pub isolate: bool,
}

/// What building a command needs from the machine.
pub trait Host {
    /// An environment variable of the process reading the file, for
    /// `${VAR}`; an entry's own `env` is not consulted.
    fn var(&self, name: &str) -> Option<String>;
    /// The runtime's command-line tool, as the inventory finds it for `at`.
    fn program(&self, runtime: &str, at: &Destination) -> Option<PathBuf>;
    /// This `agentdocker`, which the coordination adapters run.
    fn agentdocker(&self) -> anyhow::Result<PathBuf>;
    /// The runtime's own configuration, as `at` reads it, already runs our
    /// MCP server.
    fn mcp_wired(&self, runtime: &str, at: &Destination) -> bool;
    /// Some Claude Code settings the session at `at` loads already run our
    /// hooks: adding them again would run each one twice.
    fn claude_hooks_present(&self, at: &Destination) -> bool;
}

/// Runtimes with an input adapter, for `idle_messages`.
const IDLE_RUNTIMES: &[&str] = &["claude-code", "codex"];

/// The variables of an entry's own `env` that name directories which choose
/// its CLI and the profile its wiring is read from: `PATH`, the home, and
/// the profiles Claude Code and Codex read.
const DIRECTORY_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
];

/// Whether two variable names are one, as the platform compares them:
/// without regard to case on Windows.
fn same_var(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// A variable of an entry's own `env`, by name.
pub fn entry_var<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(key, _)| same_var(key, name))
        .map(|(_, value)| value.as_str())
}

/// The directories `value` names when it is the variable `name`: each
/// entry of a `PATH` (an empty one is the current directory, as a shell
/// reads it), or the one directory a home or profile variable names; none
/// for any other variable, or an empty home or profile, which is unset.
fn directories(name: &str, value: &str) -> Vec<PathBuf> {
    if same_var(name, "PATH") {
        std::env::split_paths(value).collect()
    } else if DIRECTORY_VARS.iter().any(|var| same_var(var, name)) && !value.is_empty() {
        vec![PathBuf::from(value)]
    } else {
        Vec::new()
    }
}

/// A variable of an entry's own `env` as the agent will read it, starting
/// in `start`: a relative directory in its `PATH`, home or profile is
/// relative to where the agent starts, not to where `up` runs. Any other
/// variable is as written.
pub fn anchored(name: &str, value: &str, start: &Path) -> std::ffi::OsString {
    let dirs = directories(name, value);
    if dirs.iter().all(|dir| dir.is_absolute()) {
        return value.into();
    }
    let mut dirs = dirs.into_iter().map(|dir| {
        if dir.is_absolute() {
            dir
        } else {
            start.join(dir)
        }
    });
    if same_var(name, "PATH") {
        std::env::join_paths(dirs).unwrap_or_else(|_| value.into())
    } else {
        dirs.next()
            .map_or_else(|| value.into(), PathBuf::into_os_string)
    }
}

/// Which text of an entry [`check`] is judging: as the file gives it,
/// where a value with a `$` in it is still to be expanded and is left for
/// later, or as expanded, where every value is final and a `$` is just a
/// character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judged {
    AsWritten,
    Expanded,
}

impl Judged {
    /// Whether `value` is still to be expanded, so it cannot be judged yet.
    fn deferred(self, value: &str) -> bool {
        self == Self::AsWritten && value.contains('$')
    }
}

/// The first relative directory an entry's `env` names in a variable that
/// chooses its CLI or profile, with the variable: what an isolated launch
/// cannot place, since the worktree it starts in is the daemon's to make.
fn relative_directory(env: &BTreeMap<String, String>, judged: Judged) -> Option<(String, PathBuf)> {
    env.iter()
        .filter(|(_, value)| !judged.deferred(value))
        .find_map(|(name, value)| {
            directories(name, value)
                .into_iter()
                .find(|dir| dir.is_relative())
                .map(|dir| (name.clone(), dir))
        })
}

/// What is wrong with an entry's launch fields, judged from the file alone,
/// with the agent and the field named. As written, a value with a `$` in
/// it is still to be expanded and is left for later; [`check`] runs again
/// on the expanded entry, where every value is final, before anything is
/// launched.
pub fn check(name: &str, entry: &AgentEntry, judged: Judged) -> Result<(), String> {
    let runtime = entry.runtime.as_str();
    if !entry.built() {
        for (field, set) in [
            ("prompt", entry.prompt.is_some()),
            ("effort", entry.effort.is_some()),
            ("args", !entry.args.is_empty()),
            ("config", !entry.config.is_empty()),
            ("coordinate", entry.coordinate.is_some()),
        ] {
            if set {
                return Err(format!(
                    "agent `{name}` sets both `command` and `{field}`; `{field}` is for a command \
                     agentdocker builds, so remove `command` or put everything in it"
                ));
            }
        }
        if entry.idle_messages && !IDLE_RUNTIMES.contains(&runtime) {
            return Err(idle_unsupported(name, runtime));
        }
        return Ok(());
    }
    if !BUILT.contains(&runtime) {
        return Err(if entry.prompt.is_some() {
            format!(
                "agent `{name}`: agentdocker cannot build a command for runtime `{runtime}` \
                 (it can for {}); write `command`",
                BUILT.join(", ")
            )
        } else {
            format!(
                "agent `{name}` has no `command`; write one, or give `prompt` to a runtime \
                 agentdocker can launch ({})",
                BUILT.join(", ")
            )
        });
    }
    // An ordinary launch starts in its working directory, where a relative
    // directory in its own `env` is found; an isolated one starts in a
    // worktree the daemon makes, which is not known here.
    if entry.isolate
        && let Some((var, dir)) = relative_directory(&entry.env, judged)
    {
        return Err(format!(
            "agent `{name}` has `isolate = true` and `env.{var}` names the relative directory \
             {dir:?}; an isolated agent starts in a worktree the daemon makes, so write it as an \
             absolute path"
        ));
    }
    let terminal = entry.tty || entry.in_pane;
    let needs_prompt = |what: &str| {
        format!(
            "agent `{name}`: {what} without a terminal needs a `prompt`; give one, or set \
             `tty = true` for an interactive session"
        )
    };
    match runtime {
        "claude-code" => {
            effort_in(name, entry, CLAUDE_EFFORTS, judged)?;
            if !terminal && entry.prompt.is_none() {
                return Err(needs_prompt("Claude Code"));
            }
            if entry.idle_messages && !terminal {
                return Err(format!(
                    "agent `{name}`: idle messages reach an interactive Claude Code session; \
                     set `tty = true` or `in_pane = true`"
                ));
            }
        }
        "codex" => {
            effort_in(name, entry, CODEX_EFFORTS, judged)?;
            if entry.idle_messages {
                if entry.prompt.is_some() {
                    return Err(format!(
                        "agent `{name}`: Codex with idle messages starts without a prompt on its \
                         command line; leave `prompt` out and send the first message with \
                         `agentdocker send --to {name} …` once it is up"
                    ));
                }
            } else if !terminal && entry.prompt.is_none() {
                return Err(needs_prompt("Codex"));
            }
        }
        "gemini-cli" => {
            if entry.effort.is_some() {
                return Err(format!(
                    "agent `{name}`: Gemini CLI has no effort option on its command line; \
                     remove `effort`"
                ));
            }
            if !entry.config.is_empty() {
                return Err(format!(
                    "agent `{name}`: Gemini CLI takes no settings on its command line; put them \
                     in its settings file and remove `config`"
                ));
            }
            if entry.idle_messages {
                return Err(idle_unsupported(name, runtime));
            }
            if !terminal && entry.prompt.is_none() {
                return Err(needs_prompt("Gemini CLI"));
            }
        }
        _ => unreachable!("BUILT is checked above"),
    }
    Ok(())
}

fn idle_unsupported(name: &str, runtime: &str) -> String {
    format!(
        "agent `{name}`: idle messages need an input adapter, which AgentDocker has for {}, \
         not `{runtime}`",
        IDLE_RUNTIMES.join(" and ")
    )
}

fn effort_in(
    name: &str,
    entry: &AgentEntry,
    levels: &[&str],
    judged: Judged,
) -> Result<(), String> {
    match &entry.effort {
        Some(effort) if !judged.deferred(effort) && !levels.contains(&effort.as_str()) => {
            Err(format!(
                "agent `{name}` has effort = \"{effort}\"; {} takes {}",
                entry.runtime,
                levels.join(", ")
            ))
        }
        _ => Ok(()),
    }
}

/// Fill in `spec.command` (and its environment and labels) for an entry
/// that was checked by [`check`] and expanded. Returns notes for the
/// person: what was not wired, and why.
pub fn apply(
    spec: &mut AgentSpec,
    entry: &AgentEntry,
    host: &dyn Host,
) -> anyhow::Result<Vec<String>> {
    let mut notes = Vec::new();
    if entry.built() {
        let (command, built_notes) = build(spec, entry, host)?;
        spec.command = command;
        notes.extend(built_notes);
        if let Some(effort) = &entry.effort {
            spec.labels.insert(EFFORT_LABEL.to_owned(), effort.clone());
        }
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

/// The built command for one of [`BUILT`], and the notes building it left.
fn build(
    spec: &AgentSpec,
    entry: &AgentEntry,
    host: &dyn Host,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let runtime = entry.runtime.as_str();
    let mut notes = Vec::new();
    let at = Destination {
        env: &entry.env,
        workdir: spec.workdir.as_deref().unwrap_or(Path::new(".")),
        isolate: entry.isolate,
    };
    let program = match host.program(runtime, &at) {
        Some(path) => path.to_string_lossy().into_owned(),
        None => {
            let name = agentdocker_core::runtime::spec(runtime)
                .and_then(|spec| spec.clis.first().copied())
                .unwrap_or(runtime);
            notes.push(format!(
                "{}: `{name}` was not found on this machine; the daemon looks it up on PATH when \
                 the agent starts",
                spec.name
            ));
            name.to_owned()
        }
    };
    let coordinate = entry.coordinate.unwrap_or(true);
    let terminal = entry.tty || entry.in_pane;
    let choices = choice_args(
        runtime,
        entry.model.as_deref(),
        entry.effort.as_deref(),
        &entry.args,
    )
    .map_err(|why| anyhow::anyhow!("{why}: model {:?}, effort {:?}", entry.model, entry.effort))?;
    let mut command = vec![program];
    match runtime {
        "claude-code" => {
            if !terminal {
                command.push("-p".into());
            }
            command.extend(choices);
            if coordinate {
                let exe = host.agentdocker()?;
                // The channel enabler adds its own MCP entry.
                if !entry.idle_messages && !host.mcp_wired(runtime, &at) {
                    command.push("--mcp-config".into());
                    command.push(
                        json!({"mcpServers": {MARKER: {
                            "type": "stdio", "command": exe.to_string_lossy(),
                            "args": ["mcp", "--runtime", "claude-code"]
                        }}})
                        .to_string(),
                    );
                }
            }
            let mut settings = settings_json(&entry.config)?;
            if coordinate {
                let exe = host.agentdocker()?;
                if !host.claude_hooks_present(&at) {
                    crate::hooks::merge_claude_code_hooks(
                        &mut settings,
                        &agentdocker_host::runtimes::claude_hook_command(&exe)?,
                    )?;
                }
            }
            if settings.as_object().is_some_and(|map| !map.is_empty()) {
                command.push("--settings".into());
                command.push(settings.to_string());
            }
            command.extend(entry.args.iter().cloned());
            if let Some(prompt) = &entry.prompt {
                command.extend(["--".to_owned(), prompt.clone()]);
            }
        }
        "codex" => {
            if !terminal && !entry.idle_messages {
                command.push("exec".into());
            }
            command.extend(choices);
            command.extend(codex_config_args(&entry.config));
            // The idle-input bridge serves its own MCP connection.
            if coordinate && !entry.idle_messages && !host.mcp_wired(runtime, &at) {
                let exe = host.agentdocker()?;
                command.extend([
                    "-c".to_owned(),
                    format!(
                        "mcp_servers.{MARKER}.command={}",
                        json!(exe.to_string_lossy())
                    ),
                    "-c".to_owned(),
                    format!(r#"mcp_servers.{MARKER}.args=["mcp","--runtime","codex"]"#),
                ]);
            }
            command.extend(entry.args.iter().cloned());
            if let Some(prompt) = &entry.prompt {
                command.extend(["--".to_owned(), prompt.clone()]);
            }
        }
        "gemini-cli" => {
            if let Some(model) = &entry.model {
                command.extend(["--model".to_owned(), model.clone()]);
            }
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
            if coordinate && !host.mcp_wired(runtime, &at) {
                notes.push(format!(
                    "{}: Gemini CLI reads MCP servers only from its settings file; \
                     `agentdocker setup gemini-cli` registers AgentDocker there",
                    spec.name
                ));
            }
        }
        other => anyhow::bail!("no command builder for runtime `{other}`"),
    }
    Ok((command, notes))
}

/// `config` as the JSON object Claude Code's `--settings` takes.
fn settings_json(config: &BTreeMap<String, toml::Value>) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::to_value(config)?)
}

/// `config` as Codex's `-c key=value` pairs, one per leaf, nested tables
/// as dotted keys (`-c mcp_servers.x.command="…"`), each value written as
/// TOML, which is what `-c` reads.
fn codex_config_args(config: &BTreeMap<String, toml::Value>) -> Vec<String> {
    fn leaves(prefix: &str, value: &toml::Value, out: &mut Vec<String>) {
        match value {
            toml::Value::Table(table) => {
                for (key, value) in table {
                    leaves(&format!("{prefix}.{key}"), value, out);
                }
            }
            other => {
                out.extend(["-c".to_owned(), format!("{prefix}={}", toml_literal(other))]);
            }
        }
    }
    let mut out = Vec::new();
    for (key, value) in config {
        leaves(key, value, &mut out);
    }
    out
}

/// A TOML value as it is written inline. Strings take JSON's quoting, which
/// TOML's basic strings read.
fn toml_literal(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => json!(text).to_string(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        toml::Value::Array(items) => format!(
            "[{}]",
            items.iter().map(toml_literal).collect::<Vec<_>>().join(",")
        ),
        toml::Value::Table(table) => format!(
            "{{{}}}",
            table
                .iter()
                .map(|(key, value)| format!("{key}={}", toml_literal(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::agentfile::Agentfile;
    use std::collections::BTreeMap;

    /// A machine where every CLI is found and nothing is wired unless the
    /// test says so.
    pub struct Fake {
        pub vars: BTreeMap<&'static str, &'static str>,
        pub wired: bool,
        pub installed: bool,
        /// A real file, where an input enabler insists on one.
        pub exe: Option<PathBuf>,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                vars: BTreeMap::from([("MODEL", "opus"), ("TASK", "the parser")]),
                wired: false,
                installed: true,
                exe: None,
            }
        }
    }

    impl Host for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).map(|v| (*v).to_owned())
        }
        fn program(&self, runtime: &str, _: &Destination) -> Option<PathBuf> {
            self.installed
                .then(|| PathBuf::from(format!("/bin/{runtime}")))
        }
        fn agentdocker(&self) -> anyhow::Result<PathBuf> {
            Ok(self
                .exe
                .clone()
                .unwrap_or_else(|| PathBuf::from("/opt/ad/agentdocker")))
        }
        fn mcp_wired(&self, _: &str, _: &Destination) -> bool {
            self.wired
        }
        fn claude_hooks_present(&self, _: &Destination) -> bool {
            self.wired
        }
    }

    fn built(text: &str, host: &Fake) -> (AgentSpec, Vec<String>) {
        let file = Agentfile::parse(text).unwrap();
        let built = file
            .specs(Path::new("/repo/Agentfile.toml"), &[], host)
            .unwrap()
            .remove(0);
        (built.spec, built.notes)
    }

    /// A Claude Code entry that says what it wants gets the flags, and the
    /// coordination the person's own configuration lacks; nothing doubled
    /// when it already has it.
    #[test]
    fn claude_code_commands_are_built_with_coordination_wired_in() {
        let text = r#"
version = 2
[agents.writer]
runtime = "claude-code"
prompt = "Implement ${TASK}"
model = "${MODEL}"
effort = "high"
args = ["--permission-mode", "acceptEdits"]
"#;
        let (spec, notes) = built(text, &Fake::default());
        assert!(notes.is_empty(), "{notes:?}");
        let c = &spec.command;
        assert_eq!(
            &c[..6],
            [
                "/bin/claude-code",
                "-p",
                "--model",
                "opus",
                "--effort",
                "high"
            ]
        );
        assert_eq!(c[6], "--mcp-config");
        let mcp: serde_json::Value = serde_json::from_str(&c[7]).unwrap();
        assert_eq!(
            mcp,
            json!({"mcpServers":{"agentdocker":{"args":["mcp","--runtime","claude-code"],"command":"/opt/ad/agentdocker","type":"stdio"}}})
        );
        assert_eq!(c[8], "--settings");
        let settings: serde_json::Value = serde_json::from_str(&c[9]).unwrap();
        assert!(settings["hooks"]["PreToolUse"].is_array(), "{}", c[9]);
        assert!(c[9].contains("/opt/ad/agentdocker hook claude-code"));
        assert_eq!(
            &c[10..],
            [
                "--permission-mode",
                "acceptEdits",
                "--",
                "Implement the parser"
            ]
        );
        assert_eq!(spec.model.as_deref(), Some("opus"));
        assert_eq!(spec.labels[EFFORT_LABEL], "high");

        let wired = Fake {
            wired: true,
            ..Fake::default()
        };
        let (spec, _) = built(text, &wired);
        assert_eq!(
            spec.command,
            [
                "/bin/claude-code",
                "-p",
                "--model",
                "opus",
                "--effort",
                "high",
                "--permission-mode",
                "acceptEdits",
                "--",
                "Implement the parser"
            ]
        );

        let (spec, _) = built(
            "version = 2\n[agents.w]\nruntime = \"claude-code\"\nprompt = \"go\"\ncoordinate = false\n",
            &Fake::default(),
        );
        assert_eq!(spec.command, ["/bin/claude-code", "-p", "--", "go"]);
        assert!(!spec.labels.contains_key(EFFORT_LABEL));
    }

    /// `config` reaches Claude Code as one `--settings` document, with the
    /// hooks merged into it rather than a second `--settings`; a model or
    /// effort the person's own `args` set is left to them.
    #[test]
    fn claude_config_is_one_settings_document_and_args_win() {
        let (spec, _) = built(
            r#"
version = 2
[agents.w]
runtime = "claude-code"
tty = true
model = "opus"
effort = "low"
args = ["--model", "sonnet"]
config = { permissionMode = "acceptEdits", env = { FOO = "1" } }
coordinate = false
"#,
            &Fake::default(),
        );
        assert_eq!(
            &spec.command[..4],
            ["/bin/claude-code", "--effort", "low", "--settings"],
            "interactive: no -p"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&spec.command[4]).unwrap(),
            json!({"env":{"FOO":"1"},"permissionMode":"acceptEdits"})
        );
        assert_eq!(&spec.command[5..], ["--model", "sonnet"], "no prompt");
        let (spec, _) = built(
            "version = 2\n[agents.w]\nruntime = \"claude-code\"\nprompt = \"go\"\nconfig = { permissionMode = \"plan\" }\n",
            &Fake {
                wired: true,
                ..Fake::default()
            },
        );
        assert_eq!(
            spec.command,
            [
                "/bin/claude-code",
                "-p",
                "--settings",
                r#"{"permissionMode":"plan"}"#,
                "--",
                "go"
            ]
        );
        let (_, notes) = built(
            "version = 2\n[agents.w]\nruntime = \"claude-code\"\nprompt = \"go\"\n",
            &Fake {
                installed: false,
                ..Fake::default()
            },
        );
        assert!(
            notes[0].contains("`claude` was not found"),
            "the bare name is used, and said: {notes:?}"
        );
    }

    #[test]
    fn interactive_and_idle_claude_sessions_use_the_channel() {
        let exe = tempfile::NamedTempFile::new().unwrap();
        let host = Fake {
            exe: Some(exe.path().to_owned()),
            ..Fake::default()
        };
        let (spec, _) = built(
            "version = 2\n[agents.w]\nruntime = \"claude-code\"\ntty = true\nidle_messages = true\n",
            &host,
        );
        let c = &spec.command;
        assert_eq!(c[0], "/bin/claude-code");
        assert!(!c.contains(&"-p".to_owned()), "interactive: {c:?}");
        assert!(
            c.contains(&"--dangerously-load-development-channels".to_owned()),
            "{c:?}"
        );
        assert_eq!(
            c.iter().filter(|w| *w == "--mcp-config").count(),
            1,
            "the channel's MCP entry only: {c:?}"
        );
        assert!(
            c.contains(&"--settings".to_owned()),
            "the hooks still ride along: {c:?}"
        );
        assert_eq!(
            spec.env[agentdocker_host::provider_input::CLAUDE_CHANNEL_ENV],
            "1"
        );
    }

    #[test]
    fn codex_commands_are_built_with_config_and_coordination() {
        let (spec, notes) = built(
            r#"
version = 2
[agents.r]
runtime = "codex"
prompt = "review"
model = "gpt-5"
effort = "low"
config = { approval_policy = "never", sandbox = { mode = "workspace-write" }, flags = ["a", "b"], n = 3, ok = true }
"#,
            &Fake::default(),
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            spec.command,
            [
                "/bin/codex",
                "exec",
                "--model",
                "gpt-5",
                "-c",
                "model_reasoning_effort=\"low\"",
                "-c",
                "approval_policy=\"never\"",
                "-c",
                "flags=[\"a\",\"b\"]",
                "-c",
                "n=3",
                "-c",
                "ok=true",
                "-c",
                "sandbox.mode=\"workspace-write\"",
                "-c",
                "mcp_servers.agentdocker.command=\"/opt/ad/agentdocker\"",
                "-c",
                r#"mcp_servers.agentdocker.args=["mcp","--runtime","codex"]"#,
                "--",
                "review",
            ]
        );
        assert_eq!(spec.labels[EFFORT_LABEL], "low");
        // Interactive, wired: the plain tool with its prompt.
        let (spec, _) = built(
            "version = 2\n[agents.r]\nruntime = \"codex\"\ntty = true\nprompt = \"-x\"\n",
            &Fake {
                wired: true,
                ..Fake::default()
            },
        );
        assert_eq!(spec.command, ["/bin/codex", "--", "-x"]);
    }

    #[test]
    fn idle_codex_runs_through_the_input_adapter_without_exec_or_prompt() {
        let exe = tempfile::NamedTempFile::new().unwrap();
        let host = Fake {
            exe: Some(exe.path().to_owned()),
            ..Fake::default()
        };
        let (spec, _) = built(
            "version = 2\n[agents.r]\nruntime = \"codex\"\nidle_messages = true\nmodel = \"gpt-5\"\neffort = \"high\"\n",
            &host,
        );
        let c = &spec.command;
        assert_eq!(c[0], exe.path().to_str().unwrap());
        assert_eq!(&c[1..4], ["codex-input", "--", "/bin/codex"]);
        assert!(!c.contains(&"exec".to_owned()), "{c:?}");
        assert!(
            c.windows(2).any(|w| w == ["-c", "model=\"gpt-5\""]),
            "the adapter takes the model as a setting: {c:?}"
        );
        assert!(
            c.windows(2)
                .any(|w| w == ["-c", "model_reasoning_effort=\"high\""]),
            "{c:?}"
        );
        assert!(
            !c.iter().any(|w| w.contains("mcp_servers")),
            "the bridge serves MCP itself: {c:?}"
        );
        assert_eq!(
            spec.env[agentdocker_host::provider_input::CODEX_INPUT_ENV],
            "1"
        );
    }

    #[test]
    fn gemini_commands_are_built_and_its_settings_file_is_pointed_at() {
        let (spec, notes) = built(
            "version = 2\n[agents.g]\nruntime = \"gemini-cli\"\nprompt = \"-x\"\nmodel = \"gemini-pro\"\nargs = [\"--yolo\"]\n",
            &Fake::default(),
        );
        assert_eq!(
            spec.command,
            [
                "/bin/gemini-cli",
                "--model",
                "gemini-pro",
                "--yolo",
                "--prompt=-x"
            ]
        );
        assert!(notes[0].contains("setup gemini-cli"), "{notes:?}");
        let (spec, notes) = built(
            "version = 2\n[agents.g]\nruntime = \"gemini-cli\"\ntty = true\nprompt = \"go\"\n",
            &Fake {
                wired: true,
                ..Fake::default()
            },
        );
        assert_eq!(spec.command, ["/bin/gemini-cli", "--prompt-interactive=go"]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// What a built command cannot be is refused when the file is read,
    /// with the agent's name and what to do.
    #[test]
    fn launch_fields_that_cannot_work_are_refused_with_the_fix() {
        for (text, expect) in [
            (
                "[agents.a]\nruntime = \"claude-code\"\ncommand = [\"claude\"]\nprompt = \"x\"",
                "agent `a` sets both `command` and `prompt`",
            ),
            (
                "[agents.a]\nruntime = \"claude-code\"\ncommand = [\"claude\"]\nconfig = { a = 1 }",
                "sets both `command` and `config`",
            ),
            (
                "[agents.a]\nruntime = \"claude-code\"\ncommand = [\"claude\"]\ncoordinate = true",
                "sets both `command` and `coordinate`",
            ),
            (
                "[agents.a]\nruntime = \"claude-code\"\neffort = \"huge\"\nprompt = \"x\"",
                "agent `a` has effort = \"huge\"; claude-code takes low, medium, high, xhigh, max",
            ),
            (
                "[agents.a]\nruntime = \"codex\"\neffort = \"max\"\nprompt = \"x\"",
                "codex takes minimal, low, medium, high, xhigh",
            ),
            (
                "[agents.a]\nruntime = \"claude-code\"",
                "agent `a`: Claude Code without a terminal needs a `prompt`",
            ),
            (
                "[agents.a]\nruntime = \"codex\"",
                "Codex without a terminal needs a `prompt`",
            ),
            (
                "[agents.a]\nruntime = \"claude-code\"\nprompt = \"x\"\nidle_messages = true",
                "set `tty = true`",
            ),
            (
                "[agents.a]\nruntime = \"codex\"\ntty = true\nidle_messages = true\nprompt = \"x\"",
                "agentdocker send --to a",
            ),
            (
                "[agents.a]\nruntime = \"aider\"\nprompt = \"x\"",
                "cannot build a command for runtime `aider`",
            ),
            (
                "[agents.a]\nruntime = \"gemini-cli\"\nprompt = \"x\"\neffort = \"high\"",
                "Gemini CLI has no effort option",
            ),
            (
                "[agents.a]\nruntime = \"gemini-cli\"\nprompt = \"x\"\nconfig = { a = 1 }",
                "Gemini CLI takes no settings",
            ),
            (
                "[agents.a]\nruntime = \"gemini-cli\"\nprompt = \"x\"\nidle_messages = true",
                "claude-code and codex, not `gemini-cli`",
            ),
            (
                "[agents.a]\nruntime = \"gemini-cli\"\ncommand = [\"gemini\"]\nidle_messages = true",
                "claude-code and codex",
            ),
            (
                "[agents.a]\nruntime = \"claude-code\"\nprompt = \"x\"\nisolate = true\nenv = { CLAUDE_CONFIG_DIR = \"profile\" }",
                "`env.CLAUDE_CONFIG_DIR` names the relative directory \"profile\"",
            ),
        ] {
            let error = Agentfile::parse(&format!("version = 2\n{text}\n"))
                .unwrap_err()
                .to_string();
            assert!(error.contains(expect), "{text}\n→ {error}");
        }
    }

    /// An entry's own relative directories are where the agent starts: its
    /// working directory. An isolated launch, whose worktree is the
    /// daemon's to make, refuses them where it builds the command, and a
    /// written command keeps them for the agent to read where it runs.
    #[test]
    fn relative_directories_in_an_entrys_env_are_taken_from_where_it_starts() {
        let start = Path::new("/work/app");
        assert_eq!(
            anchored("CLAUDE_CONFIG_DIR", "profile", start),
            start.join("profile").into_os_string()
        );
        assert_eq!(anchored("CODEX_HOME", "/abs/codex", start), "/abs/codex");
        assert_eq!(anchored("CODEX_HOME", "", start), "", "empty is unset");
        assert_eq!(
            anchored("RUST_LOG", "info", start),
            "info",
            "not a directory"
        );
        if cfg!(unix) {
            assert_eq!(
                anchored("PATH", "bin:/usr/bin", start),
                "/work/app/bin:/usr/bin"
            );
            assert_eq!(anchored("PATH", "/a:/b", start), "/a:/b");
        }
        let entry = |text: &str| {
            Agentfile::parse(&format!("version = 2\n[agents.a]\n{text}\n"))
                .map(|_| ())
                .map_err(|e| e.to_string())
        };
        let refused =
            entry("runtime = \"codex\"\nprompt = \"x\"\nisolate = true\nenv = { PATH = \"bin\" }")
                .unwrap_err();
        assert!(
            refused.contains("agent `a` has `isolate = true`"),
            "{refused}"
        );
        assert!(
            refused.contains("write it as an absolute path"),
            "{refused}"
        );
        let absolute = if cfg!(windows) {
            "C:\\opt\\bin"
        } else {
            "/opt/bin"
        };
        entry(&format!(
            "runtime = \"codex\"\nprompt = \"x\"\nisolate = true\nenv = {{ PATH = '{absolute}' }}"
        ))
        .unwrap();
        entry("command = [\"make\"]\nisolate = true\nenv = { PATH = \"bin\" }").unwrap();
        entry("runtime = \"codex\"\nprompt = \"x\"\nenv = { PATH = \"bin\" }").unwrap();
        // `$$` is one `$` once expanded: still relative, still refused;
        // so is a variable whose value has a `$` in it.
        for env in [
            "{ CLAUDE_CONFIG_DIR = \"$$profile\" }",
            "{ CODEX_HOME = \"${DOLLAR}\" }",
        ] {
            let file = Agentfile::parse(&format!(
                "version = 2\n[agents.a]\nruntime = \"codex\"\nprompt = \"x\"\nisolate = true\nenv = {env}\n"
            ))
            .expect("as written, a `$` is still to be expanded");
            let mut host = Fake::default();
            host.vars.insert("DOLLAR", "$profile");
            let error = file
                .specs(Path::new("/repo/Agentfile.toml"), &[], &host)
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("relative directory \"$profile\""),
                "{env}: {error:#}"
            );
        }
        let effort = Agentfile::parse(
            "version = 2\n[agents.a]\nruntime = \"codex\"\nprompt = \"x\"\neffort = \"$$high\"\n",
        )
        .unwrap()
        .specs(Path::new("/repo/Agentfile.toml"), &[], &Fake::default())
        .unwrap_err();
        assert!(
            format!("{effort:#}").contains("effort = \"$high\""),
            "{effort:#}"
        );
        // A value still holding a variable is judged once it is expanded.
        let file = Agentfile::parse(
            "version = 2\n[agents.a]\nruntime = \"codex\"\nprompt = \"x\"\nisolate = true\nenv = { CODEX_HOME = \"${DIR}\" }\n",
        )
        .unwrap();
        let mut host = Fake::default();
        host.vars.insert("DIR", "codex-home");
        let error = file
            .specs(Path::new("/repo/Agentfile.toml"), &[], &host)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("relative directory"),
            "{error:#}"
        );
    }

    /// A value from the environment is checked once expanded: an effort
    /// written as `${VAR:-high}` is accepted when the file is read, and an
    /// expanded effort the runtime does not take is refused before launch.
    #[test]
    fn expanded_launch_fields_are_checked_after_expansion() {
        let text = "version = 2\n[agents.w]\nruntime = \"claude-code\"\nprompt = \"go\"\neffort = \"${EFFORT:-high}\"\n";
        let file = Agentfile::parse(text).expect("an expandable effort reads");
        let spec = file
            .specs(Path::new("/repo/Agentfile.toml"), &[], &Fake::default())
            .unwrap()
            .remove(0)
            .spec;
        assert!(spec.command.windows(2).any(|w| w == ["--effort", "high"]));
        let mut host = Fake::default();
        host.vars.insert("EFFORT", "huge");
        let error = file
            .specs(Path::new("/repo/Agentfile.toml"), &[], &host)
            .unwrap_err()
            .to_string();
        assert!(error.contains("huge"), "{error}");
        let empty = Agentfile::parse("version = 2\n[agents.a]\ncommand = [\"${NOTHING-}\"]\n")
            .unwrap()
            .specs(Path::new("/x/A.toml"), &[], &Fake::default())
            .unwrap_err()
            .to_string();
        assert!(empty.contains("empty command"), "{empty}");
        // A model is one word: an expansion that makes it two is refused
        // rather than read as another option.
        let mut host = Fake::default();
        host.vars.insert("MODEL", "--dangerously-skip-permissions");
        let error = Agentfile::parse(
            "version = 2\n[agents.w]\nruntime = \"claude-code\"\nprompt = \"go\"\nmodel = \"${MODEL}\"\n",
        )
        .unwrap()
        .specs(Path::new("/repo/Agentfile.toml"), &[], &host)
        .unwrap_err();
        assert!(format!("{error:#}").contains("one word"), "{error:#}");
    }

    #[test]
    fn toml_literals_are_what_codex_reads() {
        assert_eq!(toml_literal(&toml::Value::from("a \"b\"")), r#""a \"b\"""#);
        assert_eq!(toml_literal(&toml::Value::from(2.5)), "2.5");
        assert_eq!(
            toml_literal(&toml::Value::Array(vec![
                toml::Value::from(1),
                toml::Value::from("x")
            ])),
            "[1,\"x\"]"
        );
    }
}
