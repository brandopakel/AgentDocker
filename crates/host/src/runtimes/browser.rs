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

fn error(path: &Path, detail: &str) -> io::Error {
    io::Error::other(format!("browser inventory at {}: {detail}", path.display()))
}

/// The vendor's extension in every browser profile it is installed in.
pub(super) fn extensions(spec: &RuntimeSpec, roots: &Roots) -> io::Result<Vec<InstalledExtension>> {
    let mut found = Vec::new();
    if spec.extensions.is_empty() {
        return Ok(found);
    }
    for dir in &roots.browser_dirs {
        match std::fs::metadata(&dir.path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(error(&dir.path, "user data cannot be inspected")),
        }
        let bridges = bridges(&dir.path.join("NativeMessagingHosts"))?;
        let profiles = profiles(&dir.path)?;
        let names = profile_names(&dir.path.join("Local State"));
        for profile in &profiles {
            for extension in spec.extensions {
                let unpacked = profile.join("Extensions").join(extension.id);
                let Some(version) = newest_version(&unpacked)? else {
                    continue;
                };
                found.push(InstalledExtension {
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
    Ok(found)
}

/// Every profile directory: the ones with an `Extensions` directory.
fn profiles(user_data: &Path) -> io::Result<Vec<PathBuf>> {
    let mut profiles: Vec<PathBuf> = std::fs::read_dir(user_data)
        .map_err(|_| error(user_data, "profiles cannot be listed"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("Extensions").is_dir())
        .collect();
    profiles.sort();
    Ok(profiles)
}

/// What the person calls each profile, by directory name, from the
/// browser's `Local State`. Unreadable or unfamiliar: no names, and the
/// directory names stand.
fn profile_names(local_state: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(local_state) else {
        return BTreeMap::new();
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        return BTreeMap::new();
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
/// the extension id each one admits: whose bridge it is. Another vendor's
/// malformed manifest is not our failure and is skipped.
fn bridges(hosts: &Path) -> io::Result<BTreeMap<String, PathBuf>> {
    let entries = match std::fs::read_dir(hosts) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(_) => return Err(error(hosts, "native messaging hosts cannot be listed")),
    };
    let mut bridges = BTreeMap::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
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
    Ok(bridges)
}

/// The version of the newest unpacked copy of an extension: `<version>_<n>`
/// directories, the manifest's own `version` when it says one.
fn newest_version(unpacked: &Path) -> io::Result<Option<Option<String>>> {
    let entries = match std::fs::read_dir(unpacked) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error(unpacked, "extension cannot be listed")),
    };
    let mut copies: Vec<(Vec<u64>, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| (version_key(&path), path))
        .collect();
    copies.sort();
    let Some((_, newest)) = copies.pop() else {
        return Ok(None);
    };
    let version = std::fs::read_to_string(newest.join("manifest.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|manifest| manifest["version"].as_str().map(str::to_owned))
        .or_else(|| {
            newest
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .map(|name| name.split('_').next().unwrap_or(&name).to_owned())
        });
    Ok(Some(version))
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
        };
        (tmp, roots)
    }

    #[test]
    fn extensions_are_found_per_profile_with_their_bridge() {
        let (_tmp, roots) = machine();
        let claude = extensions(spec("claude-browser").unwrap(), &roots).unwrap();
        assert_eq!(
            claude,
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
        let chatgpt = extensions(spec("chatgpt-browser").unwrap(), &roots).unwrap();
        assert_eq!(chatgpt.len(), 1);
        assert_eq!(chatgpt[0].profile.as_deref(), Some("Work"));
        assert_eq!(chatgpt[0].version.as_deref(), Some("2.1.0"));
        assert_eq!(chatgpt[0].bridge, None, "no host admits that id");
        assert!(
            extensions(spec("claude-code").unwrap(), &roots)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_lone_profile_goes_unnamed() {
        let (tmp, mut roots) = machine();
        std::fs::remove_dir_all(tmp.path().join("Chrome/Profile 1")).unwrap();
        roots.browser_dirs.truncate(1);
        let claude = extensions(spec("claude-browser").unwrap(), &roots).unwrap();
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].profile, None);
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
