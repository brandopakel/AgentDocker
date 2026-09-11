//! Configure a newly managed provider's input path without editing its profile.
use agentdocker_core::AgentSpec;
use std::io;
use std::path::Path;

pub const CLAUDE_CHANNEL_ENV: &str = "AGENTDOCKER_CLAUDE_CHANNEL_INPUT";

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
