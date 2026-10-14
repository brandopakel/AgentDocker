//! The application an agent's terminal is in: the window a person brings
//! to the front to reach it. A multiplexer (`multiplexer`) may sit between
//! the two; this is the app around it.
//!
//! This module is the conventions themselves: which executable is which
//! host. Walking a process's ancestry and reading its terminal device are
//! host I/O and live in `agentdocker-host::terminal`. Nothing here guesses:
//! an executable outside every known host and every application bundle is
//! not a host, and the walk moves on to its parent.

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
/// multiplexer or anything else outside both, so the walk carries on.
pub fn classify(executable: &Path) -> Option<Host> {
    let mut bundle = PathBuf::new();
    for component in executable.components() {
        bundle.push(component);
        let Some(name) = component
            .as_os_str()
            .to_str()
            .and_then(|part| part.strip_suffix(".app"))
        else {
            continue;
        };
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
