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

/// A daemon this test owns, stopped by `shutdown` and then by force.
#[cfg(unix)]
struct Fixture {
    daemon: std::process::Child,
    socket: PathBuf,
}

#[cfg(unix)]
impl Fixture {
    fn start(mut command: Command, home: &Path, socket: &Path, env: &[(&str, &Path)]) -> Self {
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENTDOCKER_") {
                command.env_remove(key);
            }
        }
        let log = std::fs::File::create(home.with_extension("log")).unwrap();
        let daemon = command
            .arg("--home")
            .arg(home)
            .arg("--socket")
            .arg(socket)
            // The harness acts as the person from wherever it runs.
            .env("AGENTDOCKER_PERSON_CHECK", "off")
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env("RUST_LOG", "warn")
            .envs(env.iter().copied())
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut fixture = Self {
            daemon,
            socket: socket.to_owned(),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while rpc(socket, serde_json::json!({"op": "ping"})).is_err() {
            assert!(
                fixture.daemon.try_wait().unwrap().is_none(),
                "the daemon exited at startup: {}",
                std::fs::read_to_string(home.with_extension("log")).unwrap_or_default()
            );
            assert!(
                std::time::Instant::now() < deadline,
                "the daemon never answered"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        fixture
    }
}

#[cfg(unix)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = rpc(&self.socket, serde_json::json!({"op": "shutdown"}));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.daemon.try_wait().ok().flatten().is_none()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

#[cfg(unix)]
fn rpc(socket: &Path, request: serde_json::Value) -> std::io::Result<serde_json::Value> {
    use std::io::{BufRead, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    // Longer than a session owner may take to answer, so a broken start
    // comes back as the daemon's refusal rather than a timeout here.
    stream.set_read_timeout(Some(std::time::Duration::from_secs(20)))?;
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// The arguments of a running process, as `ps` reports them.
#[cfg(unix)]
fn arguments_of(pid: u64) -> String {
    let output = Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A renamed copy of this executable: no `agentd` or `agentdocker` beside
/// it, a name that runs the CLI.
#[cfg(unix)]
fn renamed_copy(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("one-binary-renamed-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let copy = dir.path().join(name);
    std::fs::copy(cli(), &copy).unwrap();
    (dir, copy)
}

/// The daemon running from this executable under another name, whether it
/// was chosen by `daemon run` or by an `argv[0]` of `agentd`, starts its
/// session owners as the daemon: `<file> daemon run --session-owner`, since
/// by that file's name alone it would be the CLI.
#[cfg(unix)]
#[test]
fn a_renamed_daemon_starts_its_session_owners_as_the_daemon() {
    use std::os::unix::process::CommandExt;
    let (_copy, renamed) = renamed_copy("renamed");
    for chosen_by in ["daemon run", "argv0"] {
        let tmp = tempfile::Builder::new()
            .prefix("ad-renamed-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (home, socket, work) = (root.join("state"), root.join("d.sock"), root.join("work"));
        std::fs::create_dir(&work).unwrap();
        let mut command = Command::new(&renamed);
        if chosen_by == "daemon run" {
            command.args(["daemon", "run"]);
        } else {
            command.arg0("agentd");
        }
        let fixture = Fixture::start(command, &home, &socket, &[]);
        let response = rpc(
            &fixture.socket,
            serde_json::json!({"op": "run", "spec": {
                "name": "probe", "workdir": work,
                "command": ["sh", "-c", "printf ran > ran; exec sleep 30"]
            }}),
        )
        .unwrap();
        assert_eq!(response["type"], "agent", "{chosen_by}: {response}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !work.join("ran").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "{chosen_by}: the agent never ran"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The agent's parent is its session owner: the renamed file, as the
        // daemon.
        let pid = response["agent"]["pid"].as_u64().unwrap();
        let parent = Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let owner: u64 = String::from_utf8_lossy(&parent.stdout)
            .trim()
            .parse()
            .unwrap();
        let owner = arguments_of(owner);
        assert!(
            owner.ends_with("/renamed daemon run --session-owner"),
            "{chosen_by}: {owner}"
        );
    }
}

/// A reload candidate that is this executable renamed answers
/// `--build-info` only as `daemon run`, and its successor is started that
/// way and serves.
#[cfg(unix)]
#[test]
fn a_renamed_reload_candidate_is_started_as_the_daemon() {
    let (_copy, candidate) = renamed_copy("candidate");
    let tmp = tempfile::Builder::new()
        .prefix("ad-candidate-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (home, socket) = (root.join("state"), root.join("d.sock"));
    let mut command = Command::new(cli());
    command.args(["daemon", "run"]);
    let mut fixture = Fixture::start(
        command,
        &home,
        &socket,
        &[
            ("AGENTDOCKER_EXPERIMENTAL_RELOAD", Path::new("1")),
            ("AGENTDOCKER_RELOAD_CANDIDATE", &candidate),
        ],
    );
    let before = fixture.daemon.id();
    let response = rpc(&socket, serde_json::json!({"op": "reload"})).unwrap();
    assert_eq!(response["type"], "ok", "{response}");
    // The predecessor leaves once the successor serves.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while fixture.daemon.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the predecessor did not leave"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let pong = rpc(&socket, serde_json::json!({"op": "ping"})).unwrap();
    let successor = pong["pid"].as_u64().unwrap();
    assert_ne!(successor, u64::from(before));
    let started = arguments_of(successor);
    assert!(
        started.ends_with("/candidate daemon run --take-over 3"),
        "{started}"
    );
    // The successor is not this test's child: stop it, and wait for it.
    rpc(&socket, serde_json::json!({"op": "shutdown"})).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    // SAFETY: signal zero delivers nothing; it only asks whether the pid
    // still names a process.
    while unsafe { libc::kill(successor as libc::pid_t, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the successor did not stop"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
