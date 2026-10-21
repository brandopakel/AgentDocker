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

/// Whether `path` is a native executable by its first bytes: ELF, Mach-O
/// (either word size or byte order, or universal) or a Windows PE image.
fn is_native(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 4];
    if std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_err()
    {
        return false;
    }
    head == *b"\x7fELF"
        || matches!(
            u32::from_be_bytes(head),
            0xfeed_face | 0xfeed_facf | 0xcefa_edfe | 0xcffa_edfe | 0xcafe_babe
        )
        || head[..2] == *b"MZ"
}

/// The npm package an entry point belongs to, when it is Codex's own:
/// `<package>/bin/codex.js` in a package whose manifest names
/// `@openai/codex`. Any other script is not unwrapped.
fn codex_package_of(entry: &Path) -> Option<PathBuf> {
    let bin = entry.parent()?;
    if entry.file_name()? != "codex.js" || bin.file_name()? != "bin" {
        return None;
    }
    let package = bin.parent()?;
    let manifest: serde_json::Value =
        serde_json::from_str(&read_small(&package.join("package.json"))?).ok()?;
    (manifest["name"] == "@openai/codex").then(|| package.to_path_buf())
}

/// The lines npm writes into the shims it makes for a package's `bin`
/// (`cmd-shim`: the `sh` shim, `codex.cmd`, `codex.ps1`), with `@ENTRY@`
/// where the path to the entry point goes, `/` and `\` forms each.
const SHIM_LINES: &[&str] = &[
    // sh
    "#!/bin/sh",
    r#"basedir=$(dirname "$(echo "$0" | sed -e 's,\\,/,g')")"#,
    "case `uname` in",
    "*CYGWIN*|*MINGW*|*MSYS*)",
    "if command -v cygpath > /dev/null 2>&1; then",
    r#"basedir=`cygpath -w "$basedir"`"#,
    "fi",
    ";;",
    "esac",
    r#"if [ -x "$basedir/node" ]; then"#,
    r#"exec "$basedir/node" "$basedir/@ENTRY@" "$@""#,
    "else",
    r#"exec node "$basedir/@ENTRY@" "$@""#,
    // cmd
    "@ECHO off",
    "GOTO start",
    ":find_dp0",
    "SET dp0=%~dp0",
    "EXIT /b",
    ":start",
    "SETLOCAL",
    "CALL :find_dp0",
    r#"IF EXIST "%dp0%\node.exe" ("#,
    r#"SET "_prog=%dp0%\node.exe""#,
    ") ELSE (",
    r#"SET "_prog=node""#,
    "SET PATHEXT=%PATHEXT:;.JS;=;%",
    ")",
    r#"endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%" "%dp0%\@WINENTRY@" %*"#,
    // ps1
    "#!/usr/bin/env pwsh",
    "$basedir=Split-Path $MyInvocation.MyCommand.Definition -Parent",
    r#"$exe="""#,
    r#"if ($PSVersionTable.PSVersion -lt "6.0" -or $IsWindows) {"#,
    r#"$exe=".exe""#,
    "}",
    "$ret=0",
    r#"if (Test-Path "$basedir/node$exe") {"#,
    "if ($MyInvocation.ExpectingInput) {",
    r#"$input | & "$basedir/node$exe" "$basedir/@ENTRY@" $args"#,
    "} else {",
    r#"& "$basedir/node$exe" "$basedir/@ENTRY@" $args"#,
    "$ret=$LASTEXITCODE",
    r#"$input | & "node$exe" "$basedir/@ENTRY@" $args"#,
    r#"& "node$exe" "$basedir/@ENTRY@" $args"#,
    "exit $ret",
];

/// The entry point an npm shim runs, when the shim is one npm generated for
/// Codex beside its package: every line is one of npm's own, with
/// Codex's entry point in it, or a comment. A line of anything else (a
/// `basedir` or `_prog` set elsewhere, another program run) and the shim is
/// not followed, so it is never taken for a package it does not run.
fn shim_entry(shim: &Path) -> Option<PathBuf> {
    const ENTRY: &str = "node_modules/@openai/codex/bin/codex.js";
    let normal = |line: &str| line.split_whitespace().collect::<Vec<_>>().join(" ");
    let known: std::collections::HashSet<String> = SHIM_LINES
        .iter()
        .map(|line| {
            normal(
                &line
                    .replace("@WINENTRY@", &ENTRY.replace('/', "\\"))
                    .replace("@ENTRY@", ENTRY),
            )
        })
        .collect();
    let text = read_small(shim)?;
    let mut runs = 0;
    for line in text.lines().map(normal) {
        let comment = line.starts_with('#') && !line.starts_with("#!");
        if line.is_empty() || comment {
            continue;
        }
        if !known.contains(&line) {
            return None;
        }
        runs += usize::from(line.contains("codex.js"));
    }
    if runs == 0 {
        return None;
    }
    let entry = shim.parent()?.join(ENTRY);
    entry.is_file().then_some(entry)
}

/// A text file small enough to be a shim or a manifest.
fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    (meta.is_file() && meta.len() <= 64 * 1024)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

/// The native binary for `found`, a `codex` from the person's `PATH`: itself
/// when it is a native executable; the one Codex's npm package runs when it
/// is that package's entry point or an npm shim for it. Any other wrapper is
/// refused, whatever lies beside it, and what is found must be native.
pub fn native_codex(found: &Path) -> Option<PathBuf> {
    native_codex_for(found, platform()?)
}

fn native_codex_for(found: &Path, (triple, package): (&str, &str)) -> Option<PathBuf> {
    let found = found.canonicalize().ok()?;
    if !found.is_file() {
        return None;
    }
    // A native file is Codex only by its own name: a version manager's
    // binary shim a `codex` link resolves to is some other program.
    if is_native(&found) {
        let named = found
            .file_name()
            .is_some_and(|name| name == "codex" || name == "codex.exe");
        return named.then_some(found);
    }
    let entry = if codex_package_of(&found).is_some() {
        found
    } else {
        shim_entry(&found)?.canonicalize().ok()?
    };
    let root = codex_package_of(&entry)?;
    let binary = if triple.contains("windows") {
        "codex.exe"
    } else {
        "codex"
    };
    let vendor = |base: &Path| base.join("vendor").join(triple).join("bin").join(binary);
    [
        vendor(&root.join("node_modules").join("@openai").join(package)),
        vendor(&root.parent()?.join(package)),
        vendor(&root),
    ]
    .into_iter()
    .filter_map(|candidate| candidate.canonicalize().ok())
    .find(|candidate| candidate.is_file() && is_native(candidate))
}

/// Why no native Codex was found behind `PATH`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotFound {
    /// No `codex` on `PATH` at all.
    Absent,
    /// The first `codex` on `PATH` is not one this can follow: a wrapper of
    /// its own, or an npm package without its platform binary.
    Unrecognized(PathBuf),
}

/// The native binary behind the first `codex` on `PATH`.
pub fn native_codex_on_path() -> Result<PathBuf, NotFound> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    // The bare name, so `PATH` and `PATHEXT` order it as the shell would.
    let found = crate::command::find_program(&dirs, "codex").ok_or(NotFound::Absent)?;
    native_codex(&found).ok_or(NotFound::Unrecognized(found))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn file(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    const MAC: (&str, &str) = ("aarch64-apple-darwin", "codex-darwin-arm64");
    const WINDOWS: (&str, &str) = ("x86_64-pc-windows-msvc", "codex-win32-x64");
    const MACH_O: &[u8] = b"\xcf\xfa\xed\xfe native";

    /// `<prefix>/lib/node_modules/@openai/codex`, its manifest and entry point.
    fn package(prefix: &Path) -> PathBuf {
        let package = prefix.join("lib/node_modules/@openai/codex");
        file(
            &package.join("package.json"),
            br#"{"name":"@openai/codex"}"#,
        );
        file(&package.join("bin/codex.js"), b"#!/usr/bin/env node\n");
        package
    }

    #[test]
    fn codex_npm_entry_point_leads_to_the_binary_its_package_runs() {
        let root = tempfile::tempdir().unwrap();
        let package = package(root.path());
        let entry = package.join("bin/codex.js");
        let nested = package
            .join("node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex");
        file(&nested, MACH_O);
        assert_eq!(
            native_codex_for(&entry, MAC),
            Some(nested.canonicalize().unwrap())
        );
        std::fs::remove_file(&nested).unwrap();
        let beside = root.path().join(
            "lib/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex",
        );
        file(&beside, MACH_O);
        assert_eq!(
            native_codex_for(&entry, MAC),
            Some(beside.canonicalize().unwrap())
        );
        // A script where the binary should be is not one.
        file(&beside, b"#!/bin/sh\n");
        assert_eq!(native_codex_for(&entry, MAC), None);
    }

    #[test]
    fn a_wrapper_that_is_not_codexs_own_is_refused_whatever_lies_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let wrapper = root.path().join("prefix/bin/codex");
        file(&wrapper, b"#!/bin/sh\nexec /somewhere/else/codex \"$@\"\n");
        file(
            &root
                .path()
                .join("prefix/vendor/aarch64-apple-darwin/bin/codex"),
            MACH_O,
        );
        file(
            &root
                .path()
                .join("prefix/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex"),
            MACH_O,
        );
        assert_eq!(native_codex_for(&wrapper, MAC), None);
        // A `codex.js` of some other package is not unwrapped either.
        let other = root.path().join("other/bin/codex.js");
        file(&other, b"#!/usr/bin/env node\n");
        file(
            &root.path().join("other/package.json"),
            br#"{"name":"not-codex"}"#,
        );
        assert_eq!(native_codex_for(&other, MAC), None);
    }

    #[test]
    fn a_windows_npm_shim_leads_to_its_package() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("npm");
        let package = prefix.join("node_modules/@openai/codex");
        file(
            &package.join("package.json"),
            br#"{"name":"@openai/codex"}"#,
        );
        file(&package.join("bin/codex.js"), b"#!/usr/bin/env node\n");
        let exe = package.join(
            "node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe",
        );
        file(&exe, b"MZ\x90\x00 pe image");
        let shim = prefix.join("codex.cmd");
        file(
            &shim,
b"@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n",
        );
        assert_eq!(
            native_codex_for(&shim, WINDOWS),
            Some(exe.canonicalize().unwrap())
        );
    }

    #[test]
    fn only_a_shim_npm_wrote_for_codex_is_followed() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("npm");
        let package = prefix.join("node_modules/@openai/codex");
        file(
            &package.join("package.json"),
            br#"{"name":"@openai/codex"}"#,
        );
        file(&package.join("bin/codex.js"), b"#!/usr/bin/env node\n");
        let native = package
            .join("node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex");
        file(&native, MACH_O);
        // The sh shim npm writes.
        let generated = prefix.join("codex");
        file(
            &generated,
            b"#!/bin/sh\nbasedir=$(dirname \"$(echo \"$0\" | sed -e 's,\\\\,/,g')\")\n\nif [ -x \"$basedir/node\" ]; then\n  exec \"$basedir/node\"  \"$basedir/node_modules/@openai/codex/bin/codex.js\" \"$@\"\nelse \n  exec node  \"$basedir/node_modules/@openai/codex/bin/codex.js\" \"$@\"\nfi\n",
        );
        assert_eq!(
            native_codex_for(&generated, MAC),
            Some(native.canonicalize().unwrap())
        );
        // A wrapper of the person's own that only mentions it, with the
        // package still beside it, is not followed to that package.
        let custom = prefix.join("codex-pinned");
        file(
            &custom,
            b"#!/bin/sh\n# replaces @openai/codex/bin/codex.js\nexec /opt/pinned/codex \"$@\"\n",
        );
        assert_eq!(native_codex_for(&custom, MAC), None);
        // Nor one that sets its own `basedir` before npm's line.
        let moved = prefix.join("codex-moved");
        file(
            &moved,
            b"#!/bin/sh\nbasedir=/opt/pinned\nexec node \"$basedir/node_modules/@openai/codex/bin/codex.js\" \"$@\"\n",
        );
        assert_eq!(native_codex_for(&moved, MAC), None);
        // Nor one that runs it and something else as well.
        let both = prefix.join("codex-both");
        file(
            &both,
            b"#!/bin/sh\nexec node \"$basedir/node_modules/@openai/codex/bin/codex.js\" \"$@\"\nexec /opt/pinned/codex\n",
        );
        assert_eq!(native_codex_for(&both, MAC), None);
        // A codex.js outside a bin folder is not Codex's entry point.
        let loose = package.join("codex.js");
        file(&loose, b"#!/usr/bin/env node\n");
        assert_eq!(native_codex_for(&loose, MAC), None);
    }

    #[test]
    fn a_native_binary_on_path_is_its_own_codex() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("bin/codex");
        file(&binary, MACH_O);
        assert_eq!(
            native_codex_for(&binary, MAC),
            Some(binary.canonicalize().unwrap())
        );
        assert_eq!(native_codex_for(&root.path().join("missing"), MAC), None);
        // A link named codex to some other native program is not Codex.
        let manager = root.path().join("tools/mise");
        file(&manager, MACH_O);
        let link = root.path().join("shims/codex");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&manager, &link).unwrap();
        assert_eq!(native_codex_for(&link, MAC), None);
    }
}

/// With `AGENTDOCKER_TEST_INSTALLED_CODEX=1`, the installed Codex's `codex`
/// on `PATH` leads to a native binary. Opt-in: a machine whose `codex` is a
/// wrapper of its own is refused by design, not a failure of the suite.
#[cfg(all(test, unix))]
#[test]
fn the_installed_codex_leads_to_its_binary() {
    if std::env::var_os("AGENTDOCKER_TEST_INSTALLED_CODEX").is_none() {
        return;
    }
    let native = native_codex_on_path().expect("the installed codex has its binary");
    assert_eq!(native.file_name().unwrap(), "codex");
    assert!(
        native.is_file() && is_native(&native),
        "{}",
        native.display()
    );
}
