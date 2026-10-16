//! Read the saved project trust entry for the desktop launch notice.
//! Missing trust does not establish effective sandbox or approval policy:
//! explicit provider configuration can override trust-derived defaults.
//! The notice directs the person to review Codex's settings without changing them.

use std::path::{Path, PathBuf};

/// Codex's configuration: `$CODEX_HOME/config.toml`, else
/// `~/.codex/config.toml`.
pub fn config_path() -> Option<PathBuf> {
    match std::env::var_os("CODEX_HOME").filter(|p| !p.is_empty()) {
        Some(home) => Some(PathBuf::from(home).join("config.toml")),
        None => std::env::home_dir().map(|home| home.join(".codex").join("config.toml")),
    }
}

/// What a Codex configuration says of `root`: trusted only when its
/// `[projects."<root>"]` table says `trust_level = "trusted"`. Codex keys
/// trust by the project's own root (a worktree resolves to its main
/// repository), so a trusted parent folder does not count. `None` when
/// the text is not a configuration Codex could read.
pub fn trusts(config: &str, root: &Path) -> Option<bool> {
    let root = root.to_str()?;
    let table = config.parse::<toml::Table>().ok()?;
    let level = table
        .get("projects")
        .and_then(toml::Value::as_table)
        .and_then(|projects| projects.get(root))
        .and_then(|project| project.get("trust_level"))
        .and_then(toml::Value::as_str);
    Some(level == Some("trusted"))
}

/// Whether this machine's Codex trusts `root`; `None` when there is no
/// configuration to read, so nothing is claimed.
pub fn trusted(root: &Path) -> Option<bool> {
    trusts(&std::fs::read_to_string(config_path()?).ok()?, root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_root_has_unknown_trust() {
        use std::os::unix::ffi::OsStrExt;
        let root = Path::new(std::ffi::OsStr::from_bytes(b"/project/\xff"));
        assert_eq!(trusts("", root), None);
    }

    #[test]
    fn only_the_projects_own_trusted_entry_counts() {
        let config = r#"
model = "gpt-5"
[projects."/home/me"]
trust_level = "trusted"
[projects."/home/me/app"]
trust_level = "trusted"
[projects."/home/me/other"]
trust_level = "untrusted"
"#;
        assert_eq!(trusts(config, Path::new("/home/me/app")), Some(true));
        assert_eq!(trusts(config, Path::new("/home/me/other")), Some(false));
        // A trusted parent does not make a new project trusted.
        assert_eq!(trusts(config, Path::new("/home/me/new")), Some(false));
        assert_eq!(trusts("model = \"gpt-5\"\n", Path::new("/x")), Some(false));
        assert_eq!(trusts("not [ toml", Path::new("/x")), None);
    }
}
