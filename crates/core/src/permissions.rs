//! The concrete subset of Codex permission requests that every client can show
//! completely. Unknown selectors are rejected rather than silently omitted.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QuestionPermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<QuestionNetworkPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_system: Option<QuestionFileSystemPermissions>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionNetworkPermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QuestionFileSystemPermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<QuestionPermissionEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob_scan_max_depth: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionPermissionEntry {
    pub access: QuestionPermissionAccess,
    pub path: QuestionPermissionPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionPermissionAccess {
    Read,
    Write,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuestionPermissionPath {
    Path { path: String },
}

/// A lexical comparison key only: never rewrite the reviewed/wire profile or
/// resolve filesystem aliases in core. Refuse dot segments instead of reducing
/// them, since a preceding component could be a symlink on the provider host.
fn concrete_path_key(path: &str) -> Option<String> {
    if path.len() > 16_000
        || path.trim() != path
        || path.chars().any(|ch| {
            ch.is_control()
                || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        return None;
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    let unc = path.starts_with("\\\\");
    if drive || unc {
        // This portable review subset does not pretend to implement a host's
        // Unicode case mapping, device namespaces or alternate data streams.
        if !path.is_ascii() {
            return None;
        }
        let key = path.replace('\\', "/").to_ascii_lowercase();
        let tail = &key[if drive { 3 } else { 2 }..];
        if drive && tail.is_empty() {
            return Some(key);
        }
        let components: Vec<_> = tail.split('/').collect();
        if unc && components.len() < 2 {
            return None;
        }
        if components.iter().any(|part| {
            let stem = part.split('.').next().unwrap_or_default();
            let device = matches!(stem, "con" | "prn" | "aux" | "nul")
                || ((stem.starts_with("com") || stem.starts_with("lpt"))
                    && stem.len() == 4
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'));
            part.is_empty()
                || part.ends_with(['.', ' '])
                || device
                || part.contains(['<', '>', ':', '"', '|', '?', '*'])
        }) {
            return None;
        }
        return Some(key);
    }
    let tail = path.strip_prefix('/')?;
    if !tail.is_empty() && tail.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return None;
    }
    Some(path.to_owned())
}

impl QuestionPermissions {
    pub fn valid(&self) -> bool {
        let mut paths = std::collections::HashMap::new();
        let mut grants = self
            .network
            .as_ref()
            .is_some_and(|n| n.enabled == Some(true));
        if let Some(files) = &self.file_system {
            if files.glob_scan_max_depth.is_some() {
                return false;
            }
            for (values, access) in [
                (&files.read, QuestionPermissionAccess::Read),
                (&files.write, QuestionPermissionAccess::Write),
            ] {
                for path in values.iter().flatten() {
                    let Some(key) = concrete_path_key(path) else {
                        return false;
                    };
                    if paths.insert(key, access).is_some() {
                        return false;
                    }
                    grants = true;
                }
            }
            let mut entries = std::collections::HashSet::new();
            for entry in files.entries.iter().flatten() {
                let QuestionPermissionPath::Path { path } = &entry.path;
                let Some(key) = concrete_path_key(path) else {
                    return false;
                };
                if !entries.insert(key.clone()) {
                    return false;
                }
                // Codex 0.153.4 mirrors concrete entries in legacy lists.
                // Accept only an identical mirror, never conflicting access.
                if paths
                    .insert(key, entry.access)
                    .is_some_and(|old| old != entry.access)
                {
                    return false;
                }
                grants |= entry.access != QuestionPermissionAccess::Deny;
            }
        }
        grants && paths.len() <= 16
    }

    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(enabled) = self.network.as_ref().and_then(|n| n.enabled) {
            lines.push(
                if enabled {
                    "Allow network access"
                } else {
                    "No additional network access"
                }
                .into(),
            );
        }
        if let Some(files) = &self.file_system {
            lines.extend(files.read.iter().flatten().map(|p| format!("Read: {p}")));
            lines.extend(files.write.iter().flatten().map(|p| format!("Write: {p}")));
            for entry in files.entries.iter().flatten() {
                let QuestionPermissionPath::Path { path } = &entry.path;
                let access = match entry.access {
                    QuestionPermissionAccess::Read => "Read",
                    QuestionPermissionAccess::Write => "Write",
                    QuestionPermissionAccess::Deny => "Exclude",
                };
                lines.push(format!("{access}: {path}"));
            }
        }
        let mut shown = std::collections::HashSet::new();
        lines.retain(|line| shown.insert(line.clone()));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_paths_refuse_direction_overrides_but_keep_ordinary_unicode() {
        for code in [0x061c, 0x200e, 0x200f]
            .into_iter()
            .chain(0x202a..=0x202e)
            .chain(0x2066..=0x2069)
        {
            let path = format!("/tmp/review{}txt", char::from_u32(code).unwrap());
            assert!(concrete_path_key(&path).is_none(), "U+{code:04X}");
        }
        for path in ["/tmp/日本語", "/tmp/العربية", "/tmp/עברית"] {
            assert_eq!(concrete_path_key(path).as_deref(), Some(path));
        }
        let longest = format!("/{}", "x".repeat(15_999));
        assert!(concrete_path_key(&longest).is_some());
        assert!(concrete_path_key(&(longest + "x")).is_none());
        assert!(concrete_path_key(" /tmp/review").is_none());
        assert!(concrete_path_key("/tmp/review ").is_none());
    }
    use serde_json::json;

    #[test]
    fn ambiguous_and_noncanonical_permission_paths_are_refused() {
        for path in [
            "/owned/../private",
            "/owned/./file",
            "/owned//file",
            "/owned/",
            r"C:\owned\..\private",
            r"C:\owned\.\file",
            r"C:\owned\\file",
            r"\\server",
            r"\\server\",
            r"\\server\\file",
            r"\\?\C:\file",
            r"\\.\NUL",
            r"C:\owned\file.",
            r"C:\owned\file ",
            r"C:\owned\file:stream",
            r"C:\owned\NUL.txt",
            r"C:\owned\COM1",
            r"C:\owned\é",
        ] {
            let profile = json!({"fileSystem":{"write":[path]}});
            assert!(
                !serde_json::from_value::<QuestionPermissions>(profile)
                    .unwrap()
                    .valid(),
                "{path}"
            );
        }
        for path in [
            "/",
            "/owned/日本語",
            r"C:\",
            r"C:\owned\file",
            r"\\server\share\file",
        ] {
            assert!(concrete_path_key(path).is_some(), "{path}");
        }
    }

    #[test]
    fn windows_equivalent_paths_share_conflict_checks_without_rewriting_grants() {
        for (left, right) in [
            (r"C:\Owned\File", "c:/owned/file"),
            (r"\\Server\Share\File", r"\\server\share/file"),
        ] {
            for wire in [
                json!({"fileSystem":{"read":[left],"write":[right]}}),
                json!({"fileSystem":{"write":[left,right]}}),
                json!({"fileSystem":{"write":[left],"entries":[{"access":"deny","path":{"type":"path","path":right}}]}}),
                json!({"fileSystem":{"entries":[{"access":"write","path":{"type":"path","path":left}},{"access":"write","path":{"type":"path","path":right}}]}}),
            ] {
                assert!(
                    !serde_json::from_value::<QuestionPermissions>(wire)
                        .unwrap()
                        .valid()
                );
            }
            let wire = json!({"fileSystem":{"write":[left],"entries":[{"access":"write","path":{"type":"path","path":right}}]}});
            let parsed: QuestionPermissions = serde_json::from_value(wire.clone()).unwrap();
            assert!(parsed.valid());
            assert_eq!(serde_json::to_value(parsed).unwrap(), wire);
        }
    }

    #[test]
    fn matching_legacy_permission_mirrors_are_preserved_but_shown_once() {
        let wire = json!({"fileSystem":{"write":["/owned/output"],"entries":[{"path":{"type":"path","path":"/owned/output"},"access":"write"}]}});
        let parsed: QuestionPermissions = serde_json::from_value(wire.clone()).unwrap();
        assert!(parsed.valid());
        assert_eq!(parsed.lines(), ["Write: /owned/output"]);
        assert_eq!(serde_json::to_value(parsed).unwrap(), wire);
        let mut conflict = wire.clone();
        conflict["fileSystem"]["entries"][0]["access"] = json!("deny");
        assert!(
            !serde_json::from_value::<QuestionPermissions>(conflict)
                .unwrap()
                .valid()
        );
        let mut repeated = wire;
        let first = repeated["fileSystem"]["entries"][0].clone();
        repeated["fileSystem"]["entries"]
            .as_array_mut()
            .unwrap()
            .push(first);
        assert!(
            !serde_json::from_value::<QuestionPermissions>(repeated)
                .unwrap()
                .valid()
        );
    }

    #[test]
    fn concrete_profiles_are_complete_bounded_and_preserve_restrictions() {
        let profile = json!({"network":{"enabled":true},"fileSystem":{"read":["/owned/input"],"entries":[{"access":"write","path":{"type":"path","path":"C:\\owned\\output"}},{"access":"deny","path":{"type":"path","path":"/owned/private"}}]}});
        let parsed: QuestionPermissions = serde_json::from_value(profile.clone()).unwrap();
        assert!(parsed.valid());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), profile);
        assert_eq!(
            parsed.lines(),
            [
                "Allow network access",
                "Read: /owned/input",
                "Write: C:\\owned\\output",
                "Exclude: /owned/private"
            ]
        );
        for profile in [
            json!({}),
            json!({"network":{"enabled":false}}),
            json!({"fileSystem":{"read":["relative"]}}),
            json!({"fileSystem":{"read":["/owned\nhidden"]}}),
            json!({"fileSystem":{"read":["/same"],"write":["/same"]}}),
            json!({"fileSystem":{"entries":[{"access":"deny","path":{"type":"path","path":"/only-deny"}}]}}),
            json!({"network":{"enabled":true},"fileSystem":{"globScanMaxDepth":1}}),
            json!({"fileSystem":{"read":(0..17).map(|i| format!("/path/{i}")).collect::<Vec<_>>()}}),
        ] {
            assert!(
                !serde_json::from_value::<QuestionPermissions>(profile)
                    .unwrap()
                    .valid()
            );
        }
    }

    #[test]
    fn unknown_permission_fields_and_non_concrete_selectors_are_refused() {
        for profile in [
            json!({"network":{"enabled":true,"hosts":["example.com"]}}),
            json!({"environment":"remote"}),
            json!({"fileSystem":{"other":true}}),
            json!({"fileSystem":{"entries":[{"access":"write","path":{"type":"glob_pattern","pattern":"/**"}}]}}),
            json!({"fileSystem":{"entries":[{"access":"write","path":{"type":"special","value":{"kind":"root"}}}]}}),
        ] {
            assert!(serde_json::from_value::<QuestionPermissions>(profile).is_err());
        }
    }
}
