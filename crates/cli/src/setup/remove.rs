//! What `setup --remove` takes out of a provider's configuration: only
//! AgentDocker's own entries, recognised as setup and health recognise them
//! (the command an entry runs, never a name alone), and nothing else in the
//! same file. Each is a pure change of a document's text or value; the plan
//! that carries them previews, applies and undoes like any setup plan.

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Take our hook objects out of a hooks document (Claude Code's settings,
/// Codex's `hooks.json`): in every event, every group's hooks that run
/// `agentdocker hook <runtime>`. A group left with no hooks goes, an event
/// left with no groups goes, and `hooks` left empty goes. Other tools'
/// hooks, and theirs beside ours in one group, stay. How many were taken.
pub(super) fn without_hooks(document: &mut Value, runtime: &str) -> Result<usize> {
    let Some(root) = document.as_object_mut() else {
        bail!("settings must be a JSON object");
    };
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(0);
    };
    let hooks = hooks
        .as_object_mut()
        .context("`hooks` must be a JSON object")?;
    let ours = |hook: &Value| {
        hook["type"] == "command"
            && hook["command"].as_str().is_some_and(|command| {
                agentdocker_host::runtimes::hook_command_matches_for(
                    command,
                    "agentdocker",
                    runtime,
                )
            })
    };
    let mut taken = 0;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|hook| !ours(hook));
                taken += before - list.len();
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|list| !list.is_empty())
        });
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|groups| !groups.is_empty()));
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(taken)
}

/// Take `<key>.agentdocker` out of a JSON MCP document when it runs us. An
/// entry under our name that runs something else is not ours to remove.
pub(super) fn without_json_server(document: &mut Value, key: &str, runtime: &str) -> Result<bool> {
    let Some(root) = document.as_object_mut() else {
        bail!("configuration must be a JSON object");
    };
    let Some(servers) = root.get_mut(key) else {
        return Ok(false);
    };
    let servers = servers
        .as_object_mut()
        .with_context(|| format!("`{key}` must be a JSON object"))?;
    let ours = servers
        .get("agentdocker")
        .is_some_and(|server| super::runs_agentdocker(server, runtime));
    if ours {
        servers.remove("agentdocker");
    }
    Ok(ours)
}

/// Take OpenCode's `mcp.agentdocker` out when it runs us.
pub(super) fn without_opencode_server(document: &mut Value) -> Result<bool> {
    let spec = agentdocker_core::runtime::spec("opencode").expect("opencode is in the catalog");
    let Some(root) = document.as_object_mut() else {
        bail!("configuration must be a JSON object");
    };
    let Some(servers) = root.get_mut("mcp") else {
        return Ok(false);
    };
    let servers = servers
        .as_object_mut()
        .context("`mcp` must be a JSON object")?;
    let ours = servers.get("agentdocker").is_some_and(|server| {
        agentdocker_host::runtimes::server_launch(spec, server).is_some_and(|(command, args)| {
            agentdocker_host::runtimes::mcp_command_matches(
                command,
                &args,
                "agentdocker",
                "opencode",
            )
        })
    });
    if ours {
        servers.remove("agentdocker");
    }
    Ok(ours)
}

/// Take `[mcp_servers.agentdocker]`, and any table under it, out of TOML
/// text when the entry runs us, leaving every other line as it was. Done by
/// lines, as setup appends it; the result must parse to exactly the
/// document without that entry, or nothing is changed and the person is
/// asked to remove it by hand. `None` when there is nothing of ours.
pub(super) fn without_toml_server(text: &str, runtime: &str) -> Result<Option<String>> {
    let parsed: toml::Table = text.parse().context("not valid TOML")?;
    let Some(entry) = parsed
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .and_then(|servers| servers.get("agentdocker"))
    else {
        return Ok(None);
    };
    let command = entry.get("command").and_then(toml::Value::as_str);
    let args: Vec<&str> = entry
        .get("args")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .collect();
    if !agentdocker_host::runtimes::mcp_command_matches(command, &args, "agentdocker", runtime) {
        return Ok(None);
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let header = |line: &str| {
        let line = line.trim_start();
        line.starts_with('[') && !line.starts_with("[[")
    };
    let ours = |line: &str| {
        let line = line.trim();
        let name = line.split('#').next().unwrap_or(line).trim();
        let name = name.trim_start_matches('[').trim_end_matches(']').trim();
        name == "mcp_servers.agentdocker" || name.starts_with("mcp_servers.agentdocker.")
    };
    let blank = |line: &str| line.trim().is_empty();
    let quiet = |line: &str| blank(line) || line.trim_start().starts_with('#');
    let mut keep = vec![true; lines.len()];
    let mut at = 0;
    while at < lines.len() {
        if !(header(lines[at]) && ours(lines[at])) {
            at += 1;
            continue;
        }
        // Our tables run from this header to their last key line before a
        // table that is not ours: a comment or blank line before that one
        // belongs to it, and stays.
        let mut last = at;
        let mut next = at + 1;
        while next < lines.len() {
            if header(lines[next]) && !ours(lines[next]) {
                break;
            }
            if !quiet(lines[next]) {
                last = next;
            }
            next += 1;
        }
        keep[at..=last].fill(false);
        // The one blank line setup puts before our table goes with it, when
        // keeping it would leave two blank lines together or end the file.
        let after_blank = lines.get(last + 1).is_none_or(|line| blank(line));
        if at > 0 && keep[at - 1] && blank(lines[at - 1]) && after_blank {
            keep[at - 1] = false;
        }
        at = last + 1;
    }
    let after: String = lines
        .iter()
        .zip(&keep)
        .filter(|(_, kept)| **kept)
        .map(|(line, _)| *line)
        .collect();
    // What the lines left must mean: the document, less our entry.
    let mut expected = parsed.clone();
    if let Some(servers) = expected
        .get_mut("mcp_servers")
        .and_then(toml::Value::as_table_mut)
    {
        servers.remove("agentdocker");
    }
    let empty = |table: &toml::Table| {
        table
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
            .is_some_and(toml::Table::is_empty)
    };
    let mut actual: toml::Table = after
        .parse()
        .context("removing the agentdocker table left TOML that does not parse")?;
    for table in [&mut expected, &mut actual] {
        if empty(table) {
            table.remove("mcp_servers");
        }
    }
    if actual != expected {
        bail!(
            "the agentdocker entry is not one table setup wrote; remove `[mcp_servers.agentdocker]` by hand"
        );
    }
    Ok(Some(after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOOK: &str = "/usr/local/bin/agentdocker hook claude-code";

    #[test]
    fn only_our_hooks_go_and_others_stay_where_they_were() {
        let mut settings = json!({
            "model": "opus",
            "hooks": {
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": HOOK, "timeout": 15}]}
                ],
                "PostToolUse": [
                    {"matcher": "*", "hooks": [
                        {"type": "command", "command": HOOK},
                        {"type": "command", "command": "rtk audit"}
                    ]}
                ],
                "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]
            }
        });
        assert_eq!(without_hooks(&mut settings, "claude-code").unwrap(), 2);
        assert_eq!(
            settings,
            json!({
                "model": "opus",
                "hooks": {
                    "PostToolUse": [
                        {"matcher": "*", "hooks": [{"type": "command", "command": "rtk audit"}]}
                    ],
                    "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]
                }
            })
        );
        // Ours alone: `hooks` goes with them.
        let mut only =
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": HOOK}]}]}});
        assert_eq!(without_hooks(&mut only, "claude-code").unwrap(), 1);
        assert_eq!(only, json!({}));
        // Another runtime's hook of ours is not this runtime's to take.
        let mut codex = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "agentdocker hook codex"}]}]}});
        assert_eq!(without_hooks(&mut codex, "claude-code").unwrap(), 0);
    }

    #[test]
    fn a_json_server_goes_only_when_it_runs_us() {
        let mut config = json!({"mcpServers": {
            "agentdocker": {"command": "/usr/local/bin/agentdocker", "args": ["mcp", "--runtime", "cursor"]},
            "other": {"command": "npx", "args": ["other"]}
        }});
        assert!(without_json_server(&mut config, "mcpServers", "cursor").unwrap());
        assert_eq!(
            config,
            json!({"mcpServers": {"other": {"command": "npx", "args": ["other"]}}})
        );
        let mut squatted =
            json!({"mcpServers": {"agentdocker": {"command": "npx", "args": ["something"]}}});
        assert!(!without_json_server(&mut squatted, "mcpServers", "cursor").unwrap());
        assert!(
            squatted["mcpServers"]["agentdocker"].is_object(),
            "not ours, left"
        );
    }

    #[test]
    fn opencodes_entry_goes_only_when_it_runs_us() {
        let mut config = json!({"mcp": {
            "agentdocker": {"type": "local", "command": ["/usr/local/bin/agentdocker", "mcp", "--runtime", "opencode"], "enabled": true}
        }, "theme": "dark"});
        assert!(without_opencode_server(&mut config).unwrap());
        assert_eq!(config, json!({"mcp": {}, "theme": "dark"}));
    }

    #[test]
    fn our_toml_table_goes_and_every_other_line_stays() {
        let text = "model = \"gpt-6-astra\"\n# my servers\n[mcp_servers.other]\ncommand = \"npx\"\n\n[mcp_servers.agentdocker]\ncommand = \"/usr/local/bin/agentdocker\"\nargs = [\"mcp\", \"--runtime\", \"codex\"]\n\n[mcp_servers.agentdocker.env]\nX = \"1\"\n\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n";
        let after = without_toml_server(text, "codex").unwrap().unwrap();
        assert_eq!(
            after,
            "model = \"gpt-6-astra\"\n# my servers\n[mcp_servers.other]\ncommand = \"npx\"\n\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n"
        );
        // A comment that introduces the next table stays with it, and blank
        // lines that are not ours stay as they were.
        let commented = "[mcp_servers.agentdocker]\ncommand = \"agentdocker\"\nargs = [\"mcp\", \"--runtime\", \"codex\"]\n\n# why this project is trusted\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n\n\n";
        assert_eq!(
            without_toml_server(commented, "codex").unwrap().unwrap(),
            "\n# why this project is trusted\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n\n\n"
        );
        // Appended last, as setup writes it: nothing left trailing.
        let appended = "model = \"x\"\n\n[mcp_servers.agentdocker]\ncommand = \"agentdocker\"\nargs = [\"mcp\", \"--runtime\", \"codex\"]\n";
        assert_eq!(
            without_toml_server(appended, "codex").unwrap().unwrap(),
            "model = \"x\"\n"
        );
        // Whatever setup appended to, removal gives back, trailing blank
        // lines and comments included.
        for original in [
            "model = \"x\"\n",
            "model = \"x\"\n\n\n",
            "model = \"x\"\n# the end\n\n",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("config.toml");
            std::fs::write(&path, original).unwrap();
            super::super::register_toml(&path, &tmp.path().join("agentdocker"), "codex", false)
                .unwrap();
            let wired = std::fs::read_to_string(&path).unwrap();
            assert_ne!(wired, original);
            assert_eq!(
                without_toml_server(&wired, "codex").unwrap().as_deref(),
                Some(original),
                "{wired:?}"
            );
        }
        // Not ours, or not there: nothing to do.
        assert_eq!(
            without_toml_server(
                "[mcp_servers.agentdocker]\ncommand = \"npx\"\nargs = []\n",
                "codex"
            )
            .unwrap(),
            None
        );
        assert_eq!(
            without_toml_server("model = \"x\"\n", "codex").unwrap(),
            None
        );
        // Written another way (dotted keys at the root): left to the person.
        let dotted = "mcp_servers.agentdocker.command = \"agentdocker\"\nmcp_servers.agentdocker.args = [\"mcp\", \"--runtime\", \"codex\"]\n";
        assert!(without_toml_server(dotted, "codex").is_err());
    }
}
