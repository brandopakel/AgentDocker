//! Reconstruct a recorded interactive launch without replaying its first input.
use super::{CLAUDE_CHANNEL_ENV, enable_claude_channel};
use agentdocker_core::AgentSpec;
use std::{io, path::Path};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Resume with the recorded options and explicit environment, using the caller's
/// selected executable and current AgentDocker channel. Unrecorded external
/// launches have no options to recover; they retain the provider's defaults.
pub fn reconnect_claude(
    previous: &AgentSpec,
    session: &str,
    program: &Path,
    cli: &Path,
) -> io::Result<AgentSpec> {
    if previous.runtime != "claude-code"
        || !agentdocker_core::identity::plain_session_id(session)
        || program.as_os_str().is_empty()
    {
        return Err(invalid("Claude reconnect needs its recorded conversation"));
    }
    let mut command = vec![program.to_string_lossy().into_owned()];
    command.extend(resume_options(previous)?);
    command.extend(["--resume".into(), session.into()]);
    let mut spec = AgentSpec {
        name: previous.name.clone(),
        runtime: previous.runtime.clone(),
        provider: previous.provider.clone(),
        model: previous.model.clone(),
        command,
        workdir: previous.workdir.clone(),
        env: previous.env.clone(),
        tty: true,
        ..Default::default()
    };
    // Reconnect explicitly opts into the channel, including a formerly plain
    // session. Other explicitly recorded environment values are retained.
    spec.env.remove(CLAUDE_CHANNEL_ENV);
    enable_claude_channel(&mut spec, cli)?;
    Ok(spec)
}

/// Strip only the exact channel prefix that our launcher owns. A changed or
/// additional MCP configuration must not silently disappear on reconnect.
fn recorded_arguments(previous: &AgentSpec) -> io::Result<&[String]> {
    let arguments = previous.command.get(1..).unwrap_or_default();
    if arguments.first().is_none_or(|arg| arg != "--mcp-config") {
        return Ok(arguments);
    }
    if previous
        .env
        .get(CLAUDE_CHANNEL_ENV)
        .is_none_or(|v| v != "1")
        || arguments.len() < 4
        || arguments[2] != "--dangerously-load-development-channels"
        || arguments[3] != "server:agentdocker"
    {
        return Err(invalid(
            "Cannot reconstruct the recorded Claude MCP configuration",
        ));
    }
    let config: serde_json::Value = serde_json::from_str(&arguments[1])
        .map_err(|_| invalid("Cannot reconstruct the recorded Claude MCP configuration"))?;
    let executable = config["mcpServers"]["agentdocker"]["command"]
        .as_str()
        .filter(|value| Path::new(value).is_absolute())
        .ok_or_else(|| invalid("Cannot reconstruct the recorded Claude MCP configuration"))?;
    let expected = serde_json::json!({"mcpServers":{"agentdocker":{
        "type":"stdio", "command":executable,
        "args":["mcp","--runtime","claude-code","--claude-channel"]
    }}});
    if config != expected {
        return Err(invalid(
            "Cannot reconstruct the recorded Claude MCP configuration",
        ));
    }
    Ok(&arguments[4..])
}

#[derive(Clone, Copy)]
enum Arity {
    Flag,
    Required,
    Optional,
    Many,
}

/// Known option arities keep prompt-looking values inside their own options.
/// Guessing an unknown option's arity could discard a permission restriction or
/// mistake its value for the old prompt, so unsupported launches fail closed.
fn option(flag: &str) -> io::Result<(Arity, bool)> {
    use Arity::*;
    Ok(match flag {
        "--resume" | "-r" | "--from-pr" | "--teleport" => (Optional, false),
        "--session-id" => (Required, false),
        "--continue" | "-c" | "--fork-session" => (Flag, false),
        "--agent"
        | "--agents"
        | "--append-system-prompt"
        | "--append-system-prompt-file"
        | "--system-prompt"
        | "--system-prompt-file"
        | "--autocompact"
        | "--debug-file"
        | "--effort"
        | "--fallback-model"
        | "--model"
        | "--name"
        | "-n"
        | "--permission-mode"
        | "--permission-prompt-tool"
        | "--permission-prompts"
        | "--plugin-dir"
        | "--plugin-url"
        | "--remote-control-session-name-prefix"
        | "--setting-sources"
        | "--settings"
        | "--system-prompt-snapshot"
        | "--max-budget-usd"
        | "--max-turns"
        | "--json-schema" => (Required, true),
        "--add-dir" | "--allowedTools" | "--allowed-tools" | "--disallowedTools"
        | "--disallowed-tools" | "--tools" | "--betas" => (Many, true),
        "--allow-dangerously-skip-permissions"
        | "--dangerously-skip-permissions"
        | "--ax-screen-reader"
        | "--bare"
        | "--brief"
        | "--chrome"
        | "--no-chrome"
        | "--disable-slash-commands"
        | "--exclude-dynamic-system-prompt-sections"
        | "--ide"
        | "--restricted"
        | "--safe-mode"
        | "--strict-mcp-config"
        | "--verbose" => (Flag, true),
        "--debug" | "-d" | "--prompt-suggestions" | "--remote-control" => (Optional, true),
        _ => {
            return Err(invalid(
                "Cannot safely reconstruct this Claude launch: an unsupported option is recorded; resume it explicitly with its original configuration",
            ));
        }
    })
}

/// Preserve option order and exact values; discard the single initial prompt
/// and old session selectors. Never include prompt/option values in errors.
fn resume_options(previous: &AgentSpec) -> io::Result<Vec<String>> {
    let args = recorded_arguments(previous)?;
    let mut retained = Vec::new();
    let mut index = 0;
    let mut prompt = false;
    while let Some(argument) = args.get(index) {
        if argument == "--" {
            let remaining = args.len() - index - 1;
            if remaining > usize::from(!prompt) {
                return Err(invalid(
                    "Cannot safely reconstruct multiple Claude prompt arguments",
                ));
            }
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            if prompt {
                return Err(invalid(
                    "Cannot safely reconstruct multiple Claude prompt arguments",
                ));
            }
            prompt = true;
            index += 1;
            continue;
        }
        let (flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(key, value)| (key, Some(value)));
        let (arity, keep) = option(flag)?;
        let start = index;
        index += 1;
        match arity {
            Arity::Flag if inline.is_some() => {
                return Err(invalid(
                    "Cannot safely reconstruct a Claude flag with a value",
                ));
            }
            Arity::Required if inline.is_none() => {
                if index == args.len() {
                    return Err(invalid("A recorded Claude option is missing its value"));
                }
                index += 1;
            }
            Arity::Optional if inline.is_none() => {
                if args
                    .get(index)
                    .is_some_and(|value| !value.starts_with('-') || value == "-")
                {
                    index += 1;
                }
            }
            Arity::Many if inline.is_none() => {
                // Claude's bundled parser consumes one required value even
                // when it starts with '-'. Only the later values stop at an
                // option. In contrast, --tools=value does NOT activate
                // variadic consumption: the following operand is the prompt.
                if index == args.len() {
                    return Err(invalid("A recorded Claude option is missing its values"));
                }
                index += 1;
                while args
                    .get(index)
                    .is_some_and(|value| !value.starts_with('-') || value == "-")
                {
                    index += 1;
                }
            }
            _ => {}
        }
        if keep {
            retained.extend_from_slice(&args[start..index]);
        }
    }
    Ok(retained)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn launch(args: &[&str]) -> AgentSpec {
        AgentSpec {
            runtime: "claude-code".into(),
            command: std::iter::once("claude")
                .chain(args.iter().copied())
                .map(str::to_owned)
                .collect(),
            tty: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_conversation_selector_cannot_become_a_provider_option() {
        let cli = tempfile::NamedTempFile::new().unwrap();
        let previous = launch(&["--permission-mode", "plan"]);
        for session in [
            "",
            "--permission-mode=bypassPermissions",
            "-c",
            "private\nselector",
            "private\0selector",
        ] {
            let error =
                reconnect_claude(&previous, session, Path::new("claude"), cli.path()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                error.to_string(),
                "Claude reconnect needs its recorded conversation"
            );
        }
    }

    #[test]
    fn reconnect_preserves_restrictions_values_environment_and_one_channel() {
        let old_cli = tempfile::NamedTempFile::new().unwrap();
        let new_cli = tempfile::NamedTempFile::new().unwrap();
        let mut previous = launch(&[
            "--strict-mcp-config",
            "--permission-mode",
            "dontAsk",
            "--tools",
            "",
            "--allowedTools",
            "mcp__agentdocker__send_message",
            "mcp__agentdocker__list_agents",
            "--disallowed-tools=Write,Edit",
            "--settings",
            "{\"permissions\":{\"defaultMode\":\"plan\"}}",
            "--append-system-prompt",
            "--resume is text here, not a selector",
            "--model",
            "fixture-model",
            "--session-id",
            "old-session",
            "--",
            "initial input must not run again",
        ]);
        previous.env =
            BTreeMap::from([("CLAUDE_CONFIG_DIR".into(), "/private/test-profile".into())]);
        let original_options = previous.command[1..previous.command.len() - 4].to_vec();
        enable_claude_channel(&mut previous, old_cli.path()).unwrap();
        let before = previous.clone();
        let next = reconnect_claude(
            &previous,
            "bound-session",
            Path::new("chosen-claude"),
            new_cli.path(),
        )
        .unwrap();
        assert_eq!(previous, before);
        assert_eq!(next.command[0], "chosen-claude");
        assert_eq!(next.env, previous.env);
        assert_eq!(&next.command[5..next.command.len() - 2], original_options);
        assert_eq!(
            &next.command[next.command.len() - 2..],
            ["--resume", "bound-session"]
        );
        assert_eq!(
            next.command.iter().filter(|a| *a == "--mcp-config").count(),
            1
        );
        assert!(!next.command.iter().any(|a| a.contains("initial input")));
        assert!(
            next.command[2]
                .contains(&serde_json::to_string(&new_cli.path().to_string_lossy()).unwrap())
        );
    }

    #[test]
    fn session_selection_and_forking_never_survive_reconnect() {
        let previous = launch(&[
            "first prompt",
            "-c",
            "--fork-session",
            "-r",
            "old",
            "--resume=other",
            "--session-id=fresh",
            "--from-pr",
            "123",
            "--teleport",
            "remote",
            "--verbose",
        ]);
        assert_eq!(resume_options(&previous).unwrap(), ["--verbose"]);
    }

    #[test]
    fn option_values_are_not_reinterpreted_as_prompts_or_selectors() {
        let previous = launch(&[
            "--system-prompt",
            "--resume",
            "--tools=Read",
            "--debug=hooks",
            "--permission-mode=plan",
            "--",
            "--secret-looking-initial-prompt",
        ]);
        assert_eq!(
            resume_options(&previous).unwrap(),
            previous.command[1..previous.command.len() - 2]
        );
        assert_eq!(
            resume_options(&launch(&["--tools", "", "--resume", "old"])).unwrap(),
            ["--tools", ""]
        );
    }

    #[test]
    fn an_inline_variadic_value_does_not_absorb_the_original_prompt() {
        for flag in [
            "--tools",
            "--allowedTools",
            "--disallowed-tools",
            "--add-dir",
            "--betas",
        ] {
            let inline = format!("{flag}=fixture-value");
            let previous = launch(&[&inline, "original prompt", "--permission-mode", "plan"]);
            assert_eq!(
                resume_options(&previous).unwrap(),
                [inline, "--permission-mode".into(), "plan".into()]
            );
        }
    }

    #[test]
    fn the_first_separate_variadic_value_can_start_with_a_dash() {
        let previous = launch(&[
            "--add-dir",
            "-fixture-dir",
            "second-dir",
            "--permission-mode",
            "plan",
            "--",
            "original prompt",
        ]);
        assert_eq!(
            resume_options(&previous).unwrap(),
            [
                "--add-dir",
                "-fixture-dir",
                "second-dir",
                "--permission-mode",
                "plan"
            ]
        );
    }

    #[test]
    fn ambiguous_launches_refuse_without_disclosing_values_or_mutating_record() {
        for args in [
            vec!["--unknown", "private-value"],
            vec!["--permission-mode"],
            vec!["--tools"],
            vec!["--verbose=private-value"],
            vec!["first", "second"],
            vec!["first", "--", "second"],
            vec!["--", "first", "second"],
            vec!["--mcp-config", "private-value"],
        ] {
            let previous = launch(&args);
            let before = previous.clone();
            let error = resume_options(&previous).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(!error.to_string().contains("private-value"));
            assert_eq!(previous, before);
        }
    }

    #[test]
    fn modified_channel_configuration_cannot_be_silently_replaced() {
        let cli = tempfile::NamedTempFile::new().unwrap();
        let mut previous = launch(&[]);
        enable_claude_channel(&mut previous, cli.path()).unwrap();
        let mut config: serde_json::Value = serde_json::from_str(&previous.command[2]).unwrap();
        config["mcpServers"]["other"] = serde_json::json!({"command":"private-value"});
        previous.command[2] = config.to_string();
        let error = resume_options(&previous).unwrap_err();
        assert!(!error.to_string().contains("private-value"));
    }

    #[test]
    fn externally_registered_session_without_a_command_has_no_options_to_invent() {
        let previous = AgentSpec {
            runtime: "claude-code".into(),
            ..Default::default()
        };
        assert!(resume_options(&previous).unwrap().is_empty());
    }

    #[test]
    fn reconnect_can_enable_a_previously_disabled_channel_without_losing_the_profile() {
        let cli = tempfile::NamedTempFile::new().unwrap();
        let mut previous = launch(&["--permission-mode", "plan"]);
        previous.env.insert(CLAUDE_CHANNEL_ENV.into(), "0".into());
        previous
            .env
            .insert("CLAUDE_CONFIG_DIR".into(), "private-profile".into());
        let resumed =
            reconnect_claude(&previous, "original", Path::new("claude"), cli.path()).unwrap();
        assert_eq!(resumed.env[CLAUDE_CHANNEL_ENV], "1");
        assert_eq!(resumed.env["CLAUDE_CONFIG_DIR"], "private-profile");
        assert_eq!(previous.env[CLAUDE_CHANNEL_ENV], "0");
    }
}
