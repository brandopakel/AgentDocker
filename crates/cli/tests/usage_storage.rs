//! Exercise the offline command on every native platform, including Windows.
//! The fixture contains synthetic counters; no provider or daemon is started.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::Value;

struct Fixture {
    // Close SQLite before removing its directory, including on Windows.
    conn: Connection,
    home: PathBuf,
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        agentd::initialize_storage_platform().unwrap();
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("selected state");
        agentdocker_host::dirs::ensure_private_dir(&home).unwrap();
        let path = home.join("state.db");
        drop(agentdocker_host::dirs::create_private_file(&path).unwrap());
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE usage_tracking(singleton INTEGER PRIMARY KEY,bytes INTEGER,capacity INTEGER);
            CREATE TABLE usage_example(key TEXT PRIMARY KEY,payload BLOB);
            CREATE INDEX usage_example_payload ON usage_example(payload);
            INSERT INTO usage_tracking VALUES (1,12345,268435456);
            INSERT INTO usage_example VALUES ('synthetic',zeroblob(100000));").unwrap();
        conn.execute(
            "INSERT INTO meta VALUES ('schema_version',?1)",
            [agentd::STATE_SCHEMA_VERSION.to_string()],
        )
        .unwrap();
        Self { root, home, conn }
    }

    fn content(&self) -> (Vec<u8>, Vec<u8>) {
        (
            std::fs::read(self.home.join("state.db")).unwrap(),
            std::fs::read(self.home.join("state.db-wal")).unwrap(),
        )
    }

    fn run(&self, home: &Path) -> Output {
        let unused = self.root.path().join("unusable client home");
        if !unused.exists() {
            std::fs::write(&unused, b"not a state directory").unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentdocker"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENTDOCKER_") {
                command.env_remove(key);
            }
        }
        // Autostart remains enabled, but a regular file prevents a mistaken
        // client route from starting a fixture daemon or leaving one behind.
        let mut child = command
            .arg("usage-storage")
            .arg("--home")
            .arg(home)
            .env("AGENTDOCKER_HOME", &unused)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("offline storage command exceeded its deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(std::fs::read(&unused).unwrap(), b"not a state directory");
        output
    }
}

#[test]
fn command_reads_explicit_live_wal_without_changing_database_content() {
    let fixture = Fixture::new();
    let before = fixture.content();
    let output = fixture.run(&fixture.home);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["logical_tracking_bytes"], 12345);
    assert_eq!(report["accounting_table_count"], 2);
    assert_eq!(report["accounting_index_count"], 2);
    assert!(report["accounting_table_page_bytes"].as_u64().unwrap() >= 100000);
    assert!(report["accounting_index_page_bytes"].as_u64().unwrap() >= 100000);
    assert!(report["files_before"]["wal_bytes"].as_u64().unwrap() > 0);
    assert!(
        before == fixture.content(),
        "read-only inspection changed database or WAL bytes"
    );
}

#[test]
fn command_refuses_missing_future_negative_or_excessive_state_without_partial_json() {
    let fixture = Fixture::new();
    let missing = fixture.root.path().join("missing state");
    let output = fixture.run(&missing);
    assert!(!output.status.success() && output.stdout.is_empty());
    assert!(!missing.exists());
    fixture
        .conn
        .execute("UPDATE meta SET value='999' WHERE key='schema_version'", [])
        .unwrap();
    let before = fixture.content();
    let output = fixture.run(&fixture.home);
    assert!(!output.status.success() && output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("supported accounting"));
    assert!(
        before == fixture.content(),
        "unsupported state was modified"
    );
    fixture
        .conn
        .execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [agentd::STATE_SCHEMA_VERSION.to_string()],
        )
        .unwrap();
    fixture
        .conn
        .execute("UPDATE usage_tracking SET bytes=-1", [])
        .unwrap();
    let output = fixture.run(&fixture.home);
    assert!(!output.status.success() && output.stdout.is_empty());
    fixture
        .conn
        .execute("UPDATE usage_tracking SET bytes=12345", [])
        .unwrap();
    for index in 0..129 {
        fixture
            .conn
            .execute_batch(&format!("CREATE TABLE usage_extra_{index}(value TEXT);"))
            .unwrap();
    }
    let before = fixture.content();
    let output = fixture.run(&fixture.home);
    assert!(!output.status.success() && output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("object limit"));
    assert!(
        before == fixture.content(),
        "refused inspection changed state"
    );
}
