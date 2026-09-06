//! The agent runtimes AgentDocker knows how to find on a machine and wire
//! itself into: which command each one is, which desktop app, where it
//! keeps its configuration, and how it takes an MCP server or hooks.
//!
//! This is the table behind `agentdocker runtimes` and `agentdocker
//! setup`. It is data, not I/O: the host crate does the looking.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The complete Claude Code hook adapter, shared by installation and inventory.
pub const CLAUDE_CODE_EDIT_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Read|Grep|Glob";

/// Events required for complete observation and lifecycle coverage.
pub const CLAUDE_CODE_HOOKS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", Some(CLAUDE_CODE_EDIT_MATCHER)),
    ("PostToolUse", None),
    ("Stop", None),
    ("SessionEnd", None),
];

/// How a runtime registers MCP servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpWiring {
    /// An `mcpServers` object in a JSON file, relative to the home
    /// directory.
    JsonServers { file: &'static str },
    /// `[mcp_servers.<name>]` tables in a TOML file, relative to the home
    /// directory.
    TomlServers { file: &'static str },
    /// Not known to take one.
    None,
}

/// One runtime AgentDocker can recognise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeSpec {
    /// The runtime name agents register with: `claude-code`, `codex`, ...
    pub name: &'static str,
    pub vendor: &'static str,
    pub label: &'static str,
    /// Executable names to look for on `PATH`, in order of preference.
    pub clis: &'static [&'static str],
    /// Desktop apps of the same vendor, as (bundle file name, label).
    pub apps: &'static [(&'static str, &'static str)],
    /// The configuration directory, relative to the home directory.
    pub config_dir: Option<&'static str>,
    pub mcp: McpWiring,
    /// AgentDocker ships a hooks adapter for it.
    pub hooks: bool,
}

/// Every runtime the table knows, in display order.
pub const RUNTIMES: &[RuntimeSpec] = &[
    RuntimeSpec {
        name: "claude-code",
        vendor: "Anthropic",
        label: "Claude Code",
        clis: &["claude"],
        apps: &[],
        config_dir: Some(".claude"),
        mcp: McpWiring::JsonServers {
            file: ".claude.json",
        },
        hooks: true,
    },
    RuntimeSpec {
        name: "claude-desktop",
        vendor: "Anthropic",
        label: "Claude Desktop",
        clis: &[],
        apps: &[("Claude.app", "Claude Desktop")],
        config_dir: Some("Library/Application Support/Claude"),
        mcp: if cfg!(target_os = "macos") {
            McpWiring::JsonServers {
                file: "Library/Application Support/Claude/claude_desktop_config.json",
            }
        } else {
            McpWiring::None
        },
        hooks: false,
    },
    RuntimeSpec {
        name: "codex",
        vendor: "OpenAI",
        label: "Codex",
        clis: &["codex"],
        apps: &[("Codex.app", "Codex"), ("ChatGPT.app", "ChatGPT")],
        config_dir: Some(".codex"),
        mcp: McpWiring::TomlServers {
            file: ".codex/config.toml",
        },
        hooks: false,
    },
    RuntimeSpec {
        name: "gemini-cli",
        vendor: "Google",
        label: "Gemini CLI",
        clis: &["gemini"],
        apps: &[],
        config_dir: Some(".gemini"),
        mcp: McpWiring::JsonServers {
            file: ".gemini/settings.json",
        },
        hooks: false,
    },
    RuntimeSpec {
        name: "cursor",
        vendor: "Cursor",
        label: "Cursor",
        clis: &["cursor-agent"],
        apps: &[("Cursor.app", "Cursor")],
        config_dir: Some(".cursor"),
        mcp: McpWiring::JsonServers {
            file: ".cursor/mcp.json",
        },
        hooks: false,
    },
    RuntimeSpec {
        name: "windsurf",
        vendor: "Codeium",
        label: "Windsurf",
        clis: &[],
        apps: &[("Windsurf.app", "Windsurf")],
        config_dir: Some(".codeium/windsurf"),
        mcp: McpWiring::JsonServers {
            file: ".codeium/windsurf/mcp_config.json",
        },
        hooks: false,
    },
    RuntimeSpec {
        name: "copilot",
        vendor: "GitHub",
        label: "Copilot CLI",
        clis: &["copilot"],
        apps: &[],
        config_dir: Some(".copilot"),
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "vscode",
        vendor: "Microsoft",
        label: "VS Code (editor)",
        clis: &[],
        apps: &[("Visual Studio Code.app", "VS Code")],
        config_dir: Some(".vscode"),
        // An editor bundle does not prove an agent extension is installed.
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "aider",
        vendor: "Aider",
        label: "Aider",
        clis: &["aider"],
        apps: &[],
        config_dir: None,
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "goose",
        vendor: "Block",
        label: "Goose",
        clis: &["goose"],
        apps: &[],
        config_dir: Some(".config/goose"),
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "amp",
        vendor: "Sourcegraph",
        label: "Amp",
        clis: &["amp"],
        apps: &[],
        config_dir: Some(".config/amp"),
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "opencode",
        vendor: "OpenCode",
        label: "OpenCode",
        clis: &["opencode"],
        apps: &[],
        config_dir: Some(".config/opencode"),
        mcp: McpWiring::None,
        hooks: false,
    },
];

/// The table row for a runtime name.
pub fn spec(name: &str) -> Option<&'static RuntimeSpec> {
    RUNTIMES.iter().find(|r| r.name == name)
}

/// Whether AgentDocker is wired into one of a runtime's channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wiring {
    /// The runtime has no such channel, or AgentDocker has no adapter for
    /// it yet.
    Unsupported,
    Missing,
    Wired,
}

impl Wiring {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Unsupported => "-",
            Self::Missing => "no",
            Self::Wired => "yes",
        }
    }
}

/// A desktop app found on the machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledApp {
    pub label: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// What `agentdocker runtimes` reports for one runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub name: String,
    pub vendor: String,
    pub label: String,
    /// The CLI on `PATH`, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub apps: Vec<InstalledApp>,
    /// The configuration directory, when it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<PathBuf>,
    pub mcp: Wiring,
    pub hooks: Wiring,
    /// Processes of this runtime seen by the daemon's last scan that no
    /// registered agent claims.
    #[serde(default)]
    pub running: usize,
}

impl RuntimeInfo {
    /// Something of this runtime is on the machine.
    pub fn installed(&self) -> bool {
        self.cli.is_some() || !self.apps.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_consistent() {
        let mut names: Vec<&str> = RUNTIMES.iter().map(|r| r.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), RUNTIMES.len(), "runtime names are unique");
        for r in RUNTIMES {
            assert!(
                !r.clis.is_empty() || !r.apps.is_empty(),
                "{} is findable",
                r.name
            );
            assert!(!r.vendor.is_empty() && !r.label.is_empty());
            if r.hooks {
                assert_eq!(r.name, "claude-code", "hooks exist for Claude Code only");
            }
        }
        assert_eq!(spec("codex").map(|r| r.vendor), Some("OpenAI"));
        assert!(spec("nope").is_none());
    }
}
