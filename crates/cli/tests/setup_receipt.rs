//! The ordinary CLI must leave the same reviewable, undoable receipt as the app.
//! Every subprocess uses private provider/state homes and cannot start a daemon.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("CODEX_HOME", root.join("codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("AGENTDOCKER_HOME", root.join("state"))
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .env("AGENTDOCKER_NO_NOTIFICATIONS", "1")
        .env("SHELL", "/bin/zsh")
        .env("PATH", root.join("empty-bin"))
        .output()
        .unwrap()
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn value(root: &Path, args: &[&str]) -> Value {
    serde_json::from_slice(&success(cli(root, args)).stdout).unwrap()
}

#[test]
fn plain_setup_is_reviewable_idempotent_and_undo_preserves_user_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("codex")).unwrap();
    let config = root.join("codex/config.toml");
    let before = "# Keep this comment\nmodel = 'example'\n[mcp_servers.other]\ncommand = 'private-test-secret'\n";
    fs::write(&config, before).unwrap();

    let first = success(cli(root, &["setup", "codex"]));
    let id = String::from_utf8(first.stdout).unwrap().trim().to_owned();
    uuid::Uuid::parse_str(&id).unwrap();
    let stderr = String::from_utf8(first.stderr).unwrap();
    assert!(stderr.contains(&format!("setup --undo {id}")));
    assert!(!stderr.contains("private-test-secret"));
    let shown = value(root, &["setup", "--show", &id, "--json"]);
    assert_eq!(shown["phase"], "applied");
    assert!(!shown.to_string().contains("private-test-secret"));
    let receipt = root.join("state/setup").join(format!("{id}.json"));
    assert_eq!(
        fs::metadata(receipt).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(root.join("state/setup"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let after = fs::read_to_string(&config).unwrap();
    assert!(after.contains("mcp_servers.agentdocker"));
    assert!(after.contains("private-test-secret"));
    let hooks = root.join("codex/hooks.json");
    let skill = root.join("codex/skills/agentdocker/SKILL.md");
    let installed_hooks = fs::read(&hooks).unwrap();
    let installed_skill = fs::read(&skill).unwrap();

    // A repeated install owns no earlier writes and must not remove them.
    let repeat = value(root, &["setup", "codex", "--json"]);
    assert_eq!(repeat["phase"], "applied");
    assert!(repeat["changes"].as_array().unwrap().is_empty());
    value(
        root,
        &["setup", "--undo", repeat["id"].as_str().unwrap(), "--json"],
    );
    assert_eq!(fs::read_to_string(&config).unwrap(), after);
    assert_eq!(fs::read(&hooks).unwrap(), installed_hooks);
    assert_eq!(fs::read(&skill).unwrap(), installed_skill);

    let edited = format!("{after}\n# The person's later edit\n");
    fs::write(&config, &edited).unwrap();
    let refused = cli(root, &["setup", "--undo", &id]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("retained for review"));
    assert_eq!(fs::read_to_string(&config).unwrap(), edited);
    assert_eq!(fs::read(&hooks).unwrap(), installed_hooks);
    assert_eq!(fs::read(&skill).unwrap(), installed_skill);
    assert_eq!(
        value(root, &["setup", "--show", &id, "--json"])["phase"],
        "applied"
    );

    // Exact undo after restoring the expected state; unrelated settings survive.
    fs::write(&config, after).unwrap();
    assert_eq!(
        value(root, &["setup", "--undo", &id, "--json"])["phase"],
        "undone"
    );
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
    assert!(!hooks.exists());
    assert!(!skill.exists());
}

#[test]
fn invalid_plain_setup_does_not_partially_install_hooks_or_skills() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("codex")).unwrap();
    let config = root.join("codex/config.toml");
    fs::write(&config, "[unfinished").unwrap();
    let output = cli(root, &["setup", "codex", "--json"]);
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(config).unwrap(), "[unfinished");
    assert!(!root.join("codex/hooks.json").exists());
    assert!(!root.join("codex/skills/agentdocker/SKILL.md").exists());
    assert!(
        value(root, &["setup", "--list", "--json"])["plans"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn plain_claude_setup_refuses_unverified_mcp_before_changing_hooks() {
    for contents in [
        "{invalid",
        r#"{"mcpServers":{"agentdocker":{"command":"somebody-elses-server"}}}"#,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("claude")).unwrap();
        let config = root.join("claude/.claude.json");
        fs::write(&config, contents).unwrap();
        let output = cli(root, &["setup", "claude-code", "--json"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("MCP configuration needs review"));
        assert_eq!(fs::read_to_string(&config).unwrap(), contents);
        assert!(!root.join("claude/settings.json").exists());
        assert!(!root.join("claude/skills/agentdocker/SKILL.md").exists());
    }
}

#[test]
fn shell_setup_and_explicit_preview_still_wait_for_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let shell = value(root, &["setup", "--shell", "--json"]);
    assert_eq!(shell["phase"], "prepared");
    assert!(!root.join(".zshrc").exists());
    let provider = value(root, &["setup", "codex", "--preview", "--json"]);
    assert_eq!(provider["phase"], "prepared");
    assert!(!root.join("codex/config.toml").exists());
    assert!(!root.join("codex/hooks.json").exists());
    assert!(!root.join("codex/skills/agentdocker/SKILL.md").exists());
    assert!(
        !cli(root, &["setup", "codex", "--dry-run", "--json"])
            .status
            .success()
    );
}
