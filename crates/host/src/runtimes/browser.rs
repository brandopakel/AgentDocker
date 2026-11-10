//! Browser extensions in Chromium-family profiles, and the native messaging
//! hosts registered for them: the looking behind the `extensions` of
//! `agentdocker runtimes`. Read-only, and vendor-neutral: every vendor's
//! extension is a directory named by its Web Store id under a profile's
//! `Extensions`, and every bridge from an extension to a program on this
//! machine is a manifest in `NativeMessagingHosts` naming that id among its
//! `allowed_origins`. What the extension is doing is not on disk anywhere.
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use agentdocker_core::runtime::{InstalledExtension, RuntimeSpec};

use super::Roots;

/// One browser's user-data directory: where its profiles and its
/// user-level native messaging host manifests live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserDir {
    pub browser: String,
    pub path: PathBuf,
}

/// Where each Chromium-family browser keeps its user data on this
/// platform. Absence is ordinary; only what exists is read.
pub(super) fn browser_dirs(
    home: &Path,
    os: &str,
    local_app_data: Option<&Path>,
) -> Vec<BrowserDir> {
    let (base, layout): (PathBuf, &[(&str, &str)]) = match os {
        "macos" => (
            home.join("Library/Application Support"),
            &[
                ("Chrome", "Google/Chrome"),
                ("Chrome Beta", "Google/Chrome Beta"),
                ("Chrome Canary", "Google/Chrome Canary"),
                ("Chromium", "Chromium"),
                ("Brave", "BraveSoftware/Brave-Browser"),
                ("Edge", "Microsoft Edge"),
                ("Arc", "Arc/User Data"),
                ("Vivaldi", "Vivaldi"),
            ],
        ),
        "windows" => {
            let Some(local) = local_app_data else {
                return Vec::new();
            };
            (
                local.to_owned(),
                &[
                    ("Chrome", "Google/Chrome/User Data"),
                    ("Chromium", "Chromium/User Data"),
                    ("Brave", "BraveSoftware/Brave-Browser/User Data"),
                    ("Edge", "Microsoft/Edge/User Data"),
                    ("Vivaldi", "Vivaldi/User Data"),
                ],
            )
        }
        _ => (
            home.join(".config"),
            &[
                ("Chrome", "google-chrome"),
                ("Chrome Beta", "google-chrome-beta"),
                ("Chromium", "chromium"),
                ("Brave", "BraveSoftware/Brave-Browser"),
                ("Edge", "microsoft-edge"),
                ("Vivaldi", "vivaldi"),
            ],
        ),
    };
    layout
        .iter()
        .map(|(browser, relative)| BrowserDir {
            browser: (*browser).to_owned(),
            path: base.join(relative),
        })
        .filter(|dir| dir.path.is_absolute())
        .collect()
}

/// A manifest is a few kilobytes; a native messaging host file smaller.
const MAX_JSON_BYTES: u64 = 256 * 1024;
/// Chrome's `Local State` grows with profiles and experiments, not without bound.
const MAX_LOCAL_STATE_BYTES: u64 = 4 * 1024 * 1024;
/// Directory entries examined per listing: a profile directory, an
/// extension's copies, a native messaging host directory.
const MAX_DIR_ENTRIES: usize = 4096;

/// What the inventory found, and what it could not read within its bounds.
/// An unreadable or oversized file is reported, never taken for absence,
/// and so is a directory that cannot be listed: macOS, for one, lets a
/// program list a browser's user data only once the person has allowed it
/// to read other apps' data. That browser is noted and the next one
/// examined, so one closed door never costs the rest of the inventory.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub found: Vec<InstalledExtension>,
    pub incomplete: Vec<String>,
}

/// A JSON file, read only if it is a regular file within `limit` bytes:
/// `Ok(None)` when absent, `Err(why)` for a special, oversized, unreadable
/// or malformed one. Opening never follows a symlink or waits on a FIFO.
fn read_json(path: &Path, limit: u64) -> Result<Option<serde_json::Value>, String> {
    use std::io::Read;
    let file = match crate::files::open_regular(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut text = String::new();
    file.take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if text.len() as u64 > limit {
        return Err(format!(
            "{}: larger than {limit} bytes, not read",
            path.display()
        ));
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{}: not JSON ({e})", path.display()))
}

/// The entries of a directory, at most `MAX_DIR_ENTRIES` of them; the
/// note says when there were more.
fn entries(dir: &Path, notes: &mut Vec<String>) -> io::Result<Vec<PathBuf>> {
    let mut listed = Vec::new();
    let mut more = false;
    for (index, entry) in std::fs::read_dir(dir)?.enumerate() {
        if index >= MAX_DIR_ENTRIES {
            more = true;
            break;
        }
        if let Ok(entry) = entry {
            listed.push(entry.path());
        }
    }
    if more {
        notes.push(format!(
            "{}: more than {MAX_DIR_ENTRIES} entries, not all examined",
            dir.display()
        ));
    }
    listed.sort();
    Ok(listed)
}

/// The vendor's extension in every browser profile it is installed in,
/// with a note for everything the bounds, or the system, kept it from
/// reading.
pub(super) fn extensions(spec: &RuntimeSpec, roots: &Roots) -> Inventory {
    let mut inventory = Inventory::default();
    if spec.extensions.is_empty() {
        return inventory;
    }
    let notes = &mut inventory.incomplete;
    for dir in &roots.browser_dirs {
        match std::fs::metadata(&dir.path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                notes.push(not_examined(&dir.browser, "user data", &dir.path, &e));
                continue;
            }
        }
        // Profiles first: a user data directory that cannot be listed is
        // one note, not another for each thing beneath it.
        let profiles = match profiles(&dir.path, notes) {
            Ok(profiles) => profiles,
            Err(e) => {
                notes.push(not_examined(&dir.browser, "profiles", &dir.path, &e));
                continue;
            }
        };
        let bridges = bridges(&dir.browser, &dir.path.join("NativeMessagingHosts"), notes);
        let names = profile_names(&dir.path.join("Local State"), notes);
        for profile in &profiles {
            for extension in spec.extensions {
                let unpacked = profile.join("Extensions").join(extension.id);
                let Some(version) = newest_version(&dir.browser, &unpacked, notes) else {
                    continue;
                };
                inventory.found.push(InstalledExtension {
                    label: extension.label.to_owned(),
                    browser: dir.browser.clone(),
                    profile: (profiles.len() > 1).then(|| {
                        let directory = profile
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        names.get(&directory).cloned().unwrap_or(directory)
                    }),
                    version,
                    bridge: bridges.get(extension.id).cloned(),
                });
            }
        }
    }
    inventory
}

/// The note for a directory the inventory could not list: whose it is,
/// what in it went unexamined, and the system's own reason.
fn not_examined(browser: &str, what: &str, path: &Path, error: &io::Error) -> String {
    format!("{browser} {what} not examined: {}: {error}", path.display())
}

/// Every profile directory: the ones with an `Extensions` directory.
fn profiles(user_data: &Path, notes: &mut Vec<String>) -> io::Result<Vec<PathBuf>> {
    Ok(entries(user_data, notes)?
        .into_iter()
        .filter(|path| path.join("Extensions").is_dir())
        .collect())
}

/// What the person calls each profile, by directory name, from the
/// browser's `Local State`. Absent or unfamiliar: no names, and the
/// directory names stand; unreadable within bounds: a note, and the same.
fn profile_names(local_state: &Path, notes: &mut Vec<String>) -> BTreeMap<String, String> {
    let state = match read_json(local_state, MAX_LOCAL_STATE_BYTES) {
        Ok(Some(state)) => state,
        Ok(None) => return BTreeMap::new(),
        Err(why) => {
            notes.push(format!("profile names unknown: {why}"));
            return BTreeMap::new();
        }
    };
    state["profile"]["info_cache"]
        .as_object()
        .map(|cache| {
            cache
                .iter()
                .filter_map(|(directory, info)| {
                    info["name"]
                        .as_str()
                        .filter(|name| !name.is_empty())
                        .map(|name| (directory.clone(), name.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The native messaging hosts registered in this user data directory, by
/// the extension id each one admits: whose bridge it is. A manifest that
/// cannot be read within bounds, or a directory of them that cannot be
/// listed, is noted; a manifest that is not JSON is another vendor's
/// problem and is skipped.
fn bridges(browser: &str, hosts: &Path, notes: &mut Vec<String>) -> BTreeMap<String, PathBuf> {
    let listed = match entries(hosts, notes) {
        Ok(listed) => listed,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => {
            notes.push(not_examined(browser, "native messaging hosts", hosts, &e));
            return BTreeMap::new();
        }
    };
    let mut bridges = BTreeMap::new();
    for path in listed {
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let manifest = match read_json(&path, MAX_JSON_BYTES) {
            Ok(Some(manifest)) => manifest,
            Ok(None) => continue,
            Err(why) if why.contains("not JSON") => continue,
            Err(why) => {
                notes.push(format!("native messaging host skipped: {why}"));
                continue;
            }
        };
        let Some(program) = manifest["path"].as_str().filter(|p| !p.is_empty()) else {
            continue;
        };
        for origin in manifest["allowed_origins"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            if let Some(id) = origin
                .strip_prefix("chrome-extension://")
                .map(|rest| rest.trim_end_matches('/'))
                .filter(|id| !id.is_empty())
            {
                bridges
                    .entry(id.to_owned())
                    .or_insert_with(|| PathBuf::from(program));
            }
        }
    }
    bridges
}

/// The version of the newest unpacked copy of an extension: `<version>_<n>`
/// directories, the manifest's own `version` when it says one. A manifest
/// that cannot be read within bounds leaves the copy's name to say the
/// version, with a note; the extension is still there. Copies that cannot
/// be listed are a note too, and nothing is reported found.
fn newest_version(
    browser: &str,
    unpacked: &Path,
    notes: &mut Vec<String>,
) -> Option<Option<String>> {
    let listed = match entries(unpacked, notes) {
        Ok(listed) => listed,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            notes.push(not_examined(browser, "extension", unpacked, &e));
            return None;
        }
    };
    let mut copies: Vec<(Vec<u64>, PathBuf)> = listed
        .into_iter()
        .filter(|path| path.is_dir())
        .map(|path| (version_key(&path), path))
        .collect();
    copies.sort();
    let (_, newest) = copies.pop()?;
    let from_name = || {
        newest
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .map(|name| name.split('_').next().unwrap_or(&name).to_owned())
    };
    let version = match read_json(&newest.join("manifest.json"), MAX_JSON_BYTES) {
        Ok(Some(manifest)) => manifest["version"]
            .as_str()
            .map(str::to_owned)
            .or_else(from_name),
        Ok(None) => from_name(),
        Err(why) => {
            notes.push(format!("extension version taken from its directory: {why}"));
            from_name()
        }
    };
    Some(version)
}

/// `1.0.93_0` sorts after `1.0.9_0` numerically, not lexically.
fn version_key(path: &Path) -> Vec<u64> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .split(['.', '_'])
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::runtime::spec;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A machine with Chrome and Brave: Claude in both Chrome profiles
    /// with the bridge Claude Code registered, ChatGPT in the second
    /// Chrome profile only, and nothing of either in Brave.
    fn machine() -> (tempfile::TempDir, Roots) {
        let tmp = tempfile::tempdir().unwrap();
        let chrome = tmp.path().join("Chrome");
        let brave = tmp.path().join("Brave");
        let claude = spec("claude-browser").unwrap().extensions[0].id;
        let chatgpt = spec("chatgpt-browser").unwrap().extensions[0].id;
        for copy in ["1.0.9_0", "1.0.93_0"] {
            write(
                &chrome.join(format!("Default/Extensions/{claude}/{copy}/manifest.json")),
                &format!(
                    r#"{{"name":"Claude","version":"{}"}}"#,
                    copy.split('_').next().unwrap()
                ),
            );
        }
        // An older copy without a manifest still names its version.
        std::fs::create_dir_all(chrome.join(format!("Profile 1/Extensions/{claude}/1.0.80_0")))
            .unwrap();
        write(
            &chrome.join(format!(
                "Profile 1/Extensions/{chatgpt}/2.1.0_0/manifest.json"
            )),
            r#"{"name":"ChatGPT","version":"2.1.0"}"#,
        );
        write(
            &chrome.join("Local State"),
            r#"{"profile":{"info_cache":{"Default":{"name":"Person 1"},"Profile 1":{"name":"Work"}}}}"#,
        );
        write(
            &chrome.join("NativeMessagingHosts/com.example.bridge.json"),
            &format!(
                r#"{{"name":"com.example.bridge","path":"/opt/example/bridge","type":"stdio","allowed_origins":["chrome-extension://{claude}/"]}}"#
            ),
        );
        write(
            &chrome.join("NativeMessagingHosts/broken.json"),
            "{not json",
        );
        write(&chrome.join("NativeMessagingHosts/notes.txt"), "ignored");
        std::fs::create_dir_all(brave.join("Default/Extensions")).unwrap();
        let roots = Roots {
            home: tmp.path().join("home"),
            codex_home: None,
            claude_config_dir: None,
            path: vec![],
            install_dirs: vec![],
            desktop_dirs: vec![],
            app_dirs: vec![],
            browser_dirs: vec![
                BrowserDir {
                    browser: "Chrome".into(),
                    path: chrome,
                },
                BrowserDir {
                    browser: "Brave".into(),
                    path: brave,
                },
                BrowserDir {
                    browser: "Edge".into(),
                    path: tmp.path().join("absent"),
                },
            ],
            shell: None,
            versions: false,
            hook_stamps: None,
        };
        (tmp, roots)
    }

    #[test]
    fn extensions_are_found_per_profile_with_their_bridge() {
        let (_tmp, roots) = machine();
        let claude = extensions(spec("claude-browser").unwrap(), &roots);
        assert!(claude.incomplete.is_empty(), "{:?}", claude.incomplete);
        assert_eq!(
            claude.found,
            vec![
                InstalledExtension {
                    label: "Claude".into(),
                    browser: "Chrome".into(),
                    profile: Some("Person 1".into()),
                    version: Some("1.0.93".into()),
                    bridge: Some(PathBuf::from("/opt/example/bridge")),
                },
                InstalledExtension {
                    label: "Claude".into(),
                    browser: "Chrome".into(),
                    profile: Some("Work".into()),
                    version: Some("1.0.80".into()),
                    bridge: Some(PathBuf::from("/opt/example/bridge")),
                },
            ]
        );
        let chatgpt = extensions(spec("chatgpt-browser").unwrap(), &roots);
        assert_eq!(chatgpt.found.len(), 1);
        assert_eq!(chatgpt.found[0].profile.as_deref(), Some("Work"));
        assert_eq!(chatgpt.found[0].version.as_deref(), Some("2.1.0"));
        assert_eq!(chatgpt.found[0].bridge, None, "no host admits that id");
        assert!(chatgpt.incomplete.is_empty(), "{:?}", chatgpt.incomplete);
        let none = extensions(spec("claude-code").unwrap(), &roots);
        assert!(none.found.is_empty() && none.incomplete.is_empty());
    }

    #[test]
    fn a_lone_profile_goes_unnamed() {
        let (tmp, mut roots) = machine();
        std::fs::remove_dir_all(tmp.path().join("Chrome/Profile 1")).unwrap();
        roots.browser_dirs.truncate(1);
        let claude = extensions(spec("claude-browser").unwrap(), &roots).found;
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].profile, None);
    }

    /// A FIFO where a manifest should be is not waited on, an oversized
    /// one is not read, and a directory with too many entries is not
    /// walked; each is said, and the extension it concerns is still
    /// reported rather than taken for absent.
    #[cfg(unix)]
    #[test]
    fn special_oversized_and_crowded_inputs_are_bounded_and_reported() {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, roots) = machine();
        let chrome = tmp.path().join("Chrome");
        let claude = spec("claude-browser").unwrap().extensions[0].id;
        // Local State becomes a FIFO: no names, one note, nothing hangs.
        let local_state = chrome.join("Local State");
        std::fs::remove_file(&local_state).unwrap();
        let fifo = |path: &Path| {
            let name = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        };
        fifo(&local_state);
        // The newest copy's manifest is oversized: the version comes from
        // the directory name instead.
        let manifest = chrome.join(format!(
            "Default/Extensions/{claude}/1.0.93_0/manifest.json"
        ));
        std::fs::write(&manifest, "x".repeat(MAX_JSON_BYTES as usize + 1)).unwrap();
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o644)).unwrap();
        // A native messaging host that is a FIFO is skipped with a note,
        // and the readable one still names its bridge.
        fifo(&chrome.join("NativeMessagingHosts/stuck.json"));
        let started = std::time::Instant::now();
        let inventory = extensions(spec("claude-browser").unwrap(), &roots);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "no FIFO was waited on"
        );
        let default = inventory
            .found
            .iter()
            .find(|e| e.profile.as_deref() == Some("Default"))
            .expect("the extension is still reported");
        assert_eq!(
            default.version.as_deref(),
            Some("1.0.93"),
            "from the directory name"
        );
        assert_eq!(default.bridge, Some(PathBuf::from("/opt/example/bridge")));
        let notes = inventory.incomplete.join("\n");
        assert!(notes.contains("profile names unknown"), "{notes}");
        assert!(notes.contains("larger than"), "{notes}");
        assert!(notes.contains("stuck.json"), "{notes}");

        // A crowded directory: only the first MAX_DIR_ENTRIES are examined.
        let crowded = tmp.path().join("Crowded");
        for index in 0..(MAX_DIR_ENTRIES + 3) {
            std::fs::create_dir_all(crowded.join(format!("p{index:05}/Extensions"))).unwrap();
        }
        let mut roots = roots;
        roots.browser_dirs = vec![BrowserDir {
            browser: "Crowded".into(),
            path: crowded.clone(),
        }];
        let inventory = extensions(spec("claude-browser").unwrap(), &roots);
        assert!(inventory.found.is_empty());
        assert!(
            inventory
                .incomplete
                .iter()
                .any(|n| n.contains("more than") && n.contains("not all examined")),
            "{:?}",
            inventory.incomplete
        );
    }

    /// A directory the system will not list (as macOS will not list a
    /// browser's user data for a program not allowed to read other apps'
    /// data) is a note naming it, and everything else is still examined:
    /// the next browser, the profile beside it, the extension whose
    /// bridges went unread.
    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_listed_is_noted_and_the_rest_examined() {
        use std::os::unix::fs::PermissionsExt;
        /// Gives back what the test took away, so the temp dir can go.
        struct Reopen(Vec<PathBuf>);
        impl Drop for Reopen {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
                }
            }
        }
        let close = |path: &Path, reopen: &mut Reopen| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
            reopen.0.push(path.to_owned());
        };
        let (tmp, mut roots) = machine();
        let claude = spec("claude-browser").unwrap().extensions[0].id;
        let mut reopen = Reopen(Vec::new());

        // A whole browser closed, ahead of Chrome: one note, and Chrome's
        // two profiles are still found with their bridge.
        let closed = tmp.path().join("Closed");
        std::fs::create_dir_all(closed.join(format!("Default/Extensions/{claude}/1.0.0_0")))
            .unwrap();
        close(&closed, &mut reopen);
        if std::fs::read_dir(&closed).is_ok() {
            // Root: a mode closes nothing, and there is nothing to test.
            return;
        }
        roots.browser_dirs.insert(
            0,
            BrowserDir {
                browser: "Closed".into(),
                path: closed.clone(),
            },
        );
        let inventory = extensions(spec("claude-browser").unwrap(), &roots);
        assert_eq!(inventory.found.len(), 2, "{:?}", inventory.found);
        assert!(inventory.found.iter().all(|e| e.browser == "Chrome"));
        assert!(
            inventory.found.iter().all(|e| e.bridge.is_some()),
            "{:?}",
            inventory.found
        );
        assert_eq!(inventory.incomplete.len(), 1, "{:?}", inventory.incomplete);
        let note = &inventory.incomplete[0];
        assert!(note.starts_with("Closed profiles not examined: "), "{note}");
        assert!(note.contains(&closed.display().to_string()), "{note}");

        // Inside Chrome: the bridges and one profile's copies of the
        // extension closed. The other profile is still found, without a
        // bridge it could not read, and each closed door is its own note.
        let chrome = tmp.path().join("Chrome");
        close(&chrome.join("NativeMessagingHosts"), &mut reopen);
        close(
            &chrome.join(format!("Profile 1/Extensions/{claude}")),
            &mut reopen,
        );
        let inventory = extensions(spec("claude-browser").unwrap(), &roots);
        assert_eq!(
            inventory.found,
            vec![InstalledExtension {
                label: "Claude".into(),
                browser: "Chrome".into(),
                profile: Some("Person 1".into()),
                version: Some("1.0.93".into()),
                bridge: None,
            }]
        );
        let notes = inventory.incomplete.join("\n");
        assert!(notes.contains("Closed profiles not examined"), "{notes}");
        assert!(
            notes.contains("Chrome native messaging hosts not examined"),
            "{notes}"
        );
        assert!(notes.contains("Chrome extension not examined"), "{notes}");
        assert_eq!(inventory.incomplete.len(), 3, "{notes}");
    }

    #[test]
    fn platform_layouts_are_absolute_and_under_the_home() {
        let home = Path::new(if cfg!(windows) {
            r"C:\Users\p"
        } else {
            "/home/p"
        });
        for os in ["macos", "linux"] {
            let dirs = browser_dirs(home, os, None);
            assert!(dirs.iter().any(|d| d.browser == "Chrome"), "{os}");
            assert!(dirs.iter().all(|d| d.path.starts_with(home)), "{os}");
        }
        assert!(browser_dirs(home, "windows", None).is_empty());
        let local = home.join("AppData/Local");
        let windows = browser_dirs(home, "windows", Some(&local));
        assert!(windows.iter().all(|d| d.path.starts_with(&local)));
    }
}
