//! The OpenCode plugin `agentdocker setup opencode` installs: OpenCode's
//! counterpart of the Claude Code hooks (leases on edits, messages, the
//! journal, idle wake-up), run by OpenCode from its global plugin directory.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use agentdocker_host::runtimes::Roots;

const SOURCE: &str = include_str!("../opencode/agentdocker.js");
const STAMP: &str = "// agentdocker-source-sha256: ";

/// OpenCode loads every module in `~/.config/opencode/plugins/` at startup.
pub fn path(roots: &Roots) -> PathBuf {
    roots.home.join(".config/opencode/plugins/agentdocker.js")
}

/// The plugin as installed: the source with this executable's path, and a
/// stamp over it so an unmodified copy can be replaced and an edited one is
/// never overwritten.
pub fn document(executable: &Path) -> String {
    let quoted = serde_json::to_string(&executable.to_string_lossy()).expect("a path is JSON");
    let body = SOURCE.replace("\"__AGENTDOCKER__\"", &quoted);
    format!("{body}{STAMP}{:x}\n", Sha256::digest(body.as_bytes()))
}

/// Whether an installed plugin is still exactly one this command wrote.
pub fn unmodified_install(contents: &str) -> bool {
    let Some((body, digest)) = contents.rsplit_once(STAMP) else {
        return false;
    };
    digest == format!("{:x}\n", Sha256::digest(body.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_runs_this_executable_and_knows_its_own_copy() {
        let installed = document(Path::new("/Applications/Agent Docker/agentdocker"));
        assert!(installed.contains(r#"const AGENTDOCKER = "/Applications/Agent Docker/agentdocker""#));
        assert!(!installed.contains("__AGENTDOCKER__"));
        assert!(unmodified_install(&installed));
        let edited = installed.replace("timeout: 5000", "timeout: 9000");
        assert!(!unmodified_install(&edited), "a person's edit is theirs");
        assert!(!unmodified_install("export const Mine = async () => ({})\n"));
    }
}
