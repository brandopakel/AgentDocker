//! The editors installed here, and opening a folder in one.
//!
//! The conventions — which application bundle or command is which editor,
//! and the command line that opens a folder in it — live in
//! `agentdocker_core::editor`; this is the I/O: listing the application
//! folders on macOS, searching `PATH` on Linux, and starting the editor.
//! Nothing is installed or configured, and nothing runs through a shell.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::editor::{Editor, Opener};

/// How long `open` may take to hand a folder to Launch Services.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Where macOS applications are installed, searched in this order: the
/// system's folder, the person's own, and the folder JetBrains Toolbox
/// installs its IDEs into.
pub fn application_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/Applications"),
        home.join("Applications"),
        home.join("Applications").join("JetBrains Toolbox"),
    ]
}

/// The application bundles directly inside each of `dirs`, in that order
/// and by name within each. A folder that cannot be read has none.
pub fn bundles_in(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut all = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut bundles: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "app") && path.is_dir())
            .collect();
        bundles.sort();
        all.extend(bundles);
    }
    all
}

/// The editors installed here: on macOS the known editors' bundles in the
/// application folders, on Linux the known editors' commands on `PATH`.
/// Windows has none yet.
pub fn installed() -> Vec<Editor> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        agentdocker_core::editor::from_bundles(bundles_in(&application_dirs(&home)))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let path: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        agentdocker_core::editor::from_commands(|name| crate::command::find_program(&path, name))
    }
    #[cfg(windows)]
    {
        Vec::new()
    }
}

/// Open `folder` in `editor`. The folder must exist and be absolute; it is
/// passed as one argument, never as shell text.
pub fn open(editor: &Editor, folder: &Path) -> Result<(), String> {
    if !folder.is_absolute() || !folder.is_dir() {
        return Err(format!(
            "The folder {} is unavailable, so it was not opened in {}.",
            folder.display(),
            editor.name
        ));
    }
    let folder = folder
        .canonicalize()
        .map_err(|e| format!("Could not open the folder: {e}"))?;
    let argv = editor.argv(&folder);
    match &editor.opener {
        Opener::Bundle(_) => {
            // `open` hands the folder to Launch Services and returns at
            // once; when it fails, what it printed says why.
            let argv = argv
                .into_iter()
                .map(|arg| arg.into_string())
                .collect::<Result<Vec<String>, _>>()
                .map_err(|_| format!("The folder's name cannot be passed to {}.", editor.name))?;
            let output = crate::command::run(&folder, &argv, OPEN_TIMEOUT)
                .map_err(|e| format!("Could not open {}: {e}", editor.name))?;
            if !output.success {
                return Err(format!(
                    "Could not open {}: {}",
                    editor.name,
                    output.text.trim()
                ));
            }
            Ok(())
        }
        Opener::Command(_) => {
            // An editor's command may run for as long as the editor does
            // (a JetBrains launcher script does), so it is started, not
            // waited for, and reaped whenever it exits.
            let (program, args) = argv.split_first().expect("a program");
            let mut child = std::process::Command::new(program)
                .args(args)
                .current_dir(&folder)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("Could not start {}: {e}", editor.name))?;
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::editor::from_bundles;

    #[test]
    fn only_application_bundles_are_listed_in_folder_order() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for dir in ["Zed.app", "Cursor.app", "Notes"] {
            std::fs::create_dir(first.path().join(dir)).unwrap();
        }
        std::fs::write(first.path().join("Fake.app"), b"not a bundle").unwrap();
        std::fs::create_dir(second.path().join("RustRover.app")).unwrap();
        let bundles = bundles_in(&[
            first.path().to_owned(),
            first.path().join("missing"),
            second.path().to_owned(),
        ]);
        assert_eq!(
            bundles,
            [
                first.path().join("Cursor.app"),
                first.path().join("Zed.app"),
                second.path().join("RustRover.app"),
            ]
        );
        let kinds: Vec<_> = from_bundles(bundles).iter().map(|e| e.kind).collect();
        assert_eq!(kinds, ["cursor", "zed", "rustrover"]);
    }

    #[test]
    fn the_toolbox_folder_is_searched_after_the_application_folders() {
        assert_eq!(
            application_dirs(Path::new("/Users/me")),
            [
                PathBuf::from("/Applications"),
                PathBuf::from("/Users/me/Applications"),
                PathBuf::from("/Users/me/Applications/JetBrains Toolbox"),
            ]
        );
    }

    #[test]
    fn a_missing_or_relative_folder_is_never_opened() {
        // A bundle that does not exist: were the folder accepted, `open`
        // would fail rather than start anything.
        let editor = Editor {
            kind: "zed",
            name: "Zed",
            opener: Opener::Bundle("/nonexistent/Zed.app".into()),
        };
        let missing = open(&editor, Path::new("/nonexistent/agentdocker/folder")).unwrap_err();
        assert!(missing.contains("unavailable"), "{missing}");
        assert!(missing.contains("Zed"), "{missing}");
        assert!(open(&editor, Path::new("relative")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_command_is_started_with_the_folder_and_a_missing_one_says_so() {
        let folder = tempfile::tempdir().unwrap();
        let started = Editor {
            kind: "zed",
            name: "Zed",
            opener: Opener::Command("/usr/bin/true".into()),
        };
        assert_eq!(open(&started, folder.path()), Ok(()));
        let missing = Editor {
            opener: Opener::Command("/nonexistent/zed".into()),
            ..started
        };
        let error = open(&missing, folder.path()).unwrap_err();
        assert!(error.starts_with("Could not start Zed"), "{error}");
    }
}
