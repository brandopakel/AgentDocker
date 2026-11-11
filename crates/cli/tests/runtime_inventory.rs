//! `runtimes` from a real daemon, built as it ships: the inventory reads
//! the machine the daemon was started on — its `HOME` and `PATH` — on each
//! request, and a browser whose user data the system will not let it list
//! is a note, never a failed request. The machine is the fixture's: a home
//! of its own with a closed browser directory, and a `PATH` led by one
//! stand-in CLI. No installed daemon, real home or provider is used.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct RunningDaemon {
    child: Child,
    socket: PathBuf,
}

impl RunningDaemon {
    /// The daemon with `machine` as its home and `bin` first on its `PATH`,
    /// ahead of only the system's own directories (`ps` is there), and
    /// nothing else of the surrounding environment's that the inventory
    /// reads.
    fn start(home: &Path, socket: &Path, machine: &Path, bin: &Path) -> Self {
        let log =
            agentdocker_host::dirs::private_file(&home.with_extension("daemon.log"), true, true)
                .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
            .args(["daemon", "run"])
            .arg("--home")
            .arg(home)
            .arg("--socket")
            .arg(socket)
            .env_clear()
            .env("HOME", machine)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("AGENTDOCKER_HOME", home)
            .env("AGENTDOCKER_SOCKET", socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut running = Self {
            child,
            socket: socket.to_path_buf(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                running.child.try_wait().unwrap().is_none(),
                "daemon exited at startup"
            );
            if rpc(socket, json!({"op": "ping"})).is_ok_and(|response| response["type"] == "pong") {
                return running;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let _ = rpc(&self.socket, json!({"op": "shutdown"}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn rpc(socket: &Path, request: Value) -> std::io::Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// Gives a closed directory back as the test ends, so the temp dir can go.
struct Reopen(PathBuf);

impl Drop for Reopen {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn runtimes_reads_the_daemons_own_machine_and_notes_a_closed_browser() {
    // Short enough for a Unix socket path.
    let tmp = tempfile::Builder::new()
        .prefix("ad-runtimes-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = tmp.path().join("home");
    let socket = tmp.path().join("d.sock");

    // The machine: one CLI on its PATH, which says its version when asked.
    let machine = tmp.path().join("machine");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\necho '9.9.9 (Claude Code)'\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

    // And Chrome's user data where this platform keeps it, closed.
    let chrome = if cfg!(target_os = "macos") {
        machine.join("Library/Application Support/Google/Chrome")
    } else {
        machine.join(".config/google-chrome")
    };
    std::fs::create_dir_all(chrome.join("Default/Extensions")).unwrap();
    std::fs::set_permissions(&chrome, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _reopen = Reopen(chrome.clone());
    let closed = std::fs::read_dir(&chrome).is_err();

    let daemon = RunningDaemon::start(&home, &socket, &machine, &bin);
    let answer = rpc(&daemon.socket, json!({"op": "runtimes"})).unwrap();
    assert_eq!(answer["type"], "runtimes", "{answer}");
    let runtime = |name: &str| {
        answer["runtimes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed: {answer}"))
            .clone()
    };

    // The machine's own PATH, and its CLI asked for its version: not a
    // bare machine a test build would have used.
    let code = runtime("claude-code");
    assert_eq!(code["cli"], json!(claude), "{code}");
    assert_eq!(code["version"], "9.9.9 (Claude Code)", "{code}");

    // The machine's own home: its closed browser is a note, and the
    // request is answered all the same.
    if closed {
        let browser = runtime("claude-browser");
        let notes = browser["incomplete"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            notes.iter().any(|note| note
                .as_str()
                .is_some_and(|note| note.starts_with("Chrome profiles not examined: ")
                    && note.contains(&chrome.display().to_string()))),
            "{browser}"
        );
    }
}
