//! The editors a folder can be opened in: which installed application or
//! command is which editor, and the command line that opens a folder in it.
//!
//! This module is the conventions themselves. Listing the application
//! folders and searching `PATH` is host I/O and lives in
//! `agentdocker-host::editor`. Nothing here guesses: an application or a
//! command outside the known editors is not offered. The bundle names
//! mirror the hosts in [`crate::terminal`], one entry per product rather
//! than one per family, since a person chooses RustRover, not "JetBrains".

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One editor known by name: what it is called, the application bundles
/// it ships as on macOS (without `.app`) and its command on Linux.
struct Known {
    kind: &'static str,
    name: &'static str,
    bundles: &'static [&'static str],
    command: Option<&'static str>,
}

/// The known editors, in the order they are listed.
const KNOWN: &[Known] = &[
    Known {
        kind: "vscode",
        name: "Visual Studio Code",
        bundles: &["Visual Studio Code"],
        command: Some("code"),
    },
    Known {
        kind: "vscode-insiders",
        name: "VS Code Insiders",
        bundles: &["Visual Studio Code - Insiders"],
        command: None,
    },
    Known {
        kind: "cursor",
        name: "Cursor",
        bundles: &["Cursor"],
        command: Some("cursor"),
    },
    Known {
        kind: "windsurf",
        name: "Windsurf",
        bundles: &["Windsurf"],
        command: Some("windsurf"),
    },
    Known {
        kind: "zed",
        name: "Zed",
        bundles: &["Zed"],
        command: Some("zed"),
    },
    Known {
        kind: "zed-preview",
        name: "Zed Preview",
        bundles: &["Zed Preview"],
        command: None,
    },
    Known {
        kind: "sublime",
        name: "Sublime Text",
        bundles: &["Sublime Text"],
        command: Some("subl"),
    },
    Known {
        kind: "nova",
        name: "Nova",
        bundles: &["Nova"],
        command: None,
    },
    Known {
        kind: "bbedit",
        name: "BBEdit",
        bundles: &["BBEdit"],
        command: None,
    },
    Known {
        kind: "xcode",
        name: "Xcode",
        bundles: &["Xcode"],
        command: None,
    },
    Known {
        kind: "fleet",
        name: "Fleet",
        bundles: &["Fleet"],
        command: None,
    },
    Known {
        kind: "intellij",
        name: "IntelliJ IDEA",
        bundles: &["IntelliJ IDEA", "IntelliJ IDEA Ultimate"],
        command: Some("idea"),
    },
    Known {
        kind: "intellij-ce",
        name: "IntelliJ IDEA CE",
        bundles: &["IntelliJ IDEA CE", "IntelliJ IDEA Community Edition"],
        command: None,
    },
    Known {
        kind: "rustrover",
        name: "RustRover",
        bundles: &["RustRover"],
        command: Some("rustrover"),
    },
    Known {
        kind: "pycharm",
        name: "PyCharm",
        bundles: &["PyCharm", "PyCharm Professional Edition"],
        command: Some("pycharm"),
    },
    Known {
        kind: "pycharm-ce",
        name: "PyCharm CE",
        bundles: &["PyCharm CE", "PyCharm Community Edition"],
        command: None,
    },
    Known {
        kind: "goland",
        name: "GoLand",
        bundles: &["GoLand"],
        command: Some("goland"),
    },
    Known {
        kind: "webstorm",
        name: "WebStorm",
        bundles: &["WebStorm"],
        command: Some("webstorm"),
    },
    Known {
        kind: "clion",
        name: "CLion",
        bundles: &["CLion"],
        command: Some("clion"),
    },
    Known {
        kind: "phpstorm",
        name: "PhpStorm",
        bundles: &["PhpStorm"],
        command: None,
    },
    Known {
        kind: "rubymine",
        name: "RubyMine",
        bundles: &["RubyMine"],
        command: None,
    },
    Known {
        kind: "datagrip",
        name: "DataGrip",
        bundles: &["DataGrip"],
        command: None,
    },
    Known {
        kind: "android-studio",
        name: "Android Studio",
        bundles: &["Android Studio"],
        command: None,
    },
];

/// How a folder is handed to an editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opener {
    /// A macOS application bundle, opened through Launch Services.
    Bundle(PathBuf),
    /// A command that takes the folder as its argument.
    Command(PathBuf),
}

/// An installed editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    /// Stable and unique among editors: `vscode`, `rustrover`, `zed`. What
    /// a remembered choice and a control's id are made of.
    pub kind: &'static str,
    /// The name a person knows it by.
    pub name: &'static str,
    pub opener: Opener,
}

impl Editor {
    /// The command line that opens `folder` in this editor, as separate
    /// arguments for the program in the first: never a shell string, so a
    /// folder's name is never read as anything but a name. `folder` is
    /// expected absolute, so it cannot be taken for an option.
    pub fn argv(&self, folder: &Path) -> Vec<OsString> {
        match &self.opener {
            Opener::Bundle(bundle) => vec![
                "/usr/bin/open".into(),
                "-a".into(),
                bundle.into(),
                folder.into(),
            ],
            Opener::Command(program) => vec![program.into(), folder.into()],
        }
    }
}

/// The editor an application bundle is, by its name without `.app`,
/// ignoring case.
fn known_bundle(name: &str) -> Option<&'static Known> {
    KNOWN.iter().find(|known| {
        known
            .bundles
            .iter()
            .any(|bundle| bundle.eq_ignore_ascii_case(name))
    })
}

/// The editors among these application bundles (`/Applications/Zed.app`),
/// in the known order, one of each: the first bundle given for an editor
/// wins. Anything not a known editor's bundle is left out.
pub fn from_bundles(bundles: impl IntoIterator<Item = PathBuf>) -> Vec<Editor> {
    let mut found: Vec<(usize, Editor)> = Vec::new();
    for bundle in bundles {
        let Some(name) = bundle
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".app"))
        else {
            continue;
        };
        let Some(known) = known_bundle(name) else {
            continue;
        };
        if found.iter().any(|(_, editor)| editor.kind == known.kind) {
            continue;
        }
        let rank = KNOWN
            .iter()
            .position(|k| k.kind == known.kind)
            .unwrap_or(usize::MAX);
        found.push((
            rank,
            Editor {
                kind: known.kind,
                name: known.name,
                opener: Opener::Bundle(bundle),
            },
        ));
    }
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, editor)| editor).collect()
}

/// The editors whose command `find` resolves (a `PATH` search, say), in
/// the known order.
pub fn from_commands(find: impl Fn(&str) -> Option<PathBuf>) -> Vec<Editor> {
    KNOWN
        .iter()
        .filter_map(|known| {
            let program = find(known.command?)?;
            Some(Editor {
                kind: known.kind,
                name: known.name,
                opener: Opener::Command(program),
            })
        })
        .collect()
}

/// The editors as a person is offered them: the one chosen last first,
/// the rest in their order. A last choice no longer installed changes
/// nothing.
pub fn ordered<'a>(editors: &'a [Editor], last: Option<&str>) -> Vec<&'a Editor> {
    let mut list: Vec<&Editor> = editors.iter().collect();
    if let Some(at) = last.and_then(|last| list.iter().position(|e| e.kind == last)) {
        let chosen = list.remove(at);
        list.insert(0, chosen);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(editors: &[Editor]) -> Vec<&'static str> {
        editors.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn bundles_are_known_by_name_one_of_each_in_the_known_order() {
        let editors = from_bundles(
            [
                "/Applications/Docker.app",
                "/Applications/RustRover.app",
                "/Applications/Visual Studio Code.app",
                "/Applications/visual studio code.app",
                "/Users/me/Applications/JetBrains Toolbox/PyCharm Professional Edition.app",
                "/Users/me/Applications/JetBrains Toolbox/RustRover.app",
                "/Applications/Zed Preview.app",
                "/Applications/Claude.app",
                "/Applications/Cursor",
                "/Applications/IntelliJ IDEA Community Edition.app",
            ]
            .map(PathBuf::from),
        );
        assert_eq!(
            kinds(&editors),
            [
                "vscode",
                "zed-preview",
                "intellij-ce",
                "rustrover",
                "pycharm"
            ]
        );
        // The first bundle given for an editor is the one opened.
        let rover = editors.iter().find(|e| e.kind == "rustrover").unwrap();
        assert_eq!(
            rover.opener,
            Opener::Bundle("/Applications/RustRover.app".into())
        );
        assert_eq!(rover.name, "RustRover");
        let pycharm = editors.iter().find(|e| e.kind == "pycharm").unwrap();
        assert_eq!(pycharm.name, "PyCharm");
    }

    #[test]
    fn every_editor_named_in_the_request_is_known() {
        for bundle in [
            "Visual Studio Code",
            "Visual Studio Code - Insiders",
            "Cursor",
            "Windsurf",
            "Zed",
            "Zed Preview",
            "Sublime Text",
            "Nova",
            "BBEdit",
            "Xcode",
            "Fleet",
            "IntelliJ IDEA",
            "IntelliJ IDEA CE",
            "RustRover",
            "PyCharm",
            "PyCharm CE",
            "GoLand",
            "WebStorm",
            "CLion",
            "PhpStorm",
            "RubyMine",
            "DataGrip",
            "Android Studio",
        ] {
            assert!(known_bundle(bundle).is_some(), "{bundle}");
        }
        let mut kinds: Vec<_> = KNOWN.iter().map(|k| k.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), KNOWN.len(), "kinds are unique");
    }

    #[test]
    fn commands_are_found_through_the_lookup_given() {
        let editors = from_commands(|name| {
            matches!(name, "subl" | "code" | "rustrover" | "idea")
                .then(|| PathBuf::from(format!("/usr/bin/{name}")))
        });
        assert_eq!(
            kinds(&editors),
            ["vscode", "sublime", "intellij", "rustrover"]
        );
        assert_eq!(editors[0].opener, Opener::Command("/usr/bin/code".into()));
        let asked = std::cell::RefCell::new(Vec::new());
        from_commands(|name| {
            asked.borrow_mut().push(name.to_owned());
            None
        });
        assert_eq!(
            asked.into_inner(),
            [
                "code",
                "cursor",
                "windsurf",
                "zed",
                "subl",
                "idea",
                "rustrover",
                "pycharm",
                "goland",
                "webstorm",
                "clion"
            ]
        );
    }

    #[test]
    fn a_folder_is_one_argument_never_shell_text() {
        let folder = Path::new("/Users/me/my project; rm -rf ~");
        let code = Editor {
            kind: "vscode",
            name: "Visual Studio Code",
            opener: Opener::Bundle("/Applications/Visual Studio Code.app".into()),
        };
        assert_eq!(
            code.argv(folder),
            [
                "/usr/bin/open",
                "-a",
                "/Applications/Visual Studio Code.app",
                "/Users/me/my project; rm -rf ~"
            ]
            .map(OsString::from)
        );
        let zed = Editor {
            kind: "zed",
            name: "Zed",
            opener: Opener::Command("/usr/bin/zed".into()),
        };
        assert_eq!(
            zed.argv(folder),
            ["/usr/bin/zed", "/Users/me/my project; rm -rf ~"].map(OsString::from)
        );
    }

    #[test]
    fn the_last_choice_comes_first_and_a_missing_one_changes_nothing() {
        let editors = from_bundles(
            [
                "/Applications/Zed.app",
                "/Applications/RustRover.app",
                "/Applications/Cursor.app",
            ]
            .map(PathBuf::from),
        );
        let order =
            |last| -> Vec<&str> { ordered(&editors, last).iter().map(|e| e.kind).collect() };
        assert_eq!(order(None), ["cursor", "zed", "rustrover"]);
        assert_eq!(order(Some("rustrover")), ["rustrover", "cursor", "zed"]);
        assert_eq!(order(Some("cursor")), ["cursor", "zed", "rustrover"]);
        assert_eq!(order(Some("xcode")), ["cursor", "zed", "rustrover"]);
        assert!(ordered(&[], Some("zed")).is_empty());
    }
}
