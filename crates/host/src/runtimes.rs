//! What agent tools are installed on this machine, and whether AgentDocker
//! is wired into each: the looking behind `agentdocker runtimes`. The
//! table of what to look for lives in core; this module only consults the
//! filesystem, `PATH`, and each tool's own configuration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::runtime::{McpWiring, RUNTIMES, RuntimeInfo, RuntimeSpec, Wiring};

use crate::command;

mod desktop;
pub mod health;

/// Where to look: injectable so tests can build a machine in a temp dir.
#[derive(Clone, Debug)]
pub struct Roots {
    pub home: PathBuf,
    /// Explicit Codex host configuration root, when set by the caller.
    pub codex_home: Option<PathBuf>,
    /// `PATH`, split.
    pub path: Vec<PathBuf>,
    /// Standard installation directories, used for CLI inventory only.
    pub install_dirs: Vec<PathBuf>,
    /// Linux desktop-entry directories in XDG precedence order.
    pub desktop_dirs: Vec<PathBuf>,
    /// Where macOS app bundles live; injectable on other hosts for tests.
    pub app_dirs: Vec<PathBuf>,
    /// Ask each CLI for its version; off in tests that only lay out files.
    pub versions: bool,
}

impl Roots {
    pub fn from_env() -> Self {
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let path = std::env::var_os("PATH")
            .map(|p| {
                std::env::split_paths(&p)
                    .filter(|p| p.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        let app_dirs = if cfg!(target_os = "macos") {
            vec![PathBuf::from("/Applications"), home.join("Applications")]
        } else {
            Vec::new()
        };
        Self {
            codex_home: std::env::var_os("CODEX_HOME")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            install_dirs: desktop::install_dirs(&home, std::env::consts::OS),
            desktop_dirs: if cfg!(target_os = "linux") {
                desktop::xdg_dirs(
                    &home,
                    std::env::var_os("XDG_DATA_HOME").as_deref(),
                    std::env::var_os("XDG_DATA_DIRS").as_deref(),
                )
            } else {
                Vec::new()
            },
            home,
            path,
            app_dirs,
            versions: true,
        }
    }
}

/// How long one `--version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

/// Every known runtime, installed or not, with what was found of it.
/// `marker` is what identifies AgentDocker in a registration — the
/// binary's name — paired with the explicit MCP subcommand.
pub fn inventory(roots: &Roots, marker: &str) -> std::io::Result<Vec<RuntimeInfo>> {
    // A failed scan must not become an apparently empty installation inventory.
    std::thread::scope(|scope| {
        let handles = RUNTIMES
            .iter()
            .map(|spec| {
                std::thread::Builder::new()
                    .spawn_scoped(scope, move || inspect(spec, roots, marker))
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| std::io::Error::other("runtime inventory worker failed"))?
            })
            .collect()
    })
}

/// Inspect one selected runtime without consulting unrelated desktop entries.
pub fn inspect(spec: &RuntimeSpec, roots: &Roots, marker: &str) -> std::io::Result<RuntimeInfo> {
    let cli = spec
        .clis
        .iter()
        .find_map(|name| which(roots, name).or_else(|| which_in(&roots.install_dirs, name)));
    let version = cli
        .as_deref()
        .filter(|_| roots.versions)
        .and_then(|cli| version_of(cli, &roots.home));
    let apps = desktop::apps(spec, roots)?;
    let config_dir = if spec.name == "codex" {
        roots.codex_home.clone()
    } else {
        None
    }
    .or_else(|| spec.config_dir.map(|rel| roots.home.join(rel)))
    .filter(|dir| dir.is_dir());
    Ok(RuntimeInfo {
        name: spec.name.to_owned(),
        vendor: spec.vendor.to_owned(),
        label: spec.label.to_owned(),
        cli,
        version,
        apps,
        config_dir,
        mcp: mcp_wiring(spec, roots, marker),
        hooks: hooks_wiring_file(spec, &hook_config_path(spec, roots), marker),
        running: 0,
    })
}

fn which(roots: &Roots, name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        return which_in(&[path.parent()?.to_owned()], path.file_name()?.to_str()?);
    }
    if path.components().count() != 1 {
        return None;
    }
    which_in(&roots.path, name)
}

/// The first executable file of that name on the path. A data file that
/// happens to share the name is not a CLI.
#[cfg(unix)]
fn which_in(paths: &[PathBuf], name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    paths
        .iter()
        .filter(|path| path.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Resolve common Windows CLI executables and npm command shims from PATH.
/// Never implicitly search the working directory or execute a data-only file.
#[cfg(windows)]
fn which_in(paths: &[PathBuf], name: &str) -> Option<PathBuf> {
    const EXTENSIONS: &[&str] = &["exe", "com", "cmd", "bat"];
    let names: Vec<_> = if let Some(extension) = Path::new(name).extension() {
        if !EXTENSIONS
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        {
            return None;
        }
        vec![name.to_owned()]
    } else {
        EXTENSIONS
            .iter()
            .map(|extension| format!("{name}.{extension}"))
            .collect()
    };
    paths
        .iter()
        .filter(|path| path.is_absolute())
        .flat_map(|root| names.iter().map(move |name| root.join(name)))
        .find(|candidate| std::fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file()))
}

/// The first line of `<cli> --version`, trimmed to something table-sized.
fn version_of(cli: &Path, home: &Path) -> Option<String> {
    let argv = vec![cli.to_string_lossy().into_owned(), "--version".to_owned()];
    let output = command::run(home, &argv, VERSION_TIMEOUT).ok()?;
    if !output.success {
        return None;
    }
    let line = output.text.lines().find(|l| !l.trim().is_empty())?.trim();
    Some(line.chars().take(48).collect())
}

/// A macOS bundle's short version, read from its Info.plist.
fn app_version(bundle: &Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let plist = bundle.join("Contents/Info.plist");
    let argv = vec![
        "defaults".to_owned(),
        "read".to_owned(),
        plist.to_string_lossy().into_owned(),
        "CFBundleShortVersionString".to_owned(),
    ];
    let output = command::run(bundle, &argv, VERSION_TIMEOUT).ok()?;
    output
        .success
        .then(|| output.stdout.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Resolve a configuration file from injectable roots, including CODEX_HOME.
pub fn mcp_config_path(spec: &RuntimeSpec, roots: &Roots) -> Option<PathBuf> {
    match spec.mcp {
        McpWiring::None => None,
        McpWiring::JsonServers { file } | McpWiring::TomlServers { file } => {
            if spec.name == "codex" {
                if let Some(home) = &roots.codex_home {
                    return Some(home.join("config.toml"));
                }
            }
            Some(roots.home.join(file))
        }
    }
}

/// Recognize an explicit AgentDocker MCP launch, not mentions in unrelated args.
/// Recognize the bare launch and the runtime argument written by setup. Other
/// argument forms and wrappers remain unverified without executing the command.
pub fn mcp_command_matches(
    command: Option<&str>,
    args: &[&str],
    marker: &str,
    runtime: &str,
) -> bool {
    command
        .and_then(|c| Path::new(c).file_name())
        .and_then(|n| n.to_str())
        == Some(marker)
        && (args == ["mcp"] || args == ["mcp", "--runtime", runtime])
}

/// Whether the runtime's MCP configuration registers AgentDocker.
pub fn mcp_wiring(spec: &RuntimeSpec, roots: &Roots, marker: &str) -> Wiring {
    let Some(path) = mcp_config_path(spec, roots) else {
        return Wiring::Unsupported;
    };
    let is_toml = matches!(spec.mcp, McpWiring::TomlServers { .. });
    let value = match health::configuration("mcp", &path, is_toml) {
        Ok(value) => value,
        Err(check) => {
            return if check.status == health::Status::Missing {
                Wiring::Missing
            } else {
                Wiring::Unverified
            };
        }
    };
    let Some(config) = value.as_object() else {
        return Wiring::Unverified;
    };
    let Some(servers) = config.get(if is_toml { "mcp_servers" } else { "mcpServers" }) else {
        return Wiring::Missing;
    };
    let Some(servers) = servers.as_object() else {
        return Wiring::Unverified;
    };
    let recognized = |server: &serde_json::Value| {
        let args: Option<Vec<_>> = server["args"]
            .as_array()
            .and_then(|args| args.iter().map(serde_json::Value::as_str).collect());
        args.is_some_and(|args| {
            mcp_command_matches(server["command"].as_str(), &args, marker, spec.name)
        })
    };
    let enabled =
        |server: &serde_json::Value| server["enabled"] != false && server["disabled"] != true;
    // A valid alias cannot hide a conflicting reserved entry: setup must
    // preserve it and ask the user to inspect the configuration.
    if servers
        .get(marker)
        .is_some_and(|server| !recognized(server) || !enabled(server))
    {
        return Wiring::Unverified;
    }
    if servers
        .values()
        .any(|server| recognized(server) && enabled(server))
    {
        Wiring::Wired
    } else if servers.values().any(recognized) {
        Wiring::Unverified
    } else {
        Wiring::Missing
    }
}

/// Recognize the direct adapter invocation, including a quoted executable path.
/// Mentions and arbitrary shell wrappers remain unverified.
pub fn hook_command_matches(command: &str, marker: &str) -> bool {
    hook_command_matches_for(command, marker, "claude-code")
}

pub fn hook_command_matches_for(command: &str, marker: &str, runtime: &str) -> bool {
    let Some(words) = shlex::split(command) else {
        return false;
    };
    words.len() == 3
        && Path::new(&words[0]).file_name().and_then(|n| n.to_str()) == Some(marker)
        && words[1] == "hook"
        && words[2] == runtime
}

/// Render the executable as one shell argument, including spaces and quotes.
pub fn claude_hook_command(exe: &Path) -> std::io::Result<String> {
    hook_command(exe, "claude-code")
}

pub fn hook_command(exe: &Path, runtime: &str) -> std::io::Result<String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| std::io::Error::other("hook executable path is not UTF-8"))?;
    shlex::try_join([exe, "hook", runtime])
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// Whether the runtime's user-level hooks run AgentDocker's adapter.
pub fn hooks_wiring(spec: &RuntimeSpec, home: &Path, marker: &str) -> Wiring {
    let file = home.join(if spec.name == "codex" {
        ".codex/hooks.json"
    } else {
        ".claude/settings.json"
    });
    hooks_wiring_file(spec, &file, marker)
}

pub fn hook_config_path(spec: &RuntimeSpec, roots: &Roots) -> PathBuf {
    if spec.name == "codex" {
        roots
            .codex_home
            .clone()
            .unwrap_or_else(|| roots.home.join(".codex"))
            .join("hooks.json")
    } else {
        roots.home.join(".claude/settings.json")
    }
}

pub fn hook_events(runtime: &str) -> &'static [(&'static str, Option<&'static str>)] {
    if runtime == "codex" {
        agentdocker_core::runtime::CODEX_ACTIVITY_HOOKS
    } else {
        agentdocker_core::runtime::CLAUDE_CODE_HOOKS
    }
}

fn hooks_wiring_file(spec: &RuntimeSpec, file: &Path, marker: &str) -> Wiring {
    if !spec.hooks {
        return Wiring::Unsupported;
    }
    let Ok(raw) = health::read_configuration(file) else {
        return Wiring::Missing;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Wiring::Missing;
    };
    if hooks_configuration_matches_for(&value, marker, spec.name) {
        if spec.name == "codex" {
            Wiring::Unverified
        } else {
            Wiring::Wired
        }
    } else {
        Wiring::Missing
    }
}

/// Every required event must include our command with the full matcher.
fn hooks_configuration_matches_for(value: &serde_json::Value, marker: &str, runtime: &str) -> bool {
    if value["disableAllHooks"] == true {
        return false;
    }
    value
        .get("hooks")
        .and_then(|h| h.as_object())
        .is_some_and(|events| {
            hook_events(runtime).iter().all(|(event, matcher)| {
                events
                    .get(*event)
                    .and_then(|v| v.as_array())
                    .is_some_and(|entries| {
                        entries.iter().any(|entry| {
                            let actual = entry.get("matcher").and_then(|v| v.as_str());
                            let covers = actual == Some("*")
                                || match matcher {
                                    Some(expected) => actual == Some(*expected),
                                    None => actual.is_none() || actual == Some(""),
                                };
                            covers
                                && entry.get("hooks").and_then(|h| h.as_array()).is_some_and(
                                    |hooks| {
                                        hooks.iter().any(|hook| {
                                            hook.get("type").and_then(|v| v.as_str())
                                                == Some("command")
                                                && hook
                                                    .get("command")
                                                    .and_then(|v| v.as_str())
                                                    .is_some_and(|c| {
                                                        hook_command_matches_for(c, marker, runtime)
                                                    })
                                        })
                                    },
                                )
                        })
                    })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    pub(super) fn machine() -> (tempfile::TempDir, Roots) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let bin = tmp.path().join("bin");
        let apps = tmp.path().join("Applications");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        for cli in ["claude", "codex"] {
            #[cfg(unix)]
            {
                let path = bin.join(cli);
                std::fs::write(&path, "#!/bin/sh\necho 9.9.9\n").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            #[cfg(windows)]
            std::fs::write(
                bin.join(format!("{cli}.cmd")),
                "@echo off\r\necho 9.9.9\r\n",
            )
            .unwrap();
        }
        std::fs::create_dir_all(apps.join("Claude.app/Contents")).unwrap();
        let roots = Roots {
            codex_home: None,
            home,
            path: vec![bin],
            install_dirs: vec![],
            desktop_dirs: vec![],
            app_dirs: vec![apps],
            versions: false,
        };
        (tmp, roots)
    }

    #[test]
    fn inventory_reports_what_is_installed_and_what_is_wired() {
        let (_tmp, roots) = machine();
        let home = roots.home.clone();
        std::fs::write(
            home.join(".codex/config.toml"),
            "[projects.\"/x\"]\ntrust_level = \"trusted\"\n\n[mcp_servers.agentdocker]\ncommand = \"/opt/agentdocker\"\nargs = [\"mcp\"]\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/opt/agentdocker hook claude-code"}]}]}}"#,
        )
        .unwrap();

        // A file that is not executable is not a CLI, and a name in some
        // other field is a mention, not a registration.
        std::fs::write(roots.path[0].join("gemini"), "not a program").unwrap();
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        std::fs::write(
            home.join(".gemini/settings.json"),
            r#"{"mcpServers":{"x":{"command":"cat","env":{"NOTE":"agentdocker"}}}}"#,
        )
        .unwrap();

        let all = inventory(&roots, "agentdocker").unwrap();
        assert_eq!(all.len(), RUNTIMES.len());
        let by = |name: &str| all.iter().find(|r| r.name == name).unwrap().clone();
        let claude = by("claude-code");
        assert!(claude.installed());
        #[cfg(unix)]
        assert!(claude.cli.as_ref().unwrap().ends_with("bin/claude"));
        #[cfg(windows)]
        assert!(claude.cli.as_ref().unwrap().ends_with("bin/claude.cmd"));
        assert!(claude.apps.is_empty(), "the CLI is separate from Desktop");
        assert_eq!(by("claude-desktop").apps[0].label, "Claude Desktop");
        assert_eq!(by("claude-desktop").hooks, Wiring::Unsupported);
        assert!(claude.config_dir.is_some());
        assert_eq!(claude.mcp, Wiring::Missing, "another server is not ours");
        assert_eq!(
            claude.hooks,
            Wiring::Missing,
            "one event is not the complete adapter"
        );
        let codex = by("codex");
        assert!(codex.installed());
        assert_eq!(codex.mcp, Wiring::Wired);
        assert_eq!(codex.hooks, Wiring::Missing);
        let gemini = by("gemini-cli");
        assert!(!gemini.installed(), "a non-executable file is not a CLI");
        assert_eq!(
            gemini.mcp,
            Wiring::Missing,
            "a name outside command and args is not wiring"
        );
        assert_eq!(by("aider").mcp, Wiring::Unsupported);
    }

    #[test]
    fn cli_and_desktop_identities_do_not_share_integration_health() {
        let (_tmp, mut roots) = machine();
        roots.install_dirs = std::mem::take(&mut roots.path);
        for bundle in ["Codex.app", "ChatGPT.app"] {
            std::fs::create_dir_all(roots.app_dirs[0].join(bundle)).unwrap();
        }
        std::fs::write(
            roots.home.join(".codex/config.toml"),
            "[mcp_servers.agentdocker]\ncommand = \"agentdocker\"\nargs = [\"mcp\"]\n",
        )
        .unwrap();
        let all = inventory(&roots, "agentdocker").unwrap();
        let codex = all.iter().find(|r| r.name == "codex").unwrap();
        assert!(
            codex.cli.is_some(),
            "GUI inventory searches standard install locations"
        );
        assert!(codex.apps.is_empty());
        assert_eq!(codex.mcp, Wiring::Wired);
        for name in ["codex-desktop", "chatgpt"] {
            let app = all.iter().find(|r| r.name == name).unwrap();
            assert!(app.installed());
            assert_eq!(app.apps.len(), 1);
            assert!(app.cli.is_none());
            assert!(app.config_dir.is_none());
            assert_eq!(app.mcp, Wiring::Unsupported);
        }
        // Inventory's fallback must not certify a bare MCP executable against
        // a PATH that the GUI/daemon did not inherit.
        let helper = roots.install_dirs[0].join(if cfg!(windows) {
            "agentdocker.cmd"
        } else {
            "agentdocker"
        });
        std::fs::copy(codex.cli.as_ref().unwrap(), helper).unwrap();
        let check = health::inspect(
            agentdocker_core::runtime::spec("codex").unwrap(),
            &roots,
            "agentdocker",
        );
        assert_eq!(check[0].status, health::Status::ExecutableMissing);
    }

    #[test]
    fn codex_override_and_explicit_mcp_commands_determine_wiring() {
        let (_tmp, mut roots) = machine();
        let spec = agentdocker_core::runtime::spec("codex").unwrap();
        let alternate = roots.home.join("alternate-codex");
        std::fs::create_dir(&alternate).unwrap();
        roots.codex_home = Some(alternate.clone());
        let path = mcp_config_path(spec, &roots).unwrap();
        assert_eq!(path, alternate.join("config.toml"));
        for (command, args, enabled, expected) in [
            ("/opt/agentdocker", "[\"mcp\"]", true, Wiring::Wired),
            ("cat", "[\"agentdocker-notes\"]", true, Wiring::Unverified),
            ("/opt/agentdocker", "[\"mcp\"]", false, Wiring::Unverified),
            ("/opt/agentdocker", "[\"ps\"]", true, Wiring::Unverified),
            (
                "/opt/agentdocker",
                "[42, \"mcp\"]",
                true,
                Wiring::Unverified,
            ),
        ] {
            let config = format!(
                "[mcp_servers.agentdocker]\ncommand = {command:?}\nargs = {args}\nenabled = {enabled}\n"
            );
            std::fs::write(&path, config).unwrap();
            assert_eq!(mcp_wiring(spec, &roots, "agentdocker"), expected);
        }
    }

    #[test]
    fn inventory_preserves_reserved_errors_across_json_and_toml() {
        let (_tmp, roots) = machine();
        for name in ["gemini-cli", "codex"] {
            let spec = agentdocker_core::runtime::spec(name).unwrap();
            let path = mcp_config_path(spec, &roots).unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            assert_eq!(mcp_wiring(spec, &roots, "agentdocker").symbol(), "no");
            let good =
                serde_json::json!({"command":"agentdocker", "args":["mcp", "--runtime", name]});
            let cases = [
                (serde_json::json!({}), "no"),
                (
                    serde_json::json!({"other":{"command":"cat", "args":["notes"]}}),
                    "no",
                ),
                (serde_json::json!({"alias":good}), "yes"),
                (serde_json::json!({"agentdocker":good}), "yes"),
                (
                    serde_json::json!({"agentdocker":{"command":"agentdocker", "args":["mcp", "--runtime", "wrong-runtime"]}, "alias":good}),
                    "unverified",
                ),
                (
                    serde_json::json!({"agentdocker":{"command":"agentdocker", "args":["mcp"], "disabled":true}, "alias":good}),
                    "unverified",
                ),
                (
                    serde_json::json!({"agentdocker":{"command":"agentdocker", "args":["mcp"], "enabled":false}}),
                    "unverified",
                ),
                (
                    serde_json::json!({"agentdocker":{"command":"agentdocker", "args":[42,"mcp"]}}),
                    "unverified",
                ),
                (serde_json::json!({"agentdocker":"malformed"}), "unverified"),
            ];
            for (servers, expected) in cases {
                let value = serde_json::json!({if name == "codex" {"mcp_servers"} else {"mcpServers"}: servers});
                let raw = if name == "codex" {
                    toml::to_string(&value).unwrap()
                } else {
                    value.to_string()
                };
                std::fs::write(&path, &raw).unwrap();
                let result = mcp_wiring(spec, &roots, "agentdocker");
                assert_eq!(result.symbol(), expected, "{name}: {raw}");
                assert_eq!(
                    result.needs_review(),
                    matches!(expected, "no" | "unverified")
                );
                assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
            }
            std::fs::write(&path, "invalid configuration").unwrap();
            assert_eq!(
                mcp_wiring(spec, &roots, "agentdocker").symbol(),
                "unverified"
            );
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert_eq!(
                mcp_wiring(spec, &roots, "agentdocker").symbol(),
                "unverified"
            );
        }
        let state = Wiring::Unverified;
        assert_eq!(serde_json::to_string(&state).unwrap(), "\"unverified\"");
        assert_eq!(
            serde_json::from_str::<Wiring>("\"unverified\"").unwrap(),
            state
        );
    }

    #[test]
    fn versions_come_from_the_cli_when_asked() {
        let (_tmp, mut roots) = machine();
        roots.versions = true;
        let claude = inventory(&roots, "agentdocker")
            .unwrap()
            .into_iter()
            .find(|r| r.name == "claude-code")
            .unwrap();
        assert_eq!(claude.version.as_deref(), Some("9.9.9"));
    }
}
