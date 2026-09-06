//! What agent tools are installed on this machine, and whether AgentDocker
//! is wired into each: the looking behind `agentdocker runtimes`. The
//! table of what to look for lives in core; this module only consults the
//! filesystem, `PATH`, and each tool's own configuration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::runtime::{
    InstalledApp, McpWiring, RUNTIMES, RuntimeInfo, RuntimeSpec, Wiring,
};

use crate::command;

/// Where to look: injectable so tests can build a machine in a temp dir.
#[derive(Clone, Debug)]
pub struct Roots {
    pub home: PathBuf,
    /// `PATH`, split.
    pub path: Vec<PathBuf>,
    /// Where desktop apps live: `/Applications` and `~/Applications` on
    /// macOS, nothing elsewhere.
    pub app_dirs: Vec<PathBuf>,
    /// Ask each CLI for its version; off in tests that only lay out files.
    pub versions: bool,
}

impl Roots {
    pub fn from_env() -> Self {
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let path = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        let app_dirs = if cfg!(target_os = "macos") {
            vec![PathBuf::from("/Applications"), home.join("Applications")]
        } else {
            Vec::new()
        };
        Self {
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
/// binary's name — so a wiring check is a substring match on commands.
pub fn inventory(roots: &Roots, marker: &str) -> Vec<RuntimeInfo> {
    // Versions spawn processes; do them side by side.
    std::thread::scope(|scope| {
        let handles: Vec<_> = RUNTIMES
            .iter()
            .map(|spec| scope.spawn(move || inspect(spec, roots, marker)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("inventory thread"))
            .collect()
    })
}

fn inspect(spec: &RuntimeSpec, roots: &Roots, marker: &str) -> RuntimeInfo {
    let cli = spec.clis.iter().find_map(|name| which(roots, name));
    let version = cli
        .as_deref()
        .filter(|_| roots.versions)
        .and_then(|cli| version_of(cli, &roots.home));
    let apps = spec
        .apps
        .iter()
        .filter_map(|(bundle, label)| {
            let path = roots
                .app_dirs
                .iter()
                .map(|dir| dir.join(bundle))
                .find(|p| p.is_dir())?;
            let version = if roots.versions {
                app_version(&path)
            } else {
                None
            };
            Some(InstalledApp {
                label: (*label).to_owned(),
                path,
                version,
            })
        })
        .collect();
    let config_dir = spec
        .config_dir
        .map(|rel| roots.home.join(rel))
        .filter(|dir| dir.is_dir());
    RuntimeInfo {
        name: spec.name.to_owned(),
        vendor: spec.vendor.to_owned(),
        label: spec.label.to_owned(),
        cli,
        version,
        apps,
        config_dir,
        mcp: mcp_wiring(spec, &roots.home, marker),
        hooks: hooks_wiring(spec, &roots.home, marker),
        running: 0,
    }
}

/// The first executable file of that name on the path. A data file that
/// happens to share the name is not a CLI.
fn which(roots: &Roots, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    roots
        .path
        .iter()
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
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

/// Whether the runtime's MCP configuration registers AgentDocker.
pub fn mcp_wiring(spec: &RuntimeSpec, home: &Path, marker: &str) -> Wiring {
    match spec.mcp {
        McpWiring::None => Wiring::Unsupported,
        McpWiring::JsonServers { file } => {
            let Ok(raw) = std::fs::read_to_string(home.join(file)) else {
                return Wiring::Missing;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
                return Wiring::Missing;
            };
            let wired = value
                .get("mcpServers")
                .and_then(|s| s.as_object())
                .is_some_and(|servers| {
                    servers.values().any(|server| {
                        let command = server
                            .get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains(marker));
                        let args =
                            server
                                .get("args")
                                .and_then(|a| a.as_array())
                                .is_some_and(|args| {
                                    args.iter()
                                        .filter_map(|a| a.as_str())
                                        .any(|a| a.contains(marker))
                                });
                        command || args
                    })
                });
            if wired {
                Wiring::Wired
            } else {
                Wiring::Missing
            }
        }
        McpWiring::TomlServers { file } => {
            let Ok(raw) = std::fs::read_to_string(home.join(file)) else {
                return Wiring::Missing;
            };
            let Ok(value) = raw.parse::<toml::Table>() else {
                return Wiring::Missing;
            };
            let wired = value
                .get("mcp_servers")
                .and_then(|s| s.as_table())
                .is_some_and(|servers| {
                    servers.values().any(|server| {
                        server.as_table().is_some_and(|server| {
                            let command = server
                                .get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains(marker));
                            let args =
                                server
                                    .get("args")
                                    .and_then(|a| a.as_array())
                                    .is_some_and(|args| {
                                        args.iter()
                                            .filter_map(|a| a.as_str())
                                            .any(|a| a.contains(marker))
                                    });
                            command || args
                        })
                    })
                });
            if wired {
                Wiring::Wired
            } else {
                Wiring::Missing
            }
        }
    }
}

/// Whether the runtime's user-level hooks run AgentDocker's adapter.
pub fn hooks_wiring(spec: &RuntimeSpec, home: &Path, marker: &str) -> Wiring {
    if !spec.hooks {
        return Wiring::Unsupported;
    }
    let file = home.join(".claude/settings.json");
    let Ok(raw) = std::fs::read_to_string(&file) else {
        return Wiring::Missing;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Wiring::Missing;
    };
    // Every entry is `{matcher?, hooks: [{type, command}]}` under an event
    // key; ask each command what it runs.
    let wired = value
        .get("hooks")
        .and_then(|h| h.as_object())
        .is_some_and(|events| {
            events.values().any(|entries| {
                entries.as_array().is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry
                            .get("hooks")
                            .and_then(|h| h.as_array())
                            .is_some_and(|hooks| {
                                hooks.iter().any(|hook| {
                                    hook.get("command")
                                        .and_then(|c| c.as_str())
                                        .is_some_and(|c| {
                                            c.contains(marker) && c.contains("hook claude-code")
                                        })
                                })
                            })
                    })
                })
            })
        });
    if wired {
        Wiring::Wired
    } else {
        Wiring::Missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn machine() -> (tempfile::TempDir, Roots) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let bin = tmp.path().join("bin");
        let apps = tmp.path().join("Applications");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        for cli in ["claude", "codex"] {
            let path = bin.join(cli);
            std::fs::write(&path, "#!/bin/sh\necho 9.9.9\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir_all(apps.join("Claude.app/Contents")).unwrap();
        let roots = Roots {
            home,
            path: vec![bin],
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

        let all = inventory(&roots, "agentdocker");
        assert_eq!(all.len(), RUNTIMES.len());
        let by = |name: &str| all.iter().find(|r| r.name == name).unwrap().clone();
        let claude = by("claude-code");
        assert!(claude.installed());
        assert!(claude.cli.as_ref().unwrap().ends_with("bin/claude"));
        assert_eq!(claude.apps.len(), 1);
        assert_eq!(claude.apps[0].label, "Claude Desktop");
        assert!(claude.config_dir.is_some());
        assert_eq!(claude.mcp, Wiring::Missing, "another server is not ours");
        assert_eq!(claude.hooks, Wiring::Wired);
        let codex = by("codex");
        assert!(codex.installed());
        assert_eq!(codex.mcp, Wiring::Wired);
        assert_eq!(codex.hooks, Wiring::Unsupported);
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
    fn versions_come_from_the_cli_when_asked() {
        let (_tmp, mut roots) = machine();
        roots.versions = true;
        let claude = inventory(&roots, "agentdocker")
            .into_iter()
            .find(|r| r.name == "claude-code")
            .unwrap();
        assert_eq!(claude.version.as_deref(), Some("9.9.9"));
    }
}
