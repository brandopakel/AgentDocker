//! Read-only desktop registration inventory and GUI-safe installation locations.
use super::{Roots, app_version, which};
use agentdocker_core::runtime::{InstalledApp, RuntimeSpec};
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub(super) fn install_dirs(home: &Path, os: &str) -> Vec<PathBuf> {
    let mut paths = vec![home.join(".local/bin"), home.join(".cargo/bin")];
    if os == "macos" {
        paths.push("/opt/homebrew/bin".into());
    }
    if os != "windows" {
        paths.extend(["/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    }
    paths.into_iter().filter(|p| p.is_absolute()).collect()
}

pub(super) fn xdg_dirs(
    home: &Path,
    data_home: Option<&OsStr>,
    data_dirs: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut bases = vec![
        data_home
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/share")),
    ];
    bases.extend(
        data_dirs
            .filter(|s| !s.is_empty())
            .map(|s| {
                std::env::split_paths(s)
                    .filter(|p| p.is_absolute())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]),
    );
    let mut seen = HashSet::new();
    bases
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .map(|base| base.join("applications"))
        .collect()
}

fn error(path: &Path, detail: &str) -> io::Error {
    io::Error::other(format!("desktop inventory at {}: {detail}", path.display()))
}

pub(super) fn apps(spec: &RuntimeSpec, roots: &Roots) -> io::Result<Vec<InstalledApp>> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for (bundle, label) in spec.apps {
        for directory in &roots.app_dirs {
            let path = directory.join(bundle);
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(error(&path, "bundle path is not a directory")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(error(&path, "bundle cannot be inspected")),
            }
            let identity = path
                .canonicalize()
                .map_err(|_| error(&path, "bundle cannot be resolved"))?;
            if seen.insert(identity) {
                found.push(InstalledApp {
                    label: (*label).into(),
                    version: if roots.versions {
                        app_version(&path)
                    } else {
                        None
                    },
                    path,
                });
            }
        }
    }
    for (id, label) in spec.linux_apps {
        for directory in &roots.desktop_dirs {
            let path = directory.join(id);
            // The first existing ID wins, including Hidden=true tombstones.
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(error(&path, "launcher cannot be inspected")),
            }
            if desktop_entry(&path, roots)? {
                let identity = path
                    .canonicalize()
                    .map_err(|_| error(&path, "launcher cannot be resolved"))?;
                if seen.insert(identity) {
                    found.push(InstalledApp {
                        label: (*label).into(),
                        path,
                        version: None,
                    });
                }
            }
            break;
        }
    }
    Ok(found)
}

/// Desktop exports may be intentional symlinks. Bound reads and never execute
/// Exec, TryExec, actions, or a command claimed by an entry.
fn desktop_entry(path: &Path, roots: &Roots) -> io::Result<bool> {
    const LIMIT: u64 = 64 * 1024;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| error(path, "launcher cannot be read"))?;
    let metadata = file
        .metadata()
        .map_err(|_| error(path, "launcher metadata is unavailable"))?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(error(
            path,
            "launcher must be a regular file of at most 64 KiB",
        ));
    }
    let mut raw = String::new();
    file.take(LIMIT + 1)
        .read_to_string(&mut raw)
        .map_err(|_| error(path, "launcher must contain valid UTF-8"))?;
    if raw.len() as u64 > LIMIT {
        return Err(error(path, "launcher grew beyond 64 KiB"));
    }
    let fields = fields(&raw).map_err(|_| error(path, "invalid desktop entry"))?;
    for key in ["Hidden", "NoDisplay", "DBusActivatable"] {
        if fields
            .get(key)
            .is_some_and(|value| !matches!(*value, "true" | "false"))
        {
            return Err(error(path, "invalid desktop-entry boolean"));
        }
    }
    if fields.get("Hidden") == Some(&"true") {
        return Ok(false);
    }
    match fields.get("Type") {
        Some(&"Application") => {}
        Some(_) => return Ok(false),
        None => return Err(error(path, "launcher has no entry type")),
    }
    if fields.get("Name").is_none_or(|value| value.is_empty()) {
        return Err(error(path, "launcher has no application name"));
    }
    if fields.get("Exec").is_none_or(|value| value.is_empty())
        && fields.get("DBusActivatable") != Some(&"true")
    {
        return Err(error(
            path,
            "launcher has no execution or activation declaration",
        ));
    }
    if let Some(value) = fields.get("TryExec") {
        let executable = unescape(value).ok_or_else(|| error(path, "invalid TryExec string"))?;
        // TryExec is a path or program name, not a command line. which also
        // checks absolute paths using the inspecting process's real PATH.
        if which(roots, &executable).is_none() {
            return Ok(false);
        }
    }
    // NoDisplay hides a menu item, not an installed registration. Version is
    // the desktop-entry specification version, never an application version.
    Ok(true)
}

fn fields(raw: &str) -> Result<BTreeMap<&str, &str>, ()> {
    let mut fields = BTreeMap::new();
    let mut entry = false;
    let mut seen = false;
    for line in raw.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            entry = line == "[Desktop Entry]";
            if entry && seen {
                return Err(());
            }
            seen |= entry;
            continue;
        }
        if entry {
            let (key, value) = line.split_once('=').ok_or(())?;
            if fields.insert(key.trim(), value.trim()).is_some() {
                return Err(());
            }
        }
    }
    if !seen {
        return Err(());
    }
    Ok(fields)
}

fn unescape(raw: &str) -> Option<String> {
    let mut text = String::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        text.push(if ch == '\\' {
            match chars.next()? {
                's' => ' ',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\\' => '\\',
                _ => return None,
            }
        } else {
            ch
        });
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::runtime::spec;

    fn fixture() -> (tempfile::TempDir, Roots) {
        let (temp, mut roots) = super::super::tests::machine();
        roots.app_dirs.clear();
        roots.desktop_dirs = ["user-applications", "system-applications"]
            .map(|name| temp.path().join(name))
            .to_vec();
        for path in &roots.desktop_dirs {
            std::fs::create_dir(path).unwrap();
        }
        (temp, roots)
    }

    fn entry() -> &'static str {
        "[Desktop Entry]\nType=Application\nName=Fixture editor\nExec=never-execute-this-command\n"
    }

    #[test]
    #[cfg(unix)]
    fn xdg_roots_are_absolute_ordered_and_deduplicated() {
        assert_eq!(
            xdg_dirs(Path::new("/home/fixture"), None, None),
            [
                "/home/fixture/.local/share/applications",
                "/usr/local/share/applications",
                "/usr/share/applications"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            xdg_dirs(
                Path::new("/home/fixture"),
                Some(OsStr::new("relative")),
                Some(OsStr::new("relative:/system:/system::/other"))
            ),
            [
                "/home/fixture/.local/share/applications",
                "/system/applications",
                "/other/applications"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            xdg_dirs(
                Path::new("/home/fixture"),
                Some(OsStr::new("/custom")),
                Some(OsStr::new("/custom:/system"))
            ),
            ["/custom/applications", "/system/applications"].map(PathBuf::from)
        );
        assert!(
            install_dirs(Path::new("/home/fixture"), "macos")
                .contains(&PathBuf::from("/opt/homebrew/bin"))
        );
        assert!(
            !install_dirs(Path::new("/home/fixture"), "linux")
                .contains(&PathBuf::from("/opt/homebrew/bin"))
        );
    }

    #[test]
    fn user_hidden_entry_masks_system_but_no_display_is_still_installed() {
        let (_temp, roots) = fixture();
        let user = roots.desktop_dirs[0].join("code.desktop");
        let system = roots.desktop_dirs[1].join("code.desktop");
        std::fs::write(&system, entry()).unwrap();
        let found = || apps(spec("vscode").unwrap(), &roots).unwrap();
        assert_eq!(found()[0].path, system);
        std::fs::write(&user, "[Desktop Entry]\nHidden=true\n").unwrap();
        assert!(found().is_empty());
        std::fs::write(&user, format!("{}NoDisplay=true\nVersion=1.5\n", entry())).unwrap();
        assert_eq!(found().len(), 1);
        assert_eq!(found()[0].path, user);
        assert_eq!(
            found()[0].version,
            None,
            "desktop spec version is not app version"
        );
    }

    #[test]
    #[cfg(unix)]
    fn try_exec_is_checked_without_execution_and_without_inventory_path_fallback() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, mut roots) = fixture();
        let user = roots.desktop_dirs[0].join("code.desktop");
        let executable = roots.path[0].join("fixture editor");
        let marker = roots.home.join("MUST_NOT_EXIST");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let raw = format!(
            "{}TryExec={}\n",
            entry(),
            executable.to_str().unwrap().replace(' ', "\\s")
        );
        roots.install_dirs = std::mem::take(&mut roots.path);
        std::fs::write(&user, &raw).unwrap();
        assert!(
            desktop_entry(&user, &roots).unwrap(),
            "absolute TryExec needs no PATH"
        );
        std::fs::write(&user, format!("{}TryExec=fixture\\seditor\n", entry())).unwrap();
        assert!(
            !desktop_entry(&user, &roots).unwrap(),
            "fallback is inventory-only"
        );
        roots.path = roots.install_dirs.clone();
        assert!(desktop_entry(&user, &roots).unwrap());
        assert!(!marker.exists(), "neither Exec nor TryExec is executed");
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!desktop_entry(&user, &roots).unwrap());
    }

    #[test]
    fn invalid_entries_fail_the_scan_without_claiming_absence_or_using_system_fallback() {
        let (_temp, roots) = fixture();
        let user = roots.desktop_dirs[0].join("code.desktop");
        std::fs::write(roots.desktop_dirs[1].join("code.desktop"), entry()).unwrap();
        for raw in [
            "invalid".into(),
            format!("{}Hidden=maybe\n", entry()),
            format!("{}Name=duplicate\n", entry()),
            "[Desktop Entry]\nName=x\n".into(),
            "x".repeat(65537),
        ] {
            std::fs::write(&user, &raw).unwrap();
            assert!(super::super::inventory(&roots, "agentdocker").is_err());
            assert_eq!(std::fs::read_to_string(&user).unwrap(), raw);
        }
        std::fs::write(&user, [0xff, 0xfe]).unwrap();
        assert!(apps(spec("vscode").unwrap(), &roots).is_err());
        std::fs::remove_file(&user).unwrap();
        std::fs::create_dir(&user).unwrap();
        assert!(apps(spec("vscode").unwrap(), &roots).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn exported_symlinks_are_inventory_but_fifos_and_broken_exports_fail_promptly() {
        use std::os::unix::fs::symlink;
        let (_temp, roots) = fixture();
        let user = roots.desktop_dirs[0].join("code.desktop");
        let exported = roots.home.join("exported.desktop");
        std::fs::write(&exported, entry()).unwrap();
        symlink(&exported, &user).unwrap();
        assert!(desktop_entry(&user, &roots).unwrap());
        std::fs::remove_file(exported).unwrap();
        assert!(desktop_entry(&user, &roots).is_err());
        std::fs::remove_file(&user).unwrap();
        let name = std::ffi::CString::new(user.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(desktop_entry(&user, &roots).is_err());
    }
}
