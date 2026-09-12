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

fn concrete_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let absolute = path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'));
    absolute && path.len() <= 16_000 && path.trim() == path && !path.chars().any(char::is_control)
}

impl QuestionPermissions {
    pub fn valid(&self) -> bool {
        let mut paths = std::collections::HashSet::new();
        let mut grants = self
            .network
            .as_ref()
            .is_some_and(|n| n.enabled == Some(true));
        if let Some(files) = &self.file_system {
            if files.glob_scan_max_depth.is_some() {
                return false;
            }
            for path in files
                .read
                .iter()
                .flatten()
                .chain(files.write.iter().flatten())
            {
                if !concrete_path(path) || !paths.insert(path) {
                    return false;
                }
                grants = true;
            }
            for entry in files.entries.iter().flatten() {
                let QuestionPermissionPath::Path { path } = &entry.path;
                if !concrete_path(path) || !paths.insert(path) {
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
                    "Network access disabled"
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
                    QuestionPermissionAccess::Deny => "Block",
                };
                lines.push(format!("{access}: {path}"));
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
                "Block: /owned/private"
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
