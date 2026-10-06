//! Exercise the real CLI's reconnect request without starting a provider or
//! borrowing any account/profile. The private daemon records the exact launch.
#![cfg(unix)]

use agentdocker_core::{AgentRecord, AgentSpec, AgentStatus};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn record(root: &Path) -> AgentRecord {
    let mut spec = AgentSpec {
        name: "restricted-claude".into(),
        runtime: "claude-code".into(),
        command: [
            "recorded-claude",
            "--strict-mcp-config",
            "--permission-mode",
            "dontAsk",
            "--tools",
            "",
            "--allowed-tools=Read",
            "--session-id",
            "old-selector",
            "--",
            "original private prompt",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        env: std::collections::BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".into(),
            root.join("profile").to_string_lossy().into_owned(),
        )]),
        workdir: Some(root.into()),
        tty: true,
        ..Default::default()
    };
    spec.labels
        .insert("session_id".into(), "bound-conversation".into());
    agentdocker_host::provider_input::enable_claude_channel(
        &mut spec,
        Path::new(env!("CARGO_BIN_EXE_agentdocker")),
    )
    .unwrap();
    let mut record = AgentRecord::new(spec, true, chrono::Utc::now());
    record.status = AgentStatus::Exited { code: Some(0) };
    record
}

/// Bound both accept and reads, join the worker, and retain complete requests.
fn serve(socket: &Path, record: AgentRecord, count: usize) -> std::thread::JoinHandle<Vec<Value>> {
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..count {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "reconnect did not contact its private daemon"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut input = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut input)
                .unwrap();
            requests.push(serde_json::from_str(&input).unwrap());
            writeln!(stream, "{}", json!({"type":"agent","agent":record})).unwrap();
        }
        requests
    })
}

fn cli(root: &Path, socket: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agentdocker"))
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("AGENTDOCKER_HOME", root.join("state"))
        .env("AGENTDOCKER_SOCKET", socket)
        .env("AGENTDOCKER_NO_AUTOSTART", "1")
        .output()
        .unwrap()
}

#[test]
fn reconnect_cli_preserves_recorded_options_and_environment_with_explicit_executable_override() {
    for override_program in [false, true] {
        let root = tempfile::Builder::new()
            .prefix("ad-reconnect-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("d.sock");
        let record = record(root.path());
        let worker = serve(&socket, record.clone(), 2);
        let id = record.id.to_string();
        let mut arguments = vec!["reconnect", id.as_str()];
        if override_program {
            arguments.extend(["--claude", "chosen-claude"]);
        }
        let output = cli(root.path(), &socket, &arguments);
        let requests = worker.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), id);
        assert_eq!(requests[0]["op"], "inspect");
        assert_eq!(requests[1]["op"], "resume_session");
        assert_eq!(requests[1]["agent"], id);
        let launch = &requests[1]["spec"];
        assert_eq!(
            launch["env"],
            serde_json::to_value(&record.spec.env).unwrap()
        );
        assert_eq!(
            launch["command"][0],
            if override_program {
                "chosen-claude"
            } else {
                "recorded-claude"
            }
        );
        assert_eq!(
            &launch["command"].as_array().unwrap()[5..],
            json!([
                "--strict-mcp-config",
                "--permission-mode",
                "dontAsk",
                "--tools",
                "",
                "--allowed-tools=Read",
                "--resume",
                "bound-conversation"
            ])
            .as_array()
            .unwrap()
        );
        assert!(!launch.to_string().contains("original private prompt"));
        assert!(!launch.to_string().contains("old-selector"));
        assert_eq!(
            launch["workdir"],
            serde_json::to_value(root.path()).unwrap()
        );
    }
}

#[test]
fn ambiguous_recorded_options_are_refused_before_a_resume_request() {
    let root = tempfile::Builder::new()
        .prefix("ad-reconnect-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = root.path().join("d.sock");
    let mut record = record(root.path());
    record.spec.command = ["claude", "--unknown-private-option", "private-value"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let worker = serve(&socket, record.clone(), 1);
    let output = cli(root.path(), &socket, &["reconnect", record.id.as_str()]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("unsupported option is recorded"), "{error}");
    assert!(!error.contains("private-value"));
    assert!(!error.contains("unknown-private-option"));
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["op"], "inspect");
}
