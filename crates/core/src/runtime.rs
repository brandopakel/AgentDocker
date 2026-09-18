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
    ("StopFailure", None),
    ("SessionEnd", None),
];

/// Activity observations plus prompt/tool/Stop inbox delivery. MCP also
/// provides explicit reads and the remaining coordination tools.
pub const CODEX_ACTIVITY_HOOKS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", None),
    ("PostToolUse", None),
    ("PreCompact", None),
    ("PostCompact", None),
    ("Stop", None),
    ("Interrupt", None),
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

/// A vendor's browser extension: an agent that works inside the browser.
/// Its sessions run there and on the vendor's side, so nothing on this
/// machine speaks for them. AgentDocker can find the extension and say
/// so; it cannot list, message or lease for what the extension is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowserExtension {
    /// The Web Store item id: the directory the extension is unpacked
    /// into under a Chromium-family profile's `Extensions`.
    pub id: &'static str,
    pub label: &'static str,
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
    /// Known Linux desktop-entry IDs and their labels. Presence is inventory only.
    pub linux_apps: &'static [(&'static str, &'static str)],
    /// Browser extensions of the same vendor. Presence is inventory only:
    /// their sessions are not observable from this machine.
    pub extensions: &'static [BrowserExtension],
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
        linux_apps: &[],
        extensions: &[],
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
        linux_apps: &[],
        extensions: &[],
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
        name: "claude-browser",
        vendor: "Anthropic",
        label: "Claude (browser extension)",
        clis: &[],
        apps: &[],
        linux_apps: &[],
        extensions: &[BrowserExtension {
            id: "fcoeoabgfenejglbffodgkkbkcdhcgfn",
            label: "Claude",
        }],
        config_dir: None,
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "codex",
        vendor: "OpenAI",
        label: "Codex",
        clis: &["codex"],
        apps: &[],
        linux_apps: &[],
        extensions: &[],
        config_dir: Some(".codex"),
        mcp: McpWiring::TomlServers {
            file: ".codex/config.toml",
        },
        hooks: true,
    },
    RuntimeSpec {
        name: "codex-desktop",
        vendor: "OpenAI",
        label: "Codex desktop",
        clis: &[],
        apps: &[("Codex.app", "Codex")],
        linux_apps: &[],
        extensions: &[],
        config_dir: None,
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "chatgpt",
        vendor: "OpenAI",
        label: "ChatGPT",
        clis: &[],
        apps: &[("ChatGPT.app", "ChatGPT")],
        linux_apps: &[],
        extensions: &[],
        config_dir: None,
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "chatgpt-browser",
        vendor: "OpenAI",
        label: "ChatGPT (browser extension)",
        clis: &[],
        apps: &[],
        linux_apps: &[],
        extensions: &[BrowserExtension {
            id: "hehggadaopoacecdllhhajmbjkdcmajg",
            label: "ChatGPT",
        }],
        config_dir: None,
        mcp: McpWiring::None,
        hooks: false,
    },
    RuntimeSpec {
        name: "gemini-cli",
        vendor: "Google",
        label: "Gemini CLI",
        clis: &["gemini"],
        apps: &[],
        linux_apps: &[],
        extensions: &[],
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
        linux_apps: &[("cursor.desktop", "Cursor")],
        extensions: &[],
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
        linux_apps: &[("windsurf.desktop", "Windsurf")],
        extensions: &[],
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
        linux_apps: &[],
        extensions: &[],
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
        linux_apps: &[
            ("code.desktop", "VS Code"),
            ("code-insiders.desktop", "VS Code Insiders"),
            ("com.visualstudio.code.desktop", "VS Code"),
        ],
        extensions: &[],
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
        linux_apps: &[],
        extensions: &[],
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
        linux_apps: &[],
        extensions: &[],
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
        linux_apps: &[],
        extensions: &[],
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
        linux_apps: &[],
        extensions: &[],
        config_dir: Some(".config/opencode"),
        mcp: McpWiring::None,
        hooks: false,
    },
];

/// What every listing says under a runtime that works inside the browser,
/// so that nobody waits for a session that cannot appear.
pub const IN_BROWSER: &str = "Sessions in the browser run there and on the vendor's side; nothing on this machine speaks for them, so AgentDocker cannot list, message or lease for them — unless one joins through the remote connector (`agentdocker connector`), which gives it the messaging tools and nothing that touches a checkout. A bridge the browser launches for a command-line tool is that tool's helper, not a session, and adopting it is refused.";

/// The table row for a runtime name.
pub fn spec(name: &str) -> Option<&'static RuntimeSpec> {
    RUNTIMES.iter().find(|r| r.name == name)
}

/// Whether AgentDocker is wired into one of a runtime's channels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wiring {
    /// The runtime has no such channel, or AgentDocker has no adapter for
    /// it yet.
    #[default]
    Unsupported,
    Missing,
    /// Configuration exists but is invalid, disabled or conflicts with the adapter.
    Unverified,
    Wired,
}

impl Wiring {
    /// A supported channel that needs inspection before use.
    pub fn needs_review(self) -> bool {
        matches!(self, Self::Missing | Self::Unverified)
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Unsupported => "-",
            Self::Missing => "no",
            Self::Unverified => "unverified",
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

/// A vendor's browser extension found in a browser profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledExtension {
    pub label: String,
    /// The browser it is installed in: `Chrome`, `Brave`, ...
    pub browser: String,
    /// The profile within that browser, when it is not the only one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The native messaging host the browser launches for it: a vendor's
    /// bridge from the extension to a program on this machine. The bridge
    /// is that program's helper, not a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<PathBuf>,
}

/// What `agentdocker runtimes` reports for one runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub name: String,
    pub vendor: String,
    pub label: String,
    /// The CLI on PATH or in a standard installation directory, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub apps: Vec<InstalledApp>,
    /// The vendor's browser extension, once per browser profile it is
    /// installed in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<InstalledExtension>,
    /// The configuration directory, when it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<PathBuf>,
    pub mcp: Wiring,
    pub hooks: Wiring,
    /// Whether the person's shell starts this runtime with what lets
    /// AgentDocker wake an idle session — for Claude Code, the channel
    /// flag on every terminal `claude` (`setup --shell`). Unsupported for
    /// every other runtime, and for a shell we do not know.
    #[serde(default)]
    pub shell: Wiring,
    /// The hook events our command is not wired for, when `hooks` is
    /// `Missing`: what setup would add. A release that requires a new
    /// event turns a wired machine into a missing one, and this is what
    /// says so rather than "needs setup" alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks_missing: Vec<String>,
    /// Processes of this runtime seen by the daemon's last scan that no
    /// registered agent claims.
    #[serde(default)]
    pub running: usize,
}

impl RuntimeInfo {
    /// Something of this runtime is on the machine.
    pub fn installed(&self) -> bool {
        self.cli.is_some() || !self.apps.is_empty() || !self.extensions.is_empty()
    }

    /// The runtime works inside a browser: it has no command and no
    /// application of its own, only an extension, so its sessions are not
    /// observable from this machine.
    pub fn in_browser(&self) -> bool {
        spec(&self.name).is_some_and(RuntimeSpec::in_browser)
    }
}

impl RuntimeSpec {
    /// See [`RuntimeInfo::in_browser`].
    pub fn in_browser(&self) -> bool {
        !self.extensions.is_empty() && self.clis.is_empty() && self.apps.is_empty()
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
                !r.clis.is_empty() || !r.apps.is_empty() || !r.extensions.is_empty(),
                "{} is findable",
                r.name
            );
            for extension in r.extensions {
                assert!(
                    extension.id.len() == 32
                        && extension.id.bytes().all(|b| b.is_ascii_lowercase()),
                    "{} has a Web Store id",
                    r.name
                );
            }
            if r.in_browser() {
                assert!(
                    !r.hooks && r.mcp == McpWiring::None,
                    "{} runs in the browser and takes no adapter",
                    r.name
                );
            }
            assert!(!r.vendor.is_empty() && !r.label.is_empty());
            if r.hooks {
                assert!(matches!(r.name, "claude-code" | "codex"));
            }
        }
        assert_eq!(spec("codex").map(|r| r.vendor), Some("OpenAI"));
        assert!(spec("nope").is_none());
        let ids: Vec<&str> = RUNTIMES
            .iter()
            .flat_map(|r| r.extensions.iter().map(|e| e.id))
            .collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "extension ids are unique");
        assert!(spec("claude-browser").unwrap().in_browser());
        assert!(spec("chatgpt-browser").unwrap().in_browser());
        assert!(!spec("claude-code").unwrap().in_browser());
    }
}
