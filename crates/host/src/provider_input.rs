//! Configure a newly managed provider's input path without editing its profile.
use agentdocker_core::AgentSpec;
use std::io;
use std::path::Path;

pub const CLAUDE_CHANNEL_ENV: &str = "AGENTDOCKER_CLAUDE_CHANNEL_INPUT";
pub const CODEX_INPUT_ENV: &str = "AGENTDOCKER_CODEX_INPUT";

pub fn is_codex_input(agent: &agentdocker_core::AgentRecord) -> bool {
    agent.managed
        && agent.container.is_none()
        && agent.spec.runtime == "codex"
        && agent
            .spec
            .env
            .get(CODEX_INPUT_ENV)
            .is_some_and(|value| value == "1")
}

/// Recognize only the app-server launched by this supervised bridge. An ordinary
/// nested Codex session is a separate agent, even if it inherited our environment.
pub fn owns_codex_process(
    agent: &agentdocker_core::AgentRecord,
    pid: u32,
    table: &[crate::procinfo::Process],
) -> bool {
    if !is_codex_input(agent) || !agent.status.is_live() {
        return false;
    }
    let Some(process) = table.iter().find(|p| p.pid == pid) else {
        return false;
    };
    if crate::procinfo::runtime_of(&process.argv) != Some("codex")
        || !process
            .argv
            .windows(2)
            .any(|args| args == ["app-server", "--stdio"])
    {
        return false;
    }
    let mut parent = process.ppid;
    if let Some(launcher) = table.iter().find(|p| p.pid == parent)
        && is_input_launcher(&agent.spec, agent.pid, launcher)
    {
        parent = launcher.ppid;
    }
    agent.pid == Some(parent)
        && agent.process_started_at.is_some()
        && crate::procinfo::start_time(parent) == agent.process_started_at
        && crate::procinfo::start_time(pid)
            .is_some_and(|born| Some(born) >= agent.process_started_at)
        && agent.spec.workdir.as_ref().is_some_and(|cwd| {
            cwd.canonicalize()
                .ok()
                .zip(crate::procinfo::cwd(pid).and_then(|p| p.canonicalize().ok()))
                .is_some_and(|(expected, actual)| expected == actual)
        })
}

fn is_input_launcher(
    spec: &AgentSpec,
    owner: Option<u32>,
    launcher: &crate::procinfo::Process,
) -> bool {
    if owner != Some(launcher.ppid)
        || launcher.argv.len() < 4
        || launcher.argv[2..4] != ["app-server", "--stdio"]
    {
        return false;
    }
    let interpreter = Path::new(&launcher.argv[0])
        .file_name()
        .and_then(|p| p.to_str());
    if !matches!(
        interpreter,
        Some("node" | "bun" | "deno" | "python" | "python3")
    ) {
        return false;
    }
    let Some(at) = spec
        .command
        .windows(2)
        .position(|args| args == ["codex-input", "--"])
    else {
        return false;
    };
    let Some(selected) = spec.command.get(at + 2) else {
        return false;
    };
    let selected = Path::new(selected);
    let script = Path::new(&launcher.argv[1]);
    if selected.components().count() == 1 {
        // PATH launches are resolved by the directly owned interpreter. Require
        // its exact selected script name and app-server arguments, not merely
        // another agent somewhere in the process ancestry.
        script.file_name() == selected.file_name()
    } else {
        selected
            .canonicalize()
            .ok()
            .zip(script.canonicalize().ok())
            .is_some_and(|(expected, actual)| expected == actual)
    }
}

/// Wrap a new native Codex session without changing its provider profile. Only
/// arguments with an exact app-server equivalent are accepted.
pub fn enable_codex_input(spec: &mut AgentSpec, agentdocker: &Path) -> io::Result<()> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    if spec.runtime != "codex" || spec.command.is_empty() {
        return Err(invalid(
            "Codex input requires a new native Codex launch command",
        ));
    }
    if !agentdocker.is_absolute() || !agentdocker.is_file() {
        return Err(invalid(
            "Codex input requires the matching absolute AgentDocker CLI",
        ));
    }
    if spec.env.get(CODEX_INPUT_ENV).is_some_and(|v| v != "1") {
        return Err(invalid(
            "Codex input conflicts with the supplied input-mode environment",
        ));
    }
    let arguments = codex_arguments(&spec.command[1..])?;
    let mut command = vec![
        agentdocker
            .to_str()
            .ok_or_else(|| invalid("CLI path is not UTF-8"))?
            .to_owned(),
        "codex-input".into(),
        "--".into(),
        spec.command[0].clone(),
    ];
    command.extend(arguments);
    spec.command = command;
    spec.env.insert(CODEX_INPUT_ENV.into(), "1".into());
    Ok(())
}

pub fn codex_arguments(arguments: &[String]) -> io::Result<Vec<String>> {
    let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidInput, message);
    let mut result = Vec::new();
    let mut args = arguments.iter();
    while let Some(argument) = args.next() {
        let (flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(k, v)| (k, Some(v)));
        match flag {
            "-c" | "--config" | "--enable" | "--disable" | "--model" | "-m" => {
                let value = inline
                    .or_else(|| args.next().map(String::as_str))
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| invalid(format!("{flag} needs a value")))?;
                if matches!(flag, "--model" | "-m") {
                    result.extend(["-c".into(), format!("model={}", serde_json::json!(value))]);
                } else {
                    result.extend([flag.into(), value.into()]);
                }
            }
            "--strict-config" if inline.is_none() => result.push(argument.clone()),
            _ => {
                return Err(invalid(format!(
                    "Codex input does not support launch argument {flag}; use provider configuration and send the first message through AgentDocker"
                )));
            }
        }
    }
    Ok(result)
}

/// Enable the tested Claude research-preview channel for a fresh interactive
/// native session. The caller supplies its matching, absolute AgentDocker CLI.
/// This changes only the spec's command/environment, after every check succeeds.
/// Provider consent, organization policy and general tool permissions still apply.
pub fn enable_claude_channel(spec: &mut AgentSpec, agentdocker: &Path) -> io::Result<()> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    if spec.runtime != "claude-code" || !(spec.tty || spec.in_pane) {
        return Err(invalid(
            "Claude channel input requires a new interactive claude-code session",
        ));
    }
    let Some(program) = spec.command.first() else {
        return Err(invalid("Claude channel input requires a launch command"));
    };
    // An absolute caller-selected CLI avoids shell PATH changes and preserves
    // the installation's existing executable lifetime-pin behavior.
    if !agentdocker.is_absolute() || !agentdocker.is_file() {
        return Err(invalid(
            "Claude channel input requires the matching absolute AgentDocker CLI path",
        ));
    }
    let cli = agentdocker
        .to_str()
        .ok_or_else(|| invalid("AgentDocker CLI path is not valid UTF-8"))?;
    if cli.contains("${") {
        return Err(invalid(
            "AgentDocker CLI path contains provider environment-expansion syntax",
        ));
    }
    if spec
        .env
        .get(CLAUDE_CHANNEL_ENV)
        .is_some_and(|value| value != "1")
    {
        return Err(invalid(
            "Claude channel input conflicts with the supplied input-mode environment",
        ));
    }
    for argument in spec
        .command
        .iter()
        .skip(1)
        .take_while(|argument| argument.as_str() != "--")
    {
        let flag = argument.split('=').next().unwrap_or(argument);
        if matches!(
            flag,
            "--mcp-config"
                | "--dangerously-load-development-channels"
                | "--channels"
                | "--print"
                | "-p"
        ) {
            return Err(invalid(
                "Claude channel input conflicts with explicit MCP/channel or print-mode arguments",
            ));
        }
    }
    let config = serde_json::json!({"mcpServers": {"agentdocker": {
        "type": "stdio", "command": cli,
        "args": ["mcp", "--runtime", "claude-code", "--claude-channel"]
    }}});
    let mut command = vec![
        program.clone(),
        "--mcp-config".into(),
        config.to_string(),
        "--dangerously-load-development-channels".into(),
        "server:agentdocker".into(),
    ];
    // Insert before any positional '--' so the options remain provider options.
    // Do not add --strict-mcp-config: other user-configured MCP entries remain.
    command.extend(spec.command.iter().skip(1).cloned());
    spec.command = command;
    spec.env.insert(CLAUDE_CHANNEL_ENV.into(), "1".into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> AgentSpec {
        AgentSpec {
            runtime: "claude-code".into(),
            command: vec!["claude".into()],
            tty: true,
            ..Default::default()
        }
    }

    #[test]
    fn codex_input_keeps_provider_settings_and_rejects_arguments_without_an_exact_equivalent() {
        let cli = tempfile::NamedTempFile::new().unwrap();
        let mut launch = AgentSpec {
            runtime: "codex".into(),
            command: vec![
                "codex".into(),
                "--model".into(),
                "chosen-model".into(),
                "-c".into(),
                "approval_policy=\"on-request\"".into(),
                "--enable=own_feature".into(),
            ],
            ..Default::default()
        };
        launch
            .env
            .insert("CODEX_HOME".into(), "/provider/profile".into());
        enable_codex_input(&mut launch, cli.path()).unwrap();
        assert_eq!(
            launch.command[1..],
            [
                "codex-input",
                "--",
                "codex",
                "-c",
                "model=\"chosen-model\"",
                "-c",
                "approval_policy=\"on-request\"",
                "--enable",
                "own_feature"
            ]
        );
        assert_eq!(launch.env["CODEX_HOME"], "/provider/profile");
        assert_eq!(launch.env[CODEX_INPUT_ENV], "1");
        for arguments in [
            vec!["--listen", "ws://localhost:4000"],
            vec!["--profile", "work"],
            vec!["a prompt"],
            vec!["-c"],
        ] {
            let mut launch = AgentSpec {
                runtime: "codex".into(),
                command: std::iter::once("codex")
                    .chain(arguments)
                    .map(str::to_owned)
                    .collect(),
                ..Default::default()
            };
            let before = serde_json::to_value(&launch).unwrap();
            assert!(enable_codex_input(&mut launch, cli.path()).is_err());
            assert_eq!(serde_json::to_value(&launch).unwrap(), before);
        }
    }

    #[test]
    fn codex_input_launcher_accepts_the_selected_symlink_but_keeps_nested_agents_separate() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("codex.js");
        std::fs::write(&script, "fixture").unwrap();
        let selected = root.path().join("codex");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&script, &selected).unwrap();
        #[cfg(windows)]
        std::fs::copy(&script, &selected).unwrap();
        let mut spec = AgentSpec {
            command: vec![
                "/agentdocker".into(),
                "codex-input".into(),
                "--".into(),
                selected.to_string_lossy().into_owned(),
            ],
            ..Default::default()
        };
        let launcher = crate::procinfo::Process {
            pid: 20,
            ppid: 10,
            argv: vec![
                "node".into(),
                selected.to_string_lossy().into_owned(),
                "app-server".into(),
                "--stdio".into(),
            ],
        };
        assert!(is_input_launcher(&spec, Some(10), &launcher));
        assert!(!is_input_launcher(&spec, Some(9), &launcher));
        let mut other = launcher.clone();
        other.argv[1] = root.path().join("unrelated").to_string_lossy().into_owned();
        assert!(!is_input_launcher(&spec, Some(10), &other));
        let mut tui = launcher.clone();
        tui.argv.truncate(2);
        assert!(!is_input_launcher(&spec, Some(10), &tui));
        spec.command[3] = "codex".into();
        assert!(is_input_launcher(&spec, Some(10), &launcher));
        other.argv[1] = "other-agent".into();
        assert!(!is_input_launcher(&spec, Some(10), &other));
    }

    #[test]
    fn managed_channel_keeps_user_arguments_and_identity_environment() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("agentdocker with spaces");
        std::fs::write(&cli, b"fixture executable path").unwrap();
        let mut launch = spec();
        launch.command.extend([
            "--model".into(),
            "chosen-model".into(),
            "--".into(),
            "literal --mcp-config prompt".into(),
        ]);
        launch.env.insert("OWN_SETTING".into(), "retained".into());
        let arguments = launch.command[1..].to_vec();
        enable_claude_channel(&mut launch, &cli).unwrap();
        assert_eq!(&launch.command[5..], arguments.as_slice());
        let config: serde_json::Value = serde_json::from_str(&launch.command[2]).unwrap();
        assert_eq!(
            config["mcpServers"]["agentdocker"]["command"],
            cli.to_str().unwrap()
        );
        assert_eq!(
            config["mcpServers"]["agentdocker"]["args"],
            serde_json::json!(["mcp", "--runtime", "claude-code", "--claude-channel"])
        );
        assert!(
            !launch
                .command
                .iter()
                .any(|arg| arg == "--strict-mcp-config")
        );
        assert_eq!(launch.env["OWN_SETTING"], "retained");
        assert_eq!(launch.env[CLAUDE_CHANNEL_ENV], "1");
        assert!(!launch.env.contains_key("AGENTDOCKER_AGENT_ID"));
        assert!(!launch.env.contains_key("AGENTDOCKER_SOCKET"));
    }

    #[test]
    fn invalid_or_competing_input_modes_leave_the_spec_unchanged() {
        let cli = tempfile::NamedTempFile::new().unwrap();
        let mut invalid = Vec::new();
        let mut wrong_runtime = spec();
        wrong_runtime.runtime = "codex".into();
        invalid.push(wrong_runtime);
        let mut pipes = spec();
        pipes.tty = false;
        invalid.push(pipes);
        let mut conflict = spec();
        conflict.env.insert(CLAUDE_CHANNEL_ENV.into(), "0".into());
        invalid.push(conflict);
        for flag in [
            "--mcp-config",
            "--mcp-config=other.json",
            "--channels",
            "--dangerously-load-development-channels",
            "--print",
            "-p",
        ] {
            let mut launch = spec();
            launch.command.push(flag.into());
            invalid.push(launch);
        }
        for mut launch in invalid {
            let before = serde_json::to_value(&launch).unwrap();
            assert!(enable_claude_channel(&mut launch, cli.path()).is_err());
            assert_eq!(serde_json::to_value(launch).unwrap(), before);
        }
        assert!(enable_claude_channel(&mut spec(), Path::new("agentdocker")).is_err());
    }
}
