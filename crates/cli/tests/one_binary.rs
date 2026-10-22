//! One executable, two programs: the `agentdocker` file invoked as `agentd`
//! is the daemon, invoked by any other name the CLI, and
//! `agentdocker daemon run` is the daemon under the CLI's own name.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli() -> &'static str {
    env!("CARGO_BIN_EXE_agentdocker")
}

/// The daemon's name for this executable, as packages ship it: a hard link,
/// made in Cargo's own target directory so it is on the executable's
/// filesystem; a copy where that filesystem cannot link.
fn daemon_link(dir: &Path) -> PathBuf {
    let link = dir.join(format!("agentd{}", std::env::consts::EXE_SUFFIX));
    if std::fs::hard_link(cli(), &link).is_err() {
        std::fs::copy(cli(), &link).unwrap();
    }
    link
}

fn run(command: &mut Command) -> Output {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENTDOCKER_") {
            command.env_remove(key);
        }
    }
    command.output().unwrap()
}

fn stdout(command: &mut Command) -> String {
    let output = run(command);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn the_name_it_is_invoked_by_selects_the_program() {
    let dir = tempfile::Builder::new()
        .prefix("one-binary-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let agentd = daemon_link(dir.path());
    let version = env!("CARGO_PKG_VERSION");

    assert_eq!(
        stdout(Command::new(&agentd).arg("--version")).trim(),
        format!("agentd {version}")
    );
    assert_eq!(
        stdout(Command::new(cli()).arg("--version")).trim(),
        format!("agentdocker {version}")
    );

    // The daemon's own option, which opens no state: the CLI has none such.
    let home = dir.path().join("absent-state");
    let socket = dir.path().join("absent.sock");
    let build_info = |command: &mut Command| -> serde_json::Value {
        let text = stdout(
            command
                .arg("--build-info")
                .arg("--home")
                .arg(&home)
                .arg("--socket")
                .arg(&socket),
        );
        serde_json::from_str(&text).unwrap()
    };
    let info = build_info(&mut Command::new(&agentd));
    assert_eq!(info["format"], 1);
    assert_eq!(info["version"], version);
    assert!(
        info["state_schema"]
            .as_u64()
            .is_some_and(|schema| schema > 0)
    );
    assert!(!home.exists(), "build info opened no state");
    assert!(
        !run(Command::new(cli()).arg("--build-info"))
            .status
            .success()
    );

    // `daemon run` is the same daemon under the CLI's name, its usage text
    // saying so.
    assert_eq!(
        build_info(Command::new(cli()).args(["daemon", "run"])),
        info
    );
    let help = stdout(Command::new(cli()).args(["daemon", "run", "--help"]));
    assert!(help.contains("Usage: agentdocker daemon run"), "{help}");
    assert!(help.contains("--home"), "{help}");
    // The CLI's own options are not the daemon's: they go after `run`.
    let refused = run(Command::new(cli()).args(["--socket", "elsewhere", "daemon", "run"]));
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("the daemon's own options"),
        "{refused:?}"
    );
}

/// The name decides, not the file: the same file runs the CLI when invoked
/// as `agentdocker` through the daemon's link, and the daemon when the CLI's
/// file is invoked as `agentd`.
#[cfg(unix)]
#[test]
fn argv0_alone_decides_whatever_the_file_is_called() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::Builder::new()
        .prefix("one-binary-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let agentd = daemon_link(dir.path());
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        stdout(Command::new(cli()).arg0("agentd").arg("--version")).trim(),
        format!("agentd {version}")
    );
    assert_eq!(
        stdout(
            Command::new(&agentd)
                .arg0("/somewhere/agentdocker")
                .arg("--version")
        )
        .trim(),
        format!("agentdocker {version}")
    );
    // A name that only resembles the daemon's is the CLI.
    assert_eq!(
        stdout(Command::new(cli()).arg0("agentd-old").arg("--version")).trim(),
        format!("agentdocker {version}")
    );
}
