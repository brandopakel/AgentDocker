//! Read-only configuration/executable diagnostics. Never run provider commands
//! or include configuration snapshots, arguments or environment values in reports.

use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use agentdocker_core::runtime::{McpWiring, RuntimeSpec};
use serde::Serialize;
use serde_json::Value;

use super::{
    Roots, hook_command_matches, hooks_configuration_matches, mcp_command_matches, mcp_config_path,
    which,
};

const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;

/// Whether a path names something this host would run.
///
/// Two different questions on the two platforms. Unix asks the file
/// whether anybody may execute it; Windows has no such bit and decides
/// from the extension, which is why `PATHEXT` exists. Asking the Unix
/// question on Windows would report every provider as unavailable.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.BAT;.CMD;.COM".into());
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|found| {
                extensions
                    .split(';')
                    .any(|want| want.trim_start_matches('.').eq_ignore_ascii_case(found))
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Unsupported,
    Missing,
    Invalid,
    Disabled,
    Unverified,
    Incomplete,
    ExecutableMissing,
    ExecutableAvailable,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub channel: &'static str,
    pub configuration: Option<PathBuf>,
    pub status: Status,
    pub executable: Option<PathBuf>,
    pub detail: String,
}

impl Check {
    fn new(channel: &'static str, path: Option<&Path>, status: Status, detail: &str) -> Self {
        Self {
            channel,
            configuration: path.map(Path::to_owned),
            status,
            executable: None,
            detail: detail.into(),
        }
    }
}

/// Bound reads and reject special files before attempting to consume their data.
/// Provider configuration may be an intentional symlink; follow its file target.
pub(super) fn read_configuration(path: &Path) -> io::Result<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // Opening a FIFO blocks until somebody writes to it, and provider
    // configuration is a path we did not choose. `O_NONBLOCK` makes the
    // open return instead, and the regular-file check below rejects it.
    // Windows has no FIFOs to open by accident and no such flag.
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration must be a regular file no larger than 8 MiB",
        ));
    }
    let mut text = String::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration grew beyond 8 MiB",
        ));
    }
    Ok(text)
}

/// Inspect supported adapters without starting a daemon, provider or configured
/// executable. Availability is a prerequisite, not a successful MCP/hook call.
pub fn inspect(spec: &RuntimeSpec, roots: &Roots, marker: &str) -> Vec<Check> {
    let mut checks = mcp(spec, roots, marker);
    checks.extend(hooks(spec, roots, marker));
    checks
}

pub(super) fn configuration(
    channel: &'static str,
    path: &Path,
    toml: bool,
) -> Result<Value, Check> {
    let raw = read_configuration(path).map_err(|error| {
        Check::new(
            channel,
            Some(path),
            if error.kind() == io::ErrorKind::NotFound {
                Status::Missing
            } else {
                Status::Invalid
            },
            if error.kind() == io::ErrorKind::NotFound {
                "Configuration file is absent"
            } else {
                "Configuration cannot be read as a bounded regular UTF-8 file"
            },
        )
    })?;
    // TOML's Display error can include source text (including secrets). Use a
    // fixed diagnostic for both parsers instead of echoing their errors.
    if toml {
        raw.parse::<toml::Table>()
            .ok()
            .and_then(|table| serde_json::to_value(table).ok())
    } else {
        serde_json::from_str(&raw).ok()
    }
    .ok_or_else(|| {
        Check::new(
            channel,
            Some(path),
            Status::Invalid,
            if toml {
                "Configuration is not valid TOML"
            } else {
                "Configuration is not valid JSON"
            },
        )
    })
}

fn mcp(spec: &RuntimeSpec, roots: &Roots, marker: &str) -> Vec<Check> {
    let Some(path) = mcp_config_path(spec, roots) else {
        return vec![Check::new(
            "mcp",
            None,
            Status::Unsupported,
            "No supported MCP setup adapter",
        )];
    };
    let is_toml = matches!(spec.mcp, McpWiring::TomlServers { .. });
    let value = match configuration("mcp", &path, is_toml) {
        Ok(value) => value,
        Err(check) => return vec![check],
    };
    let key = if is_toml { "mcp_servers" } else { "mcpServers" };
    if !value.is_object() || value.get(key).is_some_and(|servers| !servers.is_object()) {
        return vec![Check::new(
            "mcp",
            Some(&path),
            Status::Unverified,
            "Configuration has an invalid MCP registration container; preserved",
        )];
    }
    let servers = value[key].as_object();
    let mut checks = Vec::new();
    if let Some(servers) = servers {
        for (name, server) in servers {
            let command = server["command"].as_str();
            let arguments: Option<Vec<_>> = server["args"]
                .as_array()
                .and_then(|args| args.iter().map(Value::as_str).collect());
            if !arguments
                .as_ref()
                .is_some_and(|args| mcp_command_matches(command, args, marker, spec.name))
            {
                if name == marker {
                    checks.push(Check::new(
                        "mcp",
                        Some(&path),
                        Status::Unverified,
                        "Reserved registration launches an unrecognized command; preserved",
                    ));
                }
                continue;
            }
            if server["enabled"] == false || server["disabled"] == true {
                checks.push(Check::new(
                    "mcp",
                    Some(&path),
                    Status::Disabled,
                    "Configured MCP registration is disabled",
                ));
            } else {
                checks.push(executable(
                    "mcp",
                    &path,
                    command.unwrap(),
                    server["cwd"].as_str(),
                    roots,
                ));
            }
        }
    }
    if checks.is_empty() {
        checks.push(Check::new(
            "mcp",
            Some(&path),
            Status::Missing,
            "No recognized agentdocker MCP registration",
        ));
    }
    checks
}

fn hooks(spec: &RuntimeSpec, roots: &Roots, marker: &str) -> Vec<Check> {
    if !spec.hooks {
        return vec![Check::new(
            "hooks",
            None,
            Status::Unsupported,
            "No supported hooks adapter",
        )];
    }
    let path = roots.home.join(".claude/settings.json");
    let value = match configuration("hooks", &path, false) {
        Ok(value) => value,
        Err(check) => return vec![check],
    };
    if value["disableAllHooks"] == true {
        return vec![Check::new(
            "hooks",
            Some(&path),
            Status::Disabled,
            "User settings disable hooks; project or managed settings may differ",
        )];
    }
    let mut checks = Vec::new();
    if !hooks_configuration_matches(&value, marker) {
        checks.push(Check::new(
            "hooks",
            Some(&path),
            Status::Incomplete,
            "Required hook events or matchers are missing",
        ));
    }
    let mut commands = std::collections::BTreeSet::new();
    if let Some(events) = value["hooks"].as_object() {
        for entries in events.values().filter_map(Value::as_array) {
            for entry in entries {
                for hook in entry["hooks"].as_array().into_iter().flatten() {
                    if hook["type"] == "command"
                        && let Some(command) = hook["command"]
                            .as_str()
                            .filter(|command| hook_command_matches(command, marker))
                        && let Some(words) = shlex::split(command)
                    {
                        commands.insert(words[0].clone());
                    }
                }
            }
        }
    }
    for command in commands {
        checks.push(executable("hooks", &path, &command, None, roots));
    }
    checks
}

fn executable(
    channel: &'static str,
    config: &Path,
    command: &str,
    cwd: Option<&str>,
    roots: &Roots,
) -> Check {
    let path = Path::new(command);
    let (resolved, detail) = if path.is_absolute() {
        (
            Some(path.to_owned()),
            "Configured executable is available; provider connection remains untested",
        )
    } else if path.components().count() == 1 {
        (
            which(roots, command),
            "Executable found on this process's PATH; the provider's environment may differ",
        )
    } else if let Some(cwd) = cwd.map(Path::new).filter(|cwd| cwd.is_absolute()) {
        (
            Some(cwd.join(path)),
            "Executable found relative to configured working directory; provider connection remains untested",
        )
    } else {
        return Check::new(
            channel,
            Some(config),
            Status::Unverified,
            "Relative command requires the provider's working directory to resolve",
        );
    };
    let available = resolved.as_ref().is_some_and(|path| is_executable(path));
    let mut check = Check::new(
        channel,
        Some(config),
        if available {
            Status::ExecutableAvailable
        } else {
            Status::ExecutableMissing
        },
        if available {
            detail
        } else {
            "Configured executable is missing, inaccessible or not executable"
        },
    );
    check.executable = resolved;
    check
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::runtime::{CLAUDE_CODE_HOOKS, RUNTIMES};
    use serde_json::json;

    fn machine() -> (tempfile::TempDir, Roots) {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().to_owned();
        let roots = Roots {
            home: home.clone(),
            codex_home: None,
            path: vec![home.join("bin")],
            app_dirs: vec![],
            install_dirs: vec![],
            desktop_dirs: vec![],
            versions: false,
        };
        std::fs::create_dir_all(home.join("bin")).unwrap();
        (temporary, roots)
    }

    fn spec(name: &str) -> &'static RuntimeSpec {
        RUNTIMES.iter().find(|spec| spec.name == name).unwrap()
    }

    /// An absolute path to something that is not there.
    ///
    /// Absolute matters: the check answers "no working directory to
    /// resolve this against" for a relative command and only reaches
    /// the does-it-exist question for an absolute one. `/absent/...`
    /// is absolute on Unix and *relative* on Windows, where absolute
    /// means a drive letter or a UNC prefix.
    fn absent() -> &'static str {
        if cfg!(windows) {
            r"C:\absent\agentdocker"
        } else {
            "/absent/agentdocker"
        }
    }

    fn write_mcp(roots: &Roots, command: &str, extra: Value) {
        let path = roots.home.join(".gemini/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut entry = json!({"command":command, "args":["mcp"]});
        entry
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        std::fs::write(
            path,
            json!({"mcpServers":{"agentdocker":entry}}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn malformed_mcp_containers_are_unverified_in_both_inventory_and_health() {
        let (_temporary, roots) = machine();
        for (name, cases) in [
            (
                "gemini-cli",
                vec!["[]", r#"{"mcpServers":[]}"#, r#"{"mcpServers":null}"#],
            ),
            ("codex", vec!["mcp_servers = []", "mcp_servers = false"]),
        ] {
            let spec = spec(name);
            let path = mcp_config_path(spec, &roots).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            for raw in cases {
                std::fs::write(&path, raw).unwrap();
                assert_eq!(
                    super::super::mcp_wiring(spec, &roots, "agentdocker"),
                    agentdocker_core::runtime::Wiring::Unverified,
                    "{raw}"
                );
                let checks = inspect(spec, &roots, "agentdocker");
                assert_eq!(checks[0].status, Status::Unverified, "{raw}");
                assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
            }
        }
    }

    #[test]
    // sets the execute bit directly, which Windows does not have.
    #[cfg(unix)]
    fn configured_missing_non_executable_and_available_are_distinct_without_execution() {
        let (_temporary, roots) = machine();
        let executable = roots.home.join("bin/agentdocker");
        write_mcp(&roots, executable.to_str().unwrap(), json!({}));
        let check = || mcp(spec("gemini-cli"), &roots, "agentdocker").remove(0);
        assert_eq!(check().status, Status::ExecutableMissing);
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\ntouch '{}'\n",
                roots.home.join("must-not-run").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(check().status, Status::ExecutableMissing);
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(check().status, Status::ExecutableAvailable);
        assert!(!roots.home.join("must-not-run").exists());
        std::fs::remove_file(executable).unwrap();
        assert_eq!(check().status, Status::ExecutableMissing);
    }

    #[test]
    fn disabled_and_wrapped_registrations_are_not_reported_as_available() {
        let (_temporary, roots) = machine();
        write_mcp(&roots, absent(), json!({"disabled":true}));
        assert_eq!(
            mcp(spec("gemini-cli"), &roots, "agentdocker")[0].status,
            Status::Disabled
        );
        write_mcp(&roots, "sh", json!({"args":["-c","agentdocker mcp"]}));
        assert_eq!(
            mcp(spec("gemini-cli"), &roots, "agentdocker")[0].status,
            Status::Unverified
        );
    }

    #[test]
    fn mcp_arguments_must_match_a_supported_direct_launch() {
        let (_temporary, roots) = machine();
        let runtime = spec("gemini-cli");
        for args in [
            json!(["mcp", "--unexpected"]),
            json!(["mcp", "--help"]),
            json!(["mcp", "--runtime"]),
            json!(["mcp", "--runtime", "--unexpected"]),
            json!(["mcp", "--runtime", "gemini-cli", "extra"]),
            json!(["mcp", "--runtime", "codex"]),
            json!(["mcp", "--runtime", "unknown-runtime"]),
        ] {
            write_mcp(&roots, absent(), json!({"args": args}));
            assert_eq!(
                mcp(runtime, &roots, "agentdocker")[0].status,
                Status::Unverified
            );
            assert_eq!(
                super::super::mcp_wiring(runtime, &roots, "agentdocker"),
                agentdocker_core::runtime::Wiring::Unverified
            );
        }
        // The generated setup registration carries its provider runtime.
        for args in [json!(["mcp"]), json!(["mcp", "--runtime", "gemini-cli"])] {
            write_mcp(&roots, absent(), json!({"args": args}));
            assert_eq!(
                mcp(runtime, &roots, "agentdocker")[0].status,
                Status::ExecutableMissing
            );
            assert_eq!(
                super::super::mcp_wiring(runtime, &roots, "agentdocker"),
                agentdocker_core::runtime::Wiring::Wired
            );
        }
    }

    #[test]
    fn codex_override_and_invalid_configuration_do_not_expose_config_values() {
        let (_temporary, mut roots) = machine();
        roots.codex_home = Some(roots.home.join("codex-custom"));
        std::fs::create_dir(roots.codex_home.as_ref().unwrap()).unwrap();
        let path = roots.codex_home.as_ref().unwrap().join("config.toml");
        std::fs::write(&path, "secret_value = private-test-sentinel\n").unwrap();
        let checks = mcp(spec("codex"), &roots, "agentdocker");
        assert_eq!(checks[0].status, Status::Invalid);
        assert_eq!(checks[0].configuration.as_ref(), Some(&path));
        assert!(
            !serde_json::to_string(&checks)
                .unwrap()
                .contains("private-test-sentinel")
        );
        std::fs::write(
            path,
            "[mcp_servers.agentdocker]\ncommand='agentdocker'\nargs=['mcp']\nenabled=false\n",
        )
        .unwrap();
        assert_eq!(
            mcp(spec("codex"), &roots, "agentdocker")[0].status,
            Status::Disabled
        );
    }

    #[test]
    // sets the execute bit directly, which Windows does not have.
    #[cfg(unix)]
    fn relative_paths_require_a_known_working_directory() {
        let (_temporary, roots) = machine();
        let executable = roots.home.join("bin/agentdocker");
        std::fs::write(&executable, "fixture").unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        write_mcp(&roots, "./bin/agentdocker", json!({}));
        assert_eq!(
            mcp(spec("gemini-cli"), &roots, "agentdocker")[0].status,
            Status::Unverified
        );
        write_mcp(&roots, "./bin/agentdocker", json!({"cwd":roots.home}));
        assert_eq!(
            mcp(spec("gemini-cli"), &roots, "agentdocker")[0].status,
            Status::ExecutableAvailable
        );
        write_mcp(&roots, "agentdocker", json!({}));
        assert!(
            mcp(spec("gemini-cli"), &roots, "agentdocker")[0]
                .detail
                .contains("environment may differ")
        );
    }

    #[test]
    fn hook_coverage_does_not_hide_an_unavailable_quoted_executable() {
        let (_temporary, roots) = machine();
        let executable = roots.home.join("path with spaces/agentdocker");
        let command = super::super::claude_hook_command(&executable).unwrap();
        let mut events = serde_json::Map::new();
        for (event, matcher) in CLAUDE_CODE_HOOKS {
            let mut entry = json!({"hooks":[{"type":"command", "command":command}]});
            if let Some(matcher) = matcher {
                entry["matcher"] = json!(matcher);
            }
            events.insert((*event).into(), json!([entry]));
        }
        let path = roots.home.join(".claude/settings.json");
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, json!({"hooks":events}).to_string()).unwrap();
        let checks = hooks(spec("claude-code"), &roots, "agentdocker");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::ExecutableMissing);
        assert_eq!(checks[0].executable.as_ref(), Some(&executable));
        std::fs::write(
            path,
            json!({"hooks":events, "disableAllHooks":true}).to_string(),
        )
        .unwrap();
        assert_eq!(
            hooks(spec("claude-code"), &roots, "agentdocker")[0].status,
            Status::Disabled
        );
        assert_eq!(
            super::super::hooks_wiring(spec("claude-code"), &roots.home, "agentdocker"),
            agentdocker_core::runtime::Wiring::Missing
        );
    }

    #[test]
    // makes a FIFO, which Windows has no equivalent of.
    #[cfg(unix)]
    fn oversized_and_special_files_are_refused_without_waiting_for_a_writer() {
        let (_temporary, roots) = machine();
        let oversized = roots.home.join("oversized");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len(MAX_CONFIG_BYTES + 1)
            .unwrap();
        assert!(read_configuration(&oversized).is_err());
        let fifo = roots.home.join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated path owned by this fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_configuration(&fifo).is_err());
    }
}
