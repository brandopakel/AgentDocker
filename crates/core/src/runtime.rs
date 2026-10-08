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
    /// OpenCode's `mcp` object in a JSON file, relative to the home
    /// directory: each server is `{"type": "local", "command": [executable,
    /// args…], "enabled": true}`, the command one array with its arguments.
    OpencodeJson { file: &'static str },
    /// Declared in each agent's own configuration file rather than in one
    /// of the runtime's: Docker Agent reads its MCP servers from the
    /// `toolsets` of the YAML that describes the agent. There is no file
    /// for setup to register in; setup prints the toolset to add instead.
    AgentConfig,
    /// Not known to take one.
    None,
}

/// The toolset a Docker Agent YAML file lists to make every run of that
/// agent a participant: `agentdocker mcp` over stdio, as runtime
/// `docker-agent`. `{exe}` is the absolute path of this binary.
pub fn docker_agent_toolset(exe: &str) -> String {
    let quoted = serde_json::to_string(exe).expect("a string serializes");
    format!(
        "    toolsets:\n      - type: mcp\n        command: {quoted}\n        args: [\"mcp\", \"--runtime\", \"docker-agent\"]\n"
    )
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
        mcp: McpWiring::OpencodeJson {
            file: ".config/opencode/opencode.json",
        },
        hooks: false,
    },
    // Docker's agent runtime (formerly cagent): it runs the model loop
    // itself, from a YAML file per agent, and ships both standalone and as
    // the Docker CLI plugin behind `docker agent`. Its legacy name still
    // works as a command, and its configuration directory kept that name.
    RuntimeSpec {
        name: "docker-agent",
        vendor: "Docker",
        label: "Docker Agent",
        clis: &["docker-agent", "cagent"],
        apps: &[],
        linux_apps: &[],
        extensions: &[],
        config_dir: Some(".config/cagent"),
        mcp: McpWiring::AgentConfig,
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
    /// What the inventory could not read within its bounds — a special,
    /// oversized or unreadable file, a directory with too many entries —
    /// each named with the reason. An empty list means the inventory is
    /// whole; a capped scan is never reported as absence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<String>,
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

    /// How far AgentDocker reaches into a session of this runtime, by the
    /// adapters it ships for it. This is what the adapters make possible;
    /// whether one is installed here is the runtime's [`RuntimeInfo`]
    /// wiring, which [`Capability::installed`] reads.
    pub fn capabilities(&self) -> Capabilities {
        use Adapter as A;
        use Reach::*;
        let c = |reach, via, how| Capability { reach, via, how };
        const TOOLS: &str = "the MCP tools (claim, send_message, read_journal, checkpoint…), when the model calls them";
        match self.name {
            "claude-code" => Capabilities {
                joins: c(
                    Automatic,
                    A::Hooks,
                    "SessionStart registers the session; the MCP server does too",
                ),
                tools: c(Voluntary, A::Mcp, TOOLS),
                edits: c(
                    Automatic,
                    A::Hooks,
                    "PreToolUse refuses Edit, Write, MultiEdit and NotebookEdit on a file another agent holds, also under --dangerously-skip-permissions; shell writes are not covered",
                ),
                reads: c(
                    Automatic,
                    A::Hooks,
                    "PreToolUse records Read, Grep and Glob, so a later change reads as stale",
                ),
                messages: c(
                    Automatic,
                    A::Hooks,
                    "UserPromptSubmit and PostToolUse hand queued messages to the model",
                ),
                idle_wake: c(
                    Conditional,
                    A::Channel,
                    "a session launched with the channel flag (`run --claude-channel`, `setup --shell`, Agentfile `idle_messages`), after Claude's consent",
                ),
            },
            "codex" => Capabilities {
                joins: c(
                    Automatic,
                    A::Hooks,
                    "SessionStart registers the session once Codex's /hooks review accepts the hooks; the MCP server does too",
                ),
                tools: c(Voluntary, A::Mcp, TOOLS),
                edits: c(
                    Automatic,
                    A::Hooks,
                    "PreToolUse refuses each file an apply_patch names when another agent holds it; shell writes are not covered",
                ),
                reads: c(
                    Voluntary,
                    A::Mcp,
                    "observe_paths and check_stale, when the model calls them",
                ),
                messages: c(
                    Automatic,
                    A::Hooks,
                    "the prompt, tool and Stop hooks hand queued messages to the model",
                ),
                idle_wake: c(
                    Conditional,
                    A::InputAdapter,
                    "Codex's input adapter (experimental): a session started through it (`run --codex-input`, Agentfile `idle_messages`) or an existing terminal's native queue",
                ),
            },
            "opencode" => Capabilities {
                joins: c(
                    Automatic,
                    A::Plugin,
                    "the AgentDocker plugin registers the session; the MCP server does too",
                ),
                tools: c(Voluntary, A::Mcp, TOOLS),
                edits: c(
                    Automatic,
                    A::Plugin,
                    "the plugin refuses an edit, write or patch to a file another agent holds",
                ),
                reads: c(
                    Automatic,
                    A::Plugin,
                    "the plugin records each read, so a later change reads as stale",
                ),
                messages: c(
                    Automatic,
                    A::Plugin,
                    "the plugin hands messages and the journal to the model",
                ),
                idle_wake: c(
                    Automatic,
                    A::Plugin,
                    "the plugin starts a turn through OpenCode's own promptAsync; no consent flag",
                ),
            },
            _ if self.in_browser() => Capabilities {
                joins: c(
                    Conditional,
                    A::Connector,
                    "a browser agent joins through the remote connector, with consent for one project",
                ),
                tools: c(
                    Conditional,
                    A::Connector,
                    "the messaging tools only, through the remote connector",
                ),
                edits: c(
                    Unavailable,
                    A::None,
                    "nothing in the browser touches a checkout on this machine",
                ),
                reads: c(
                    Unavailable,
                    A::None,
                    "nothing in the browser reads a checkout on this machine",
                ),
                messages: c(
                    Conditional,
                    A::Connector,
                    "read_inbox, when the agent calls it through the connector",
                ),
                idle_wake: c(
                    Unavailable,
                    A::None,
                    "no input reaches a browser session from this machine",
                ),
            },
            _ if self.mcp != McpWiring::None => {
                let (via, joins, guard) = if self.mcp == McpWiring::AgentConfig {
                    (
                        A::AgentConfig,
                        "each run whose YAML lists the agentdocker toolset joins when the toolset starts; its sub-agents share that identity",
                        "only when the model claims before editing; a harness sub-agent running Claude Code or Codex is that runtime's own session, with its guard",
                    )
                } else {
                    (
                        A::Mcp,
                        "the MCP server registers the session when the host starts it",
                        "only when the model claims before editing",
                    )
                };
                Capabilities {
                    joins: c(Automatic, via, joins),
                    tools: c(Voluntary, via, TOOLS),
                    edits: c(Voluntary, via, guard),
                    reads: c(
                        Voluntary,
                        via,
                        "observe_paths and check_stale, when the model calls them",
                    ),
                    messages: c(
                        Voluntary,
                        via,
                        "read_inbox and wait_for_messages, when the model calls them",
                    ),
                    idle_wake: c(
                        Unavailable,
                        A::None,
                        "no input adapter: a message waits until the model reads its inbox",
                    ),
                }
            }
            _ if !self.clis.is_empty() => Capabilities {
                joins: c(
                    Conditional,
                    A::Adopt,
                    "discovery finds its processes; `agentdocker adopt` registers one",
                ),
                tools: c(
                    Conditional,
                    A::Mcp,
                    "if it takes MCP servers, point it at `agentdocker mcp` by hand",
                ),
                edits: c(
                    Conditional,
                    A::Mcp,
                    "only through those tools, when the model claims before editing",
                ),
                reads: c(
                    Conditional,
                    A::Mcp,
                    "only through those tools, when the model calls them",
                ),
                messages: c(
                    Conditional,
                    A::Mcp,
                    "only through those tools, when the model reads its inbox",
                ),
                idle_wake: c(Unavailable, A::None, "no input adapter"),
            },
            _ => Capabilities {
                joins: c(
                    Unavailable,
                    A::None,
                    "an application whose sessions AgentDocker cannot tell apart; inventory only",
                ),
                tools: c(Unavailable, A::None, "no MCP registration known for it"),
                edits: c(Unavailable, A::None, "no adapter"),
                reads: c(Unavailable, A::None, "no adapter"),
                messages: c(Unavailable, A::None, "no adapter"),
                idle_wake: c(Unavailable, A::None, "no input adapter"),
            },
        }
    }
}

/// How far AgentDocker reaches with one capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// The adapter does it whatever the model decides.
    Automatic,
    /// The model can, through the tools; nothing makes it.
    Voluntary,
    /// Only with something more from the person: a launch option and the
    /// provider's consent, an experimental adapter, an adoption, a connector.
    Conditional,
    /// Nothing on this machine can.
    Unavailable,
}

impl Reach {
    /// A word for a table cell.
    pub fn word(self) -> &'static str {
        match self {
            Self::Automatic => "auto",
            Self::Voluntary => "tools",
            Self::Conditional => "if set up",
            Self::Unavailable => "-",
        }
    }
}

/// What a capability runs through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    /// The runtime's hook configuration (`setup` installs it).
    Hooks,
    /// The runtime's MCP registration (`setup` writes it).
    Mcp,
    /// A plugin the runtime loads (OpenCode's, which `setup` installs).
    Plugin,
    /// Claude's channel, chosen per launch.
    Channel,
    /// Codex's input adapter, chosen per launch.
    InputAdapter,
    /// The agent's own configuration file (docker-agent's YAML toolsets).
    AgentConfig,
    /// The remote connector, for an agent in the browser.
    Connector,
    /// `agentdocker adopt`, by hand.
    Adopt,
    None,
}

impl Adapter {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hooks => "hooks",
            Self::Mcp => "MCP server",
            Self::Plugin => "plugin",
            Self::Channel => "Claude channel",
            Self::InputAdapter => "Codex input adapter",
            Self::AgentConfig => "agent YAML",
            Self::Connector => "remote connector",
            Self::Adopt => "adopt",
            Self::None => "nothing",
        }
    }
}

/// One capability: how far it reaches, through what, and in a sentence how.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Capability {
    pub reach: Reach,
    pub via: Adapter,
    pub how: &'static str,
}

impl Capability {
    /// Whether the adapter this capability runs through is installed, as
    /// far as the inventory can tell: `Some` for the hooks and the MCP
    /// registration, which it reads; `None` for what is chosen per launch,
    /// per file or per consent, or not looked at.
    pub fn installed(&self, info: &RuntimeInfo) -> Option<Wiring> {
        match self.via {
            Adapter::Hooks => Some(info.hooks),
            Adapter::Mcp => Some(info.mcp),
            _ => None,
        }
    }
}

/// What AgentDocker can do for a session of one runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    /// The session joins the registry by itself.
    pub joins: Capability,
    /// The coordination tools the model may call.
    pub tools: Capability,
    /// An edit to a file another agent holds is refused before it happens.
    pub edits: Capability,
    /// What the session reads is recorded, so a later change makes it stale.
    pub reads: Capability,
    /// Messages reach the model during a turn without it asking.
    pub messages: Capability,
    /// A message starts a turn while the session is idle.
    pub idle_wake: Capability,
}

impl Capabilities {
    /// Each capability with its column heading and a sentence of what it is,
    /// in display order.
    pub fn rows(&self) -> [(&'static str, &'static str, Capability); 6] {
        [
            ("JOINS", "Joins by itself", self.joins),
            ("TOOLS", "Coordination tools", self.tools),
            ("EDITS", "Refuses an edit to a held file", self.edits),
            ("READS", "Notices stale reads", self.reads),
            ("MESSAGES", "Hands messages to the model", self.messages),
            ("IDLE WAKE", "Wakes an idle session", self.idle_wake),
        ]
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
        let docker = spec("docker-agent").unwrap();
        assert_eq!(docker.mcp, McpWiring::AgentConfig);
        assert!(
            docker.clis.contains(&"cagent"),
            "the legacy name is found too"
        );
    }

    /// A profile claims only what an adapter AgentDocker ships can do: an
    /// automatic edit guard needs hooks or a plugin, nothing in the browser
    /// touches a checkout, and an application with no adapter reaches
    /// nothing.
    #[test]
    fn capability_profiles_follow_the_adapters() {
        for runtime in RUNTIMES {
            let profile = runtime.capabilities();
            for (_, _, capability) in profile.rows() {
                assert!(!capability.how.is_empty(), "{} says how", runtime.name);
                if capability.reach == Reach::Unavailable {
                    assert_eq!(capability.via, Adapter::None, "{}", runtime.name);
                }
            }
            if profile.edits.reach == Reach::Automatic {
                assert!(
                    runtime.hooks || runtime.name == "opencode",
                    "{} guards edits only with an adapter",
                    runtime.name
                );
            }
            if runtime.hooks {
                assert_eq!(profile.edits.reach, Reach::Automatic, "{}", runtime.name);
                assert_eq!(profile.edits.via, Adapter::Hooks);
            }
            if runtime.in_browser() {
                assert_eq!(profile.edits.reach, Reach::Unavailable);
                assert_eq!(profile.tools.via, Adapter::Connector);
            }
            if runtime.mcp == McpWiring::None && runtime.clis.is_empty() && !runtime.in_browser() {
                assert!(
                    profile
                        .rows()
                        .iter()
                        .all(|(_, _, c)| c.reach == Reach::Unavailable),
                    "{} has no adapter",
                    runtime.name
                );
            }
            if profile.idle_wake.reach == Reach::Automatic {
                assert_eq!(runtime.name, "opencode", "only the plugin wakes unasked");
            }
        }
        let docker = spec("docker-agent").unwrap().capabilities();
        assert_eq!(docker.joins.via, Adapter::AgentConfig);
        assert_eq!(docker.edits.reach, Reach::Voluntary);
        assert!(docker.edits.how.contains("harness"));
        let claude = spec("claude-code").unwrap().capabilities();
        assert!(claude.edits.how.contains("--dangerously-skip-permissions"));
        assert_eq!(claude.idle_wake.reach, Reach::Conditional);
        // What the inventory reads says whether the adapter is installed.
        let info = RuntimeInfo {
            name: "claude-code".into(),
            vendor: "Anthropic".into(),
            label: "Claude Code".into(),
            cli: None,
            version: None,
            apps: vec![],
            extensions: vec![],
            incomplete: vec![],
            config_dir: None,
            mcp: Wiring::Missing,
            hooks: Wiring::Wired,
            shell: Wiring::Unsupported,
            hooks_missing: vec![],
            running: 0,
        };
        assert_eq!(claude.edits.installed(&info), Some(Wiring::Wired));
        assert_eq!(claude.tools.installed(&info), Some(Wiring::Missing));
        assert_eq!(claude.idle_wake.installed(&info), None, "chosen per launch");
    }
}
