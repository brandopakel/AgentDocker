//! The person's shell: whether a `claude` typed in a terminal carries the
//! channel flag that lets AgentDocker wake it. Claude Code honours a
//! channel only when the session was started with
//! `--dangerously-load-development-channels server:agentdocker`, and
//! during the research preview no setting replaces the flag; so the one
//! place that can add it to every terminal launch is the shell's own
//! startup file, as a marked block `agentdocker setup --shell` writes
//! and can take back.
use std::path::{Path, PathBuf};

use agentdocker_core::runtime::Wiring;

use super::Roots;

pub const BEGIN: &str = "# >>> agentdocker >>>";
pub const END: &str = "# <<< agentdocker <<<";
/// The flag Claude Code takes during the research preview, naming our
/// MCP server entry.
pub const CLAUDE_CHANNEL_FLAG: &str = "--dangerously-load-development-channels server:agentdocker";

/// The person's login shell: `$SHELL` when a shell set it, else the
/// passwd entry, which is what a daemon or an app started by launchd or
/// systemd — with no shell in its environment — has to go by.
pub fn login_shell() -> Option<String> {
    if let Some(shell) = std::env::var_os("SHELL")
        && !shell.is_empty()
    {
        return Some(shell.to_string_lossy().into_owned());
    }
    passwd_shell()
}

#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    // SAFETY: getpwuid_r writes into buffers we own and returns a pointer
    // into them; nothing is retained past this function.
    unsafe {
        let mut entry: libc::passwd = std::mem::zeroed();
        let mut buffer = vec![0u8; 4096];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let status = libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        );
        if status != 0 || result.is_null() || entry.pw_shell.is_null() {
            return None;
        }
        let shell = std::ffi::CStr::from_ptr(entry.pw_shell)
            .to_string_lossy()
            .into_owned();
        (!shell.is_empty()).then_some(shell)
    }
}

#[cfg(not(unix))]
fn passwd_shell() -> Option<String> {
    None
}

/// A shell whose startup file we know how to extend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    pub name: &'static str,
    pub rc: PathBuf,
}

/// The person's login shell from `$SHELL`, when it is one of the three we
/// know; anything else is not supported rather than guessed at.
pub fn shell(roots: &Roots) -> Option<Shell> {
    let name = roots.shell.as_deref()?;
    let name = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name);
    let (name, rc) = match name {
        "zsh" => ("zsh", roots.home.join(".zshrc")),
        "bash" => ("bash", roots.home.join(".bashrc")),
        "fish" => ("fish", roots.home.join(".config/fish/config.fish")),
        _ => return None,
    };
    Some(Shell { name, rc })
}

/// The block for a shell: a function named `claude` that runs the real
/// `claude` with the flag, passing every argument through.
pub fn block(shell: &str) -> String {
    let body = match shell {
        "fish" => format!(
            "function claude\n    AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 command claude {CLAUDE_CHANNEL_FLAG} $argv\nend"
        ),
        _ => format!(
            "claude() {{ AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 command claude {CLAUDE_CHANNEL_FLAG} \"$@\"; }}"
        ),
    };
    format!(
        "{BEGIN} wake idle Claude Code sessions: every `claude` carries the channel flag (agentdocker setup --shell)\n{body}\n{END}\n"
    )
}

/// The current block in a startup file, when one is there.
pub fn current_block(text: &str) -> Option<&str> {
    let start = text.find(BEGIN)?;
    let end = text[start..].find(END)? + start + END.len();
    let end = text[end..]
        .find('\n')
        .map(|offset| end + offset + 1)
        .unwrap_or(text.len());
    Some(&text[start..end])
}

/// Whether the person's terminal launches carry the flag.
pub fn wiring(roots: &Roots) -> Wiring {
    let Some(shell) = shell(roots) else {
        return Wiring::Unsupported;
    };
    match std::fs::read_to_string(&shell.rc) {
        Ok(text) => match current_block(&text) {
            Some(found) if found == block(shell.name) => Wiring::Wired,
            Some(_) => Wiring::Unverified,
            None => Wiring::Missing,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Wiring::Missing,
        Err(_) => Wiring::Unverified,
    }
}

/// The startup file with our block: replaced in place when an older one
/// is there, appended after a blank line otherwise. Nothing else moves.
pub fn with_block(before: Option<&str>, shell: &str) -> String {
    let block = block(shell);
    match before {
        Some(text) => match current_block(text) {
            Some(found) => text.replacen(found, &block, 1),
            None => {
                let mut after = text.to_owned();
                if !after.is_empty() && !after.ends_with('\n') {
                    after.push('\n');
                }
                if !after.is_empty() && !after.ends_with("\n\n") {
                    after.push('\n');
                }
                after.push_str(&block);
                after
            }
        },
        None => block,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(shell: Option<&str>, home: &Path) -> Roots {
        Roots {
            home: home.to_owned(),
            codex_home: None,
            claude_config_dir: None,
            path: vec![],
            install_dirs: vec![],
            desktop_dirs: vec![],
            app_dirs: vec![],
            browser_dirs: vec![],
            shell: shell.map(str::to_owned),
            versions: false,
        }
    }

    #[test]
    fn the_block_is_appended_once_replaced_in_place_and_recognised() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = roots(Some("/bin/zsh"), tmp.path());
        assert_eq!(wiring(&roots), Wiring::Missing, "no file yet");
        let rc = tmp.path().join(".zshrc");
        std::fs::write(&rc, "export PATH=$HOME/bin:$PATH").unwrap();
        assert_eq!(wiring(&roots), Wiring::Missing);
        let after = with_block(Some("export PATH=$HOME/bin:$PATH"), "zsh");
        assert!(after.starts_with("export PATH=$HOME/bin:$PATH\n\n# >>> agentdocker >>>"));
        assert!(after.contains(
            "claude() { AGENTDOCKER_CLAUDE_CHANNEL_INPUT=1 command claude --dangerously-load-development-channels server:agentdocker \"$@\"; }"
        ));
        assert!(after.ends_with("# <<< agentdocker <<<\n"));
        std::fs::write(&rc, &after).unwrap();
        assert_eq!(wiring(&roots), Wiring::Wired);
        // An older block is replaced, and the text around it stays.
        let old = format!(
            "alias ll='ls -l'\n{BEGIN} old\nclaude() {{ command claude; }}\n{END}\nexport EDITOR=vi\n"
        );
        std::fs::write(&rc, &old).unwrap();
        assert_eq!(wiring(&roots), Wiring::Unverified);
        let replaced = with_block(Some(&old), "zsh");
        assert!(replaced.starts_with("alias ll='ls -l'\n# >>> agentdocker >>>"));
        assert!(replaced.ends_with("# <<< agentdocker <<<\nexport EDITOR=vi\n"));
        assert_eq!(current_block(&replaced), Some(block("zsh").as_str()));
        assert_eq!(with_block(None, "zsh"), block("zsh"));
    }

    /// `$SHELL` when set; the passwd entry otherwise, which never panics
    /// and on a machine with a login shell names one.
    #[test]
    fn the_login_shell_comes_from_the_environment_or_the_passwd_entry() {
        if let Some(shell) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
            assert_eq!(login_shell().as_deref(), shell.to_str());
        }
        let from_passwd = passwd_shell();
        if cfg!(unix)
            && let Some(shell) = from_passwd
        {
            assert!(shell.starts_with('/'), "{shell}");
        }
    }

    #[test]
    fn shells_are_known_by_name_and_fish_gets_its_own_syntax() {
        let home = Path::new("/h");
        assert_eq!(
            shell(&roots(Some("/opt/homebrew/bin/fish"), home)).unwrap(),
            Shell {
                name: "fish",
                rc: home.join(".config/fish/config.fish")
            }
        );
        assert_eq!(
            shell(&roots(Some("bash"), home)).unwrap().rc,
            home.join(".bashrc")
        );
        assert!(shell(&roots(Some("/bin/tcsh"), home)).is_none());
        assert!(shell(&roots(None, home)).is_none());
        assert_eq!(wiring(&roots(Some("/bin/tcsh"), home)), Wiring::Unsupported);
        assert!(block("fish").contains("function claude\n"));
        assert!(block("fish").contains("$argv\nend\n"));
    }
}
