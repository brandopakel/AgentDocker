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
        let log =
            agentdocker_host::dirs::private_file(&home.with_extension("daemon.log"), true, true)
                .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_agentd"))
            .arg("--home")
            .arg(home)
            .arg("--socket")
            .arg(socket)
            .env("AGENTDOCKER_HOME", home)
            .env("AGENTDOCKER_SOCKET", socket)
            .env("AGENTDOCKER_NO_AUTOSTART", "1")
            .env_remove("AGENTDOCKER_TOKEN_FILE")
            .env_remove("AGENTDOCKER_AGENT_ID")
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
fn build_info_reports_compiled_contract_without_opening_state() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("absent-state");
    let socket = tmp.path().join("absent.sock");
    // The bounded host runner kills a regression that accidentally starts a daemon.
    let output = agentdocker_host::command::run(
        tmp.path(),
        &[
            env!("CARGO_BIN_EXE_agentd").to_owned(),
            "--build-info".into(),
            "--home".into(),
            home.display().to_string(),
            "--socket".into(),
            socket.display().to_string(),
        ],
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(output.success, "{}", output.text);
    let value: Value = serde_json::from_str(&output.text).unwrap();
    assert_eq!(value["format"], 1);
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["os"], std::env::consts::OS);
    assert_eq!(value["arch"], std::env::consts::ARCH);
    assert!(
        value["state_schema"]
            .as_u64()
            .is_some_and(|schema| (1..=u32::MAX as u64).contains(&schema))
    );
    assert!(!home.exists());
    assert!(!socket.exists());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn refused_reload_preserves_real_batch_and_terminal_processes_and_logs() {
    let tmp = tempfile::Builder::new()
        .prefix("ad-reload-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("state");
    let socket = root.join("host.sock");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    let mut daemon = RunningDaemon::start(&home, &socket);
    let mut agents = Vec::new();
    for tty in [false, true] {
        let name = if tty { "terminal" } else { "batch" };
        let script = format!(
            "printf '{name}-before\\n'; while ! test -f {name}-go; do sleep 0.05; done; \
             printf '{name}-after\\n'; printf survived > {name}-survived; exec sleep 30"
        );
        let response = rpc(
            &socket,
            json!({"op":"run", "spec": {
                "name":name, "workdir":work, "tty":tty, "command":["sh", "-c", script]
            }}),
        )
        .unwrap();
        assert_eq!(response["type"], "agent", "{response}");
        let id = response["agent"]["id"].as_str().unwrap().to_owned();
        let log = home.join("logs").join(format!("{id}.log"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_to_string(&log)
            .is_ok_and(|text| text.contains(&format!("{name}-before")))
        {
            assert!(Instant::now() < deadline, "fixture never produced output");
            std::thread::sleep(Duration::from_millis(20));
        }
        agents.push((name, id, response["agent"]["pid"].clone(), log));
    }
    let response = rpc(&socket, json!({"op":"reload"})).unwrap();
    assert_eq!(response["type"], "error", "{response}");
    assert_eq!(response["code"], "unavailable", "{response}");
    assert!(daemon.child.try_wait().unwrap().is_none());
    for (name, id, pid, log) in agents {
        std::fs::write(work.join(format!("{name}-go")), b"").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !work.join(format!("{name}-survived")).exists()
            || !std::fs::read_to_string(&log)
                .is_ok_and(|text| text.contains(&format!("{name}-after")))
        {
            assert!(
                Instant::now() < deadline,
                "{name} lost execution or logging"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let inspected = rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap();
        assert_eq!(inspected["agent"]["pid"], pid);
        assert_eq!(inspected["agent"]["status"]["state"], "running");
    }
    daemon.stop();
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
        if Instant::now() >= deadline {
            let record = rpc(&socket, json!({"op":"inspect", "agent":id}));
            let log = std::fs::read_to_string(home.join("logs").join(format!("{id}.log")));
            let retained = tmp.keep();
            panic!(
                "initial fixture did not coordinate; record={record:?}; log={log:?}; retained={retained:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let claimed = rpc(
        &socket,
        json!({"op":"claim", "agent":id,
        "resource": format!("path:{}", work.join("first-edit").display()), "ttl_secs":300}),
    )
    .unwrap();
    assert_eq!(claimed["type"], "lease", "{claimed}");
    // Wait for the initial write to reach the ledger, so an old/debounced
    // observation cannot satisfy the restoration assertion after restart.
    let deadline = Instant::now() + Duration::from_secs(5);
    let before_restart = loop {
        let changes = rpc(&socket, json!({"op":"changes", "project":work})).unwrap();
        let entries = changes["changes"].as_array().unwrap();
        if entries.iter().any(|entry| {
            entry["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("first-edit"))
        }) {
            break entries
                .iter()
                .filter_map(|entry| entry["seq"].as_u64())
                .max()
                .unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "initial edit did not reach the ledger: {changes}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
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
        let changes = rpc(
            &socket,
            json!({"op":"changes", "project":work, "agent":id, "since_seq":before_restart}),
        )
        .unwrap();
        let observed = changes["changes"].as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["seq"]
                    .as_u64()
                    .is_some_and(|seq| seq > before_restart)
                    && entry["path"]
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

#[test]
fn native_launch_and_restore_use_the_owning_daemon_context() {
    let temporary = tempfile::Builder::new()
        .prefix("ad-context-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let home = root.join("owning-state");
    let socket = root.join("owning.sock");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    let mut daemon = RunningDaemon::start(&home, &socket);
    let response = rpc(&socket, json!({"op":"run", "spec": {
        "name":"context-fixture", "workdir":work, "restore":true,
        "env": {"AGENTDOCKER_HOME":root.join("unrelated-state"),
            "AGENTDOCKER_SOCKET":root.join("unrelated.sock"),
            "AGENTDOCKER_TOKEN_FILE":root.join("unrelated-token"),
            "AGENTDOCKER_NO_AUTOSTART":"0"},
        "command":["sh", "-c",
            "test \"$AGENTDOCKER_HOME\" = \"$2\" || exit 71; test \"$AGENTDOCKER_SOCKET\" = \"$3\" || exit 72; test \"${AGENTDOCKER_TOKEN_FILE+x}\" != x || exit 73; test \"$AGENTDOCKER_NO_AUTOSTART\" = 1 || exit 74; \"$1\" inspect \"$AGENTDOCKER_AGENT_ID\" >/dev/null || exit 75; printf passed > context; exec sleep 30",
            "fixture", env!("CARGO_BIN_EXE_agentdocker"),home,socket]
    }})).unwrap();
    assert_eq!(response["type"], "agent", "{response}");
    let id = response["agent"]["id"].as_str().unwrap();
    let first_pid = response["agent"]["pid"].as_u64().unwrap();
    for restored in [false, true] {
        if restored {
            daemon.stop();
            std::fs::remove_file(work.join("context")).unwrap();
            daemon = RunningDaemon::start(&home, &socket);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_to_string(work.join("context")).is_ok_and(|value| value == "passed") {
            assert!(
                Instant::now() < deadline,
                "native context failed (restored={restored}): {}",
                rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        if restored {
            assert_ne!(
                rpc(&socket, json!({"op":"inspect", "agent":id})).unwrap()["agent"]["pid"].as_u64(),
                Some(first_pid)
            );
        }
    }
    assert!(!root.join("unrelated-state").exists());
    assert!(!root.join("unrelated.sock").exists());
    daemon.stop();
}
