//! Native command acceptance without a provider or running daemon.
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn standalone_command_deduplicates_copies_and_never_opens_client_state() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first.jsonl");
    let copied = temp.path().join("copy.jsonl");
    let input = include_str!("../../host/src/usage/fixtures/codex-0.160.0.jsonl");
    std::fs::write(&first, input).unwrap();
    std::fs::write(&copied, input).unwrap();
    let blocked = temp.path().join("client home is a file");
    std::fs::write(&blocked, b"leave unchanged").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentdocker"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENTDOCKER_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .arg("usage-scan")
        .arg("--codex")
        .arg(&copied)
        .arg("--codex")
        .arg(&first)
        .env("AGENTDOCKER_HOME", &blocked)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("standalone command exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["rows"][0]["counters"]["input_tokens"]["sum"], 10);
    assert_eq!(report["unique_source_records"], 2);
    assert_eq!(report["scan_complete"], true);
    assert_eq!(report["files"].as_array().unwrap().len(), 2);
    assert!(report["overhead"]["injected_bytes"].is_null());
    assert_eq!(std::fs::read(&blocked).unwrap(), b"leave unchanged");
    assert_eq!(std::fs::read_to_string(&first).unwrap(), input);
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), input);
}
