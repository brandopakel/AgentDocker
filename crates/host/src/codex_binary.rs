//! The native Codex executable behind the `codex` a person types. The npm
//! package's `codex` is a Node script that finds and runs a platform
//! binary; AgentDocker's native launcher needs that binary itself, so it
//! is found here the way the script finds it: the platform package nested
//! in the package, or beside it, else the package's own `vendor`.

use std::path::{Path, PathBuf};

/// The platform's target triple and the npm package that carries its
/// binary, as Codex's own launcher names them.
fn platform() -> Option<(&'static str, &'static str)> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("aarch64-apple-darwin", "codex-darwin-arm64"),
        ("macos", "x86_64") => ("x86_64-apple-darwin", "codex-darwin-x64"),
        ("linux", "x86_64") => ("x86_64-unknown-linux-musl", "codex-linux-x64"),
        ("linux", "aarch64") => ("aarch64-unknown-linux-musl", "codex-linux-arm64"),
        ("windows", "x86_64") => ("x86_64-pc-windows-msvc", "codex-win32-x64"),
        ("windows", "aarch64") => ("aarch64-pc-windows-msvc", "codex-win32-arm64"),
        _ => return None,
    })
}

/// Whether `path` starts as a script does (`#!`), rather than as a binary.
fn is_script(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok_and(|()| head == *b"#!")
}

/// The native binary for `found`, a `codex` from the person's `PATH`: itself
/// when it is a binary already; the one its npm package runs when it is the
/// package's script. `None` when neither can be found.
pub fn native_codex(found: &Path) -> Option<PathBuf> {
    native_codex_for(found, platform()?)
}

fn native_codex_for(found: &Path, (triple, package): (&str, &str)) -> Option<PathBuf> {
    let found = found.canonicalize().ok()?;
    if !found.is_file() {
        return None;
    }
    let script = found
        .extension()
        .is_some_and(|ext| ext == "js" || ext == "cmd" || ext == "ps1")
        || is_script(&found);
    if !script {
        return Some(found);
    }
    // `<package>/bin/codex.js`: the package is the script's grandparent.
    let root = found.parent()?.parent()?;
    let binary = if triple.contains("windows") {
        "codex.exe"
    } else {
        "codex"
    };
    let vendor = |base: &Path| base.join("vendor").join(triple).join("bin").join(binary);
    [
        vendor(&root.join("node_modules").join("@openai").join(package)),
        vendor(&root.parent()?.join(package)),
        vendor(root),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
    .and_then(|candidate| candidate.canonicalize().ok())
}

/// The native binary behind the first `codex` on `PATH`.
pub fn native_codex_on_path() -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let name = if cfg!(windows) { "codex.cmd" } else { "codex" };
    let found = crate::command::find_program(&dirs, name)
        .or_else(|| crate::command::find_program(&dirs, "codex"))?;
    native_codex(&found)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn file(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    const MAC: (&str, &str) = ("aarch64-apple-darwin", "codex-darwin-arm64");

    #[test]
    fn the_npm_script_leads_to_the_binary_its_package_runs() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("lib/node_modules/@openai/codex");
        file(&package.join("bin/codex.js"), "#!/usr/bin/env node\n");
        // Nested, as npm installs it.
        let nested = package
            .join("node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex");
        file(&nested, "\u{7f}ELF");
        assert_eq!(
            native_codex_for(&package.join("bin/codex.js"), MAC),
            Some(nested.canonicalize().unwrap())
        );
        // Beside it, as a hoisting installer leaves it.
        std::fs::remove_file(&nested).unwrap();
        let beside = root.path().join(
            "lib/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex",
        );
        file(&beside, "\u{7f}ELF");
        assert_eq!(
            native_codex_for(&package.join("bin/codex.js"), MAC),
            Some(beside.canonicalize().unwrap())
        );
        // None at all: no binary, never the script.
        std::fs::remove_file(&beside).unwrap();
        assert_eq!(native_codex_for(&package.join("bin/codex.js"), MAC), None);
    }

    #[test]
    fn a_binary_on_path_is_its_own_native_codex() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("bin/codex");
        file(&binary, "\u{7f}ELF native");
        assert_eq!(
            native_codex_for(&binary, MAC),
            Some(binary.canonicalize().unwrap())
        );
        let script = root.path().join("other/codex");
        file(&script, "#!/bin/sh\nexec node x\n");
        assert_eq!(
            native_codex_for(&script, MAC),
            None,
            "a shell wrapper is not the binary"
        );
        assert_eq!(native_codex_for(&root.path().join("missing"), MAC), None);
    }
}

/// Where Codex is installed, the `codex` on `PATH` leads to a binary, never
/// to a script. Nothing to check where Codex is not installed.
#[cfg(all(test, unix))]
#[test]
fn the_installed_codex_leads_to_its_binary() {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if crate::command::find_program(&dirs, "codex").is_none() {
        return;
    }
    let native = native_codex_on_path().expect("an installed codex has its binary");
    assert!(native.is_file());
    assert!(!is_script(&native), "{}", native.display());
}
