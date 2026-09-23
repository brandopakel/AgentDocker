//! One coordination source for provider skills and MCP onboarding.
use agentdocker_host::runtimes::Roots;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub const DOCUMENT: &str = include_str!("../skills/agentdocker/SKILL.md");

const STAMP: &str = "<!-- agentdocker-source-sha256: ";

pub fn installed_document() -> String {
    stamp(DOCUMENT)
}

fn stamp(body: &str) -> String {
    format!("{body}{STAMP}{:x} -->\n", Sha256::digest(body.as_bytes()))
}

/// A generated skill may be upgraded only while its complete installed body
/// still matches its source stamp. Any user edit preserves the file instead.
pub fn unmodified_install(contents: &str) -> bool {
    let Some((body, digest)) = contents.rsplit_once(STAMP) else {
        return false;
    };
    digest == format!("{:x} -->\n", Sha256::digest(body.as_bytes()))
}

/// Provider-specific delivery instructions remain authoritative. The shared
/// manual section is omitted for adapters whose controller/channel owns input.
pub fn instructions(manual: bool) -> &'static str {
    instructions_from(DOCUMENT, manual)
}

fn instructions_from(document: &str, manual: bool) -> &str {
    let body = document
        .split_once("\n---\n")
        .or_else(|| document.split_once("\r\n---\r\n"))
        .expect("bundled skill frontmatter")
        .1
        .trim();
    let (end, windows_end) = if manual {
        ("\n## Command-line access", "\r\n## Command-line access")
    } else {
        ("\n## Manual inbox delivery", "\r\n## Manual inbox delivery")
    };
    body.split_once(windows_end)
        .or_else(|| body.split_once(end))
        .expect("bundled skill sections")
        .0
}

/// Only documented skill loaders are configured. In particular, a desktop
/// product from the same company is not assumed to share its CLI's loader.
pub fn path(runtime: &str, roots: &Roots) -> Option<PathBuf> {
    let root = match runtime {
        "codex" => roots
            .codex_home
            .clone()
            .unwrap_or_else(|| roots.home.join(".codex")),
        "claude-code" => roots
            .claude_config_dir
            .clone()
            .unwrap_or_else(|| roots.home.join(".claude")),
        "gemini-cli" => roots.home.join(".gemini"),
        _ => return None,
    };
    Some(root.join("skills/agentdocker/SKILL.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_accepts_lf_and_windows_checkout_crlf() {
        let unix = DOCUMENT.replace("\r\n", "\n");
        let windows = unix.replace('\n', "\r\n");
        for manual in [false, true] {
            let expected = instructions_from(&unix, manual);
            assert!(!expected.is_empty());
            assert_eq!(
                instructions_from(&windows, manual).replace("\r\n", "\n"),
                expected
            );
            assert_eq!(expected.contains("## Manual inbox delivery"), manual);
            assert!(!expected.contains("## Command-line access"));
        }
    }
}
