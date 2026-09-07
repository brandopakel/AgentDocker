//! Real daemon/CLI restart acceptance. Every command and endpoint belongs to
//! the fixture; no installed daemon, provider account or user config is used.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
    fn start(home: &Path, socket: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_agentd"))
            .arg("--home")
            .arg(home)
            .arg("--socket")
            .arg(socket)
            .env("RUST_LOG", "off")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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
            if rpc(socket, json!({"op":"ping"})).is_ok_and(|response| response["type"] == "pong") {
                return running;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&mut self) {
        let _ = rpc(&self.socket, json!({"op":"shutdown"}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                self.child.kill().unwrap();
                self.child.wait().unwrap();
                panic!("fixture daemon failed to shut down cleanly");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = rpc(&self.socket, json!({"op":"shutdown"}));
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn rpc(socket: &Path, request: Value) -> std::io::Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    writeln!(stream, "{request}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

#[test]
fn restored_first_instruction_can_coordinate_and_its_first_edit_is_observed() {
    let tmp = tempfile::Builder::new()
        .prefix("ad-restore-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("state");
    let socket = root.join("host.sock");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("Agentfile.toml"), "").unwrap();
    std::fs::write(work.join("first-edit"), "initial").unwrap();
    let mut daemon = RunningDaemon::start(&home, &socket);
    let response = rpc(&socket, json!({"op":"run", "spec": {
        "name":"first-instruction", "workdir":work, "restore":true,
        "command":["sh", "-c",
          "\"$1\" inspect \"$AGENTDOCKER_AGENT_ID\" >/dev/null || exit 81; printf coordinated > first-edit; exec sleep 30",
          "fixture", env!("CARGO_BIN_EXE_agentdocker")]
    }})).unwrap();
    assert_eq!(response["type"], "agent", "{response}");
    let id = response["agent"]["id"].as_str().unwrap();
    let old_pid = response["agent"]["pid"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(work.join("first-edit")).unwrap() != "coordinated" {
        assert!(
            Instant::now() < deadline,
            "initial fixture did not coordinate"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let claimed = rpc(
        &socket,
        json!({"op":"claim", "agent":id,
        "resource": format!("path:{}", work.join("first-edit").display()), "ttl_secs":300}),
    )
    .unwrap();
    assert_eq!(claimed["type"], "lease", "{claimed}");
    daemon.stop();
    std::fs::write(work.join("first-edit"), "while-down").unwrap();
    // Legacy modes are migrated when the same installation is reopened.
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(
        home.join("state.db"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    daemon = RunningDaemon::start(&home, &socket);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let changes = rpc(&socket, json!({"op":"changes", "project":work, "agent":id})).unwrap();
        let observed = changes["changes"].as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("first-edit"))
            })
        });
        let content = std::fs::read_to_string(work.join("first-edit")).unwrap();
        if observed && content == "coordinated" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "restored first edit missing: {changes}; content={content}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    let restored = rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap();
    assert_ne!(restored["agent"]["pid"].as_u64(), Some(old_pid));
    for directory in [&home, &home.join("logs")] {
        assert_eq!(std::fs::metadata(directory).unwrap().mode() & 0o777, 0o700);
    }
    for file in [
        home.join("state.db"),
        home.join("state.db-wal"),
        home.join("state.db-shm"),
        home.join("logs").join(format!("{id}.log")),
    ] {
        assert_eq!(
            std::fs::metadata(&file).unwrap().mode() & 0o777,
            0o600,
            "{}",
            file.display()
        );
    }
    daemon.stop();
}

#[test]
fn service_install_preview_does_not_create_state_or_write_service_files() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let home = tmp.path().join("not-created");
    let result = Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(["daemon", "install", "--dry-run"])
        .env("AGENTDOCKER_HOME", &home)
        .env("AGENTDOCKER_SOCKET", tmp.path().join("host.sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !home.exists(),
        "a preview must leave the state directory absent"
    );
}
