//! The application an agent's terminal is in: the window a person brings
//! to the front to reach it. A multiplexer (`multiplexer`) may sit between
//! the two; this is the app around it.
//!
//! This module is the conventions themselves: which executable is which
//! host. Walking a process's ancestry and reading its terminal device are
//! host I/O and live in `agentdocker-host::terminal`. Nothing here guesses:
//! an executable outside every known host and every application bundle is
//! not a host, and the walk moves on to its parent.
//!
//! AgentDocker's own programs are never a host ([`own`]): the CLI (an
//! `agentdocker codex` wrapper, say) runs in whatever terminal started it,
//! so the walk carries on past it; what runs under the daemon or the
//! desktop app is in AgentDocker's own terminal or in none, so the walk
//! ends there with no other application to go to.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where an agent's terminal is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// `terminal` (macOS Terminal), `iterm2`, `ghostty`, `wezterm`,
    /// `kitty`, `alacritty`, `warp`, `vscode`, `cursor`, `windsurf`,
    /// `jetbrains`, `zed`, a Linux terminal's own name, or `app` for
    /// another application that runs terminals.
    pub kind: String,
    /// The application's name as a person knows it: `Terminal`, `iTerm`,
    /// `Visual Studio Code`, `RustRover`.
    pub name: String,
    /// The application bundle to bring forward, on macOS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<PathBuf>,
    /// The agent's terminal device (`/dev/ttys005`), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
}

impl Host {
    /// One line for a person: `Terminal (ttys005)`, `RustRover`.
    pub fn describe(&self) -> String {
        match self
            .tty
            .as_deref()
            .and_then(|tty| tty.strip_prefix("/dev/"))
        {
            Some(device) if self.selects_tab() => format!("{} ({device})", self.name),
            _ => self.name.clone(),
        }
    }

    /// Whether the exact tab can be chosen, by its terminal device, rather
    /// than only the application brought forward.
    pub fn selects_tab(&self) -> bool {
        matches!(self.kind.as_str(), "terminal" | "iterm2") && self.tty.is_some()
    }

    /// Whether this names AgentDocker itself, as a record kept from before
    /// the walk passed over AgentDocker's own programs may: never another
    /// application to go to.
    pub fn is_agentdocker(&self) -> bool {
        self.app.as_deref().is_some_and(agentdocker_bundle)
            || (self.kind == "app" && self.name.eq_ignore_ascii_case(BUNDLE))
    }
}

/// The application bundle AgentDocker ships as, without `.app`.
pub const BUNDLE: &str = "AgentDocker";

/// One of AgentDocker's own programs, met on the walk up an agent's
/// parents. None of them is a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Own {
    /// The CLI (`agentdocker codex`, a hook, the MCP server) or another
    /// helper of AgentDocker's that is not the app: it runs in the terminal
    /// that started it, so the walk carries on past it.
    Cli,
    /// The daemon, as `agentd` or `agentdocker daemon run`, and the session
    /// owners it starts the same way: what runs under it is in
    /// AgentDocker's own terminal or in none, never in a terminal of the
    /// daemon's own ancestors.
    Daemon,
    /// The desktop app (`agentdocker-ui`): what it starts is AgentDocker's
    /// too.
    App,
}

/// The CLI's file name; the daemon (`agentd`) is a link to it.
const CLI: &str = "agentdocker";
/// The daemon's file name.
const DAEMON: &str = "agentd";
/// The desktop app's file name, the bundle's own executable.
const APP: &str = "agentdocker-ui";

/// Which of AgentDocker's own programs `executable` runs, invoked with
/// `argv`, if it is one: a file named `agentdocker`, `agentd` or
/// `agentdocker-ui` wherever it is installed, or anything inside an
/// `AgentDocker.app` bundle or a bundle `ours` says is AgentDocker's (one
/// renamed or copied, which only the filesystem can tell). The daemon run
/// under the CLI's name is told apart by its `daemon run` arguments.
pub fn own(executable: &Path, argv: &[String], ours: impl Fn(&Path) -> bool) -> Option<Own> {
    let in_bundle = bundle_of(executable)
        .is_some_and(|(bundle, _)| agentdocker_bundle(&bundle) || ours(&bundle));
    let daemon_run = matches!(
        argv.get(1..3),
        Some([daemon, run]) if daemon == "daemon" && run == "run"
    );
    if named(executable, DAEMON) {
        Some(Own::Daemon)
    } else if named(executable, APP) {
        Some(Own::App)
    } else if named(executable, CLI) || in_bundle {
        Some(if daemon_run { Own::Daemon } else { Own::Cli })
    } else {
        None
    }
}

/// Whether `bundle` is named `AgentDocker.app`, in any case.
pub fn agentdocker_bundle(bundle: &Path) -> bool {
    bundle
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(&format!("{BUNDLE}.app")))
}

/// The outermost application bundle in `executable`'s path, with its name
/// without `.app`: on macOS, the application the executable belongs to.
pub fn bundle_of(executable: &Path) -> Option<(PathBuf, &str)> {
    let mut bundle = PathBuf::new();
    for component in executable.components() {
        bundle.push(component);
        if let Some(name) = component
            .as_os_str()
            .to_str()
            .and_then(|part| part.strip_suffix(".app"))
        {
            return Some((bundle, name));
        }
    }
    None
}

/// Whether `path`'s file is `program` (with `.exe`, in any case, on
/// Windows).
fn named(path: &Path, program: &str) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if cfg!(windows) {
        let stem = name
            .len()
            .checked_sub(4)
            .filter(|&cut| name.is_char_boundary(cut))
            .filter(|&cut| name[cut..].eq_ignore_ascii_case(".exe"))
            .map_or(name, |cut| &name[..cut]);
        stem.eq_ignore_ascii_case(program)
    } else {
        name == program
    }
}

/// Application bundles by name (without `.app`), lowercased.
const BUNDLES: &[(&str, &str)] = &[
    ("terminal", "terminal"),
    ("iterm", "iterm2"),
    ("iterm2", "iterm2"),
    ("ghostty", "ghostty"),
    ("wezterm", "wezterm"),
    ("kitty", "kitty"),
    ("alacritty", "alacritty"),
    ("warp", "warp"),
    ("warppreview", "warp"),
    ("visual studio code", "vscode"),
    ("visual studio code - insiders", "vscode"),
    ("cursor", "cursor"),
    ("windsurf", "windsurf"),
    ("zed", "zed"),
    ("zed preview", "zed"),
    ("rustrover", "jetbrains"),
    ("pycharm", "jetbrains"),
    ("pycharm ce", "jetbrains"),
    ("goland", "jetbrains"),
    ("intellij idea", "jetbrains"),
    ("intellij idea ce", "jetbrains"),
    ("webstorm", "jetbrains"),
    ("clion", "jetbrains"),
    ("phpstorm", "jetbrains"),
    ("rubymine", "jetbrains"),
    ("datagrip", "jetbrains"),
    ("android studio", "jetbrains"),
];

/// Terminal emulators outside a bundle (Linux), by executable name.
const EXECUTABLES: &[(&str, &str, &str)] = &[
    ("gnome-terminal-server", "gnome-terminal", "GNOME Terminal"),
    ("konsole", "konsole", "Konsole"),
    ("xfce4-terminal", "xfce4-terminal", "Xfce Terminal"),
    ("tilix", "tilix", "Tilix"),
    ("foot", "foot", "foot"),
    ("xterm", "xterm", "xterm"),
    ("kitty", "kitty", "kitty"),
    ("alacritty", "alacritty", "Alacritty"),
    ("wezterm-gui", "wezterm", "WezTerm"),
    ("ghostty", "ghostty", "Ghostty"),
    ("code", "vscode", "Visual Studio Code"),
    ("cursor", "cursor", "Cursor"),
];

/// The host an executable belongs to. On macOS that is the outermost
/// application bundle in its path (a helper inside `Visual Studio
/// Code.app/Contents/Frameworks/…` is Visual Studio Code); elsewhere a
/// known terminal emulator's executable. `None` for a shell, `login`, a
/// multiplexer or anything else outside both, so the walk carries on, and
/// for AgentDocker's own programs by their names ([`own`]), which are never
/// a host.
pub fn classify(executable: &Path) -> Option<Host> {
    if own(executable, &[], |_| false).is_some() {
        return None;
    }
    if let Some((bundle, name)) = bundle_of(executable) {
        let kind = BUNDLES
            .iter()
            .find(|(known, _)| *known == name.to_ascii_lowercase())
            .map_or("app", |(_, kind)| *kind);
        return Some(Host {
            kind: kind.to_owned(),
            name: name.to_owned(),
            app: Some(bundle),
            tty: None,
        });
    }
    let file = executable.file_name()?.to_str()?;
    EXECUTABLES
        .iter()
        .find(|(known, _, _)| *known == file)
        .map(|(_, kind, name)| Host {
            kind: (*kind).to_owned(),
            name: (*name).to_owned(),
            app: None,
            tty: None,
        })
}

/// A terminal device as `ps` names it (`ttys005`, `pts/3`), as a path.
/// `??`, `-` and anything that is not a plain device name are no device.
pub fn device(name: &str) -> Option<String> {
    let name = name.trim();
    let plain = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
    let valid = match name.split_once('/') {
        Some(("pts", number)) => !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()),
        Some(_) => false,
        None => name.len() > 3 && name.starts_with("tty") && plain(name),
    };
    valid.then(|| format!("/dev/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(path: &str) -> Option<Host> {
        classify(Path::new(path))
    }

    #[test]
    fn a_bundle_is_its_outermost_application() {
        let terminal =
            host("/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal").unwrap();
        assert_eq!(terminal.kind, "terminal");
        assert_eq!(terminal.name, "Terminal");
        assert_eq!(
            terminal.app.as_deref(),
            Some(Path::new("/System/Applications/Utilities/Terminal.app"))
        );

        let code = host(
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)",
        )
        .unwrap();
        assert_eq!(code.kind, "vscode");
        assert_eq!(code.name, "Visual Studio Code");
        assert_eq!(
            code.app.as_deref(),
            Some(Path::new("/Applications/Visual Studio Code.app"))
        );

        assert_eq!(
            host("/Applications/iTerm.app/Contents/MacOS/iTerm2")
                .unwrap()
                .kind,
            "iterm2"
        );
        assert_eq!(
            host("/Applications/RustRover.app/Contents/MacOS/rustrover")
                .unwrap()
                .kind,
            "jetbrains"
        );
        assert_eq!(
            host("/Applications/Ghostty.app/Contents/MacOS/ghostty")
                .unwrap()
                .kind,
            "ghostty"
        );

        let other = host("/Applications/Claude.app/Contents/MacOS/Claude").unwrap();
        assert_eq!(
            (other.kind.as_str(), other.name.as_str()),
            ("app", "Claude")
        );
    }

    /// Where AgentDocker installs itself: the bundle in Applications, the
    /// managed installation's versioned bundle, and its programs outside a
    /// bundle.
    const VERSIONED: &str = "/Users/me/.local/share/agentdocker/desktop/versions/0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f/AgentDocker.app";

    fn argv(words: &str) -> Vec<String> {
        words.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn agentdockers_own_programs_are_never_a_host() {
        for path in [
            "/Applications/AgentDocker.app/Contents/MacOS/agentdocker".to_owned(),
            "/Applications/AgentDocker.app/Contents/MacOS/agentd".to_owned(),
            "/Applications/AgentDocker.app/Contents/MacOS/agentdocker-ui".to_owned(),
            "/Applications/AgentDocker.app/Contents/MacOS/some-helper".to_owned(),
            format!("{VERSIONED}/Contents/MacOS/agentdocker"),
            format!("{VERSIONED}/Contents/MacOS/agentd"),
            "/Applications/AgentDocker copy.app/Contents/MacOS/agentdocker".to_owned(),
            "/Users/me/.local/bin/agentdocker".to_owned(),
            "/usr/local/bin/agentd".to_owned(),
            "/home/me/src/target/debug/agentdocker-ui".to_owned(),
        ] {
            assert_eq!(host(&path), None, "{path}");
        }
    }

    #[test]
    fn own_programs_tell_the_cli_from_the_daemon_and_the_app() {
        let none = |_: &Path| false;
        let at = |file: &str| format!("{VERSIONED}/Contents/MacOS/{file}");
        let own_at = |path: &str, words: &str| own(Path::new(path), &argv(words), none);
        // The CLI, an `agentdocker codex` wrapper among its uses, is passed
        // through; the daemon and its session owners, under either name,
        // and the app are AgentDocker itself.
        assert_eq!(
            own_at(&at("agentdocker"), "agentdocker codex"),
            Some(Own::Cli)
        );
        assert_eq!(
            own_at(&at("agentd"), "agentd --session-owner"),
            Some(Own::Daemon)
        );
        assert_eq!(
            own_at(&at("agentdocker"), "agentdocker daemon run --session-owner"),
            Some(Own::Daemon)
        );
        assert_eq!(
            own_at(&at("agentdocker-ui"), "agentdocker-ui"),
            Some(Own::App)
        );
        assert_eq!(own_at(&at("some-helper"), "some-helper"), Some(Own::Cli));
        assert_eq!(
            own_at("/Users/me/.cargo/bin/agentd", "agentd"),
            Some(Own::Daemon)
        );
        assert_eq!(
            own_at("/Users/me/.local/bin/agentdocker", "agentdocker codex"),
            Some(Own::Cli)
        );
        // A bundle under another name is AgentDocker's only when the
        // filesystem says so.
        let renamed = Path::new("/Applications/Renamed.app/Contents/MacOS/helper");
        assert_eq!(own(renamed, &[], none), None);
        assert_eq!(
            own(renamed, &[], |bundle| bundle
                == Path::new("/Applications/Renamed.app")),
            Some(Own::Cli)
        );
        // Anything else is not AgentDocker's, whatever it is asked to run.
        for (path, words) in [
            ("/bin/zsh", "-zsh"),
            ("/usr/bin/login", "login -pf me"),
            ("/opt/homebrew/bin/codex", "codex daemon run"),
            (
                "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal",
                "Terminal",
            ),
        ] {
            assert_eq!(own_at(path, words), None, "{path}");
        }
    }

    #[test]
    fn a_host_naming_agentdocker_is_never_another_application() {
        let kept = Host {
            kind: "app".into(),
            name: "AgentDocker".into(),
            app: Some(PathBuf::from(VERSIONED)),
            tty: Some("/dev/ttys005".into()),
        };
        assert!(kept.is_agentdocker());
        assert!(
            Host {
                app: None,
                ..kept.clone()
            }
            .is_agentdocker()
        );
        assert!(
            Host {
                name: "Agentdocker".into(),
                app: Some(PathBuf::from("/Applications/agentdocker.APP")),
                ..kept
            }
            .is_agentdocker()
        );
        let terminal =
            host("/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal").unwrap();
        assert!(!terminal.is_agentdocker());
        assert!(!agentdocker_bundle(Path::new(
            "/Applications/AgentDocker Beta.app"
        )));
        assert_eq!(
            bundle_of(Path::new(&format!("{VERSIONED}/Contents/MacOS/agentd"))),
            Some((PathBuf::from(VERSIONED), "AgentDocker"))
        );
    }

    #[test]
    fn shells_multiplexers_and_unknown_programs_are_not_hosts() {
        for path in [
            "/bin/zsh",
            "-zsh",
            "/usr/bin/login",
            "/opt/homebrew/bin/tmux",
            "/opt/homebrew/bin/herdr",
            "/usr/local/bin/node",
        ] {
            assert_eq!(host(path), None, "{path}");
        }
    }

    #[test]
    fn linux_terminals_are_known_by_their_executable() {
        let gnome = host("/usr/libexec/gnome-terminal-server").unwrap();
        assert_eq!(
            (gnome.kind.as_str(), gnome.name.as_str()),
            ("gnome-terminal", "GNOME Terminal")
        );
        assert_eq!(gnome.app, None);
        assert_eq!(host("/usr/bin/wezterm-gui").unwrap().kind, "wezterm");
    }

    #[test]
    fn devices_are_plain_names_only() {
        assert_eq!(device("ttys005").as_deref(), Some("/dev/ttys005"));
        assert_eq!(device(" pts/3\n").as_deref(), Some("/dev/pts/3"));
        for name in [
            "??",
            "-",
            "",
            "tty",
            "ttys0/../x",
            "pts/",
            "pts/3a",
            "s005",
            "ttys 5",
        ] {
            assert_eq!(device(name), None, "{name:?}");
        }
    }

    #[test]
    fn a_tab_is_chosen_only_where_the_host_can_select_one() {
        let mut terminal =
            host("/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal").unwrap();
        assert_eq!(terminal.describe(), "Terminal");
        terminal.tty = Some("/dev/ttys005".into());
        assert!(terminal.selects_tab());
        assert_eq!(terminal.describe(), "Terminal (ttys005)");

        let mut rover = host("/Applications/RustRover.app/Contents/MacOS/rustrover").unwrap();
        rover.tty = Some("/dev/ttys009".into());
        assert!(!rover.selects_tab());
        assert_eq!(rover.describe(), "RustRover");
    }
}
