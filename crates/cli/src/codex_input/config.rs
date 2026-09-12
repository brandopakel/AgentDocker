//! Bind this session's coordination MCP to the supervised identity explicitly.
//! Codex filters the environment passed to stdio MCP servers; inheritance alone
//! otherwise sends their tools to the user's default daemon as a new agent.
use super::ledger::Binding;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn overrides(
    config: &Value,
    binding: &Binding,
    home: &Path,
    cli: &Path,
) -> Result<Value> {
    let servers = config.get("mcp_servers").and_then(Value::as_object);
    let mut names = Vec::new();
    if let Some(servers) = servers {
        for (name, server) in servers {
            if server
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| {
                    Path::new(command)
                        .file_name()
                        .is_some_and(|file| file == "agentdocker")
                })
                && server["args"]
                    .as_array()
                    .is_some_and(|args| args.iter().any(|arg| arg == "mcp"))
            {
                names.push(name.clone());
            }
        }
    }
    if names.is_empty() {
        ensure!(
            servers.is_none_or(|s| !s.contains_key("agentdocker")),
            "the configured agentdocker MCP entry uses another command; its routing must be reviewed before enabling input"
        );
        names.push("agentdocker".into());
    }
    let mut patched = serde_json::Map::new();
    for name in names {
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "AgentDocker MCP server name cannot be bound through Codex configuration overrides"
        );
        let server = servers.and_then(|s| s.get(&name));
        if server.is_some_and(|s| s["enabled"] == false) {
            continue;
        }
        // Override only this integration's executable/identity. Other servers,
        // tools, approval policy, filters and user environment remain intact.
        ensure!(
            server.is_none_or(|s| s.get("url").is_none_or(Value::is_null)),
            "AgentDocker input requires a local stdio MCP entry"
        );
        // Normalized config/read nulls cannot be replayed as TOML values.
        // Leaf overrides preserve the original optional fields and other servers.
        let prefix = format!("mcp_servers.{name}");
        patched.insert(format!("{prefix}.command"), json!(cli));
        patched.insert(
            format!("{prefix}.args"),
            json!(["mcp", "--runtime", "codex"]),
        );
        for (name, value) in [
            ("AGENTDOCKER_HOME", json!(home)),
            ("AGENTDOCKER_SOCKET", json!(binding.socket)),
            ("AGENTDOCKER_AGENT_ID", json!(binding.agent)),
            ("AGENTDOCKER_NO_AUTOSTART", json!("1")),
            (
                agentdocker_host::provider_input::CODEX_INPUT_ENV,
                json!("1"),
            ),
        ] {
            patched.insert(format!("{prefix}.env.{name}"), value);
        }
    }
    Ok(Value::Object(patched))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_mcp_binding_preserves_policy_and_other_servers() {
        let binding = Binding {
            agent: "owned".into(),
            socket: "/private/socket".into(),
            cwd: "/checkout".into(),
            provider_home: "/provider".into(),
        };
        let config = json!({"mcp_servers":{
            "coordination":{"command":"/old/agentdocker","args":["mcp","--runtime","codex"],
                "env":{"OWN_SETTING":"retained","AGENTDOCKER_SOCKET":"/wrong"},
                "tools":{"send_message":{"approval_mode":"prompt"}},"enabled_tools":["send_message"]},
            "other":{"command":"/unrelated","env":{"SETTING":"value"}}
        }});
        let before = config.clone();
        let result = overrides(
            &config,
            &binding,
            Path::new("/state"),
            Path::new("/current/agentdocker"),
        )
        .unwrap();
        assert_eq!(
            result["mcp_servers.coordination.command"],
            "/current/agentdocker"
        );
        assert_eq!(
            result["mcp_servers.coordination.env.AGENTDOCKER_AGENT_ID"],
            "owned"
        );
        assert_eq!(
            result["mcp_servers.coordination.env.AGENTDOCKER_SOCKET"],
            "/private/socket"
        );
        assert_eq!(
            result["mcp_servers.coordination.env.AGENTDOCKER_NO_AUTOSTART"],
            "1"
        );
        assert_eq!(result.as_object().unwrap().len(), 7);
        assert!(
            result
                .as_object()
                .unwrap()
                .keys()
                .all(|key| key.starts_with("mcp_servers.coordination."))
        );
        assert_eq!(config, before);
        let mut disabled = config.clone();
        disabled["mcp_servers"]["coordination"]["enabled"] = json!(false);
        assert!(
            overrides(&disabled, &binding, Path::new("/state"), Path::new("/cli"))
                .unwrap()
                .as_object()
                .unwrap()
                .is_empty()
        );
        assert!(
            overrides(
                &json!({"mcp_servers":{"agentdocker":{"command":"custom-wrapper"}}}),
                &binding,
                Path::new("/state"),
                Path::new("/cli")
            )
            .is_err()
        );
    }
}
