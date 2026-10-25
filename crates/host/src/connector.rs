//! What a serving remote connector says about itself, for the CLI's
//! `connector status` and for the desktop's Tools screen: a small JSON
//! file under the state home, written when `connector serve` is up and
//! removed by the process that wrote it. A file whose process is gone
//! is a stale one and reads as "not running".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The connector as it is serving right now.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Serving {
    pub pid: u32,
    /// The HTTPS origin the vendors reach; `/mcp` under it is the endpoint.
    pub public_url: String,
    /// The origin the person's browser opens for consent when it is not the
    /// public one (`--authorize-url`); `/authorize` under it is the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
    /// The loopback address the process listens on.
    pub bind: String,
    /// The project a browser agent joins unless the person picks another
    /// on the consent page.
    #[serde(default, alias = "project", skip_serializing_if = "Option::is_none")]
    pub default_project: Option<PathBuf>,
    pub pairing_code: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<TunnelStatus>,
    #[serde(default)]
    pub allowlist_prefixes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelStatus {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A Tailscale Funnel a connector set or is about to set: a configuration
/// in tailscaled that outlives the process. It is written before the set
/// can take effect and removed only once the Funnel is confirmed undone, so
/// a connector stopped at any point leaves the next one what it needs to
/// clear it, and nothing that is not its own.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunnelRecord {
    pub pid: u32,
    pub https_port: u16,
    /// The loopback address the Funnel points at, `127.0.0.1:<port>`.
    pub target: String,
}

impl FunnelRecord {
    /// Whether the process that set it is still there.
    pub fn alive(&self) -> bool {
        crate::procinfo::alive(self.pid)
    }
}

pub fn funnel_path(home: &Path) -> PathBuf {
    home.join("connector").join("funnel.json")
}

pub fn write_funnel(home: &Path, record: &FunnelRecord) -> std::io::Result<()> {
    write_private(
        &funnel_path(home),
        record.pid,
        &serde_json::to_vec_pretty(record)?,
    )
}

pub fn read_funnel(home: &Path) -> Option<FunnelRecord> {
    serde_json::from_str(&std::fs::read_to_string(funnel_path(home)).ok()?).ok()
}

/// Only the process that wrote the record takes it away, or a successor
/// that has seen to its Funnel; `pid` is the record's own.
pub fn clear_funnel(home: &Path, pid: u32) {
    if read_funnel(home).is_some_and(|r| r.pid == pid) {
        let _ = std::fs::remove_file(funnel_path(home));
    }
}

impl Serving {
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.public_url)
    }

    /// Whether the process that wrote the file is still there.
    pub fn alive(&self) -> bool {
        crate::procinfo::alive(self.pid)
    }
}

/// Where the serving connector describes itself.
pub fn status_path(home: &Path) -> PathBuf {
    home.join("connector").join("serve.json")
}

pub fn write_status(home: &Path, serving: &Serving) -> std::io::Result<()> {
    write_private(
        &status_path(home),
        serving.pid,
        &serde_json::to_vec_pretty(serving)?,
    )
}

/// Replace `path` with `bytes`, through a temporary of `pid`'s beside it.
fn write_private(path: &Path, pid: u32, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("status path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("state");
    let temporary = parent.join(format!(".{name}.{pid}.tmp"));
    // It carries the pairing code: private from its first byte, never
    // written at the umask's mode and narrowed after.
    match std::fs::remove_file(&temporary) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let mut file = crate::dirs::create_private_file(&temporary)?;
    std::io::Write::write_all(&mut file, bytes)?;
    drop(file);
    std::fs::rename(&temporary, path)
}

/// Only the process that wrote the file takes it away.
pub fn clear_status(home: &Path, pid: u32) {
    if read_status(home).is_some_and(|s| s.pid == pid) {
        let _ = std::fs::remove_file(status_path(home));
    }
}

/// The file as written, whether or not its process still runs.
pub fn read_status(home: &Path) -> Option<Serving> {
    let text = std::fs::read_to_string(status_path(home)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The connector serving now: the file, with its process alive.
pub fn serving(home: &Path) -> Option<Serving> {
    read_status(home).filter(Serving::alive)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serving(pid: u32) -> Serving {
        Serving {
            pid,
            public_url: "https://node.example.ts.net".into(),
            authorize_url: None,
            bind: "127.0.0.1:1".into(),
            default_project: Some("/p/orchard".into()),
            pairing_code: "ABCD-EFGH".into(),
            started_at: chrono::Utc::now(),
            tunnel: None,
            allowlist_prefixes: 0,
        }
    }

    #[test]
    fn the_status_file_round_trips_and_only_its_writer_clears_it() {
        let home = tempfile::tempdir().unwrap();
        let mine = serving(std::process::id());
        write_status(home.path(), &mine).unwrap();
        assert_eq!(read_status(home.path()), Some(mine.clone()));
        // `procinfo::alive` answers on Unix only; elsewhere every pid reads
        // as gone and nothing is ever "serving".
        if cfg!(unix) {
            assert_eq!(
                super::serving(home.path()),
                Some(mine.clone()),
                "this process is alive"
            );
        }
        assert_eq!(mine.mcp_url(), "https://node.example.ts.net/mcp");
        clear_status(home.path(), mine.pid + 1);
        assert!(
            read_status(home.path()).is_some(),
            "another pid clears nothing"
        );
        clear_status(home.path(), mine.pid);
        assert!(read_status(home.path()).is_none());
    }

    #[test]
    fn a_funnel_record_is_cleared_only_by_its_own_pid() {
        let home = tempfile::tempdir().unwrap();
        let record = FunnelRecord {
            pid: std::process::id(),
            https_port: 443,
            target: "127.0.0.1:8765".into(),
        };
        write_funnel(home.path(), &record).unwrap();
        assert_eq!(read_funnel(home.path()), Some(record.clone()));
        clear_funnel(home.path(), record.pid + 1);
        assert!(
            read_funnel(home.path()).is_some(),
            "another pid clears nothing"
        );
        clear_funnel(home.path(), record.pid);
        assert!(read_funnel(home.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_status_file_is_private_from_its_first_byte() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let mine = serving(std::process::id());
        // A temporary left by a crash is replaced, not written through.
        std::fs::create_dir_all(status_path(home.path()).parent().unwrap()).unwrap();
        let stale = status_path(home.path())
            .parent()
            .unwrap()
            .join(format!(".serve.{}.tmp", mine.pid));
        std::fs::write(&stale, b"old").unwrap();
        write_status(home.path(), &mine).unwrap();
        let mode = std::fs::metadata(status_path(home.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(read_status(home.path()), Some(mine));
        assert!(!stale.exists());
    }

    #[test]
    fn a_file_from_a_gone_process_reads_but_is_not_serving() {
        let home = tempfile::tempdir().unwrap();
        // Well above any live pid on a test machine; `alive` says no.
        let gone = serving(u32::MAX - 7);
        write_status(home.path(), &gone).unwrap();
        assert!(read_status(home.path()).is_some());
        assert!(super::serving(home.path()).is_none());
    }

    /// The field was `project` before it became a default the consent
    /// page may override; an older connector's file still reads.
    #[test]
    fn an_older_status_file_names_its_project() {
        let older = r#"{"pid":1,"public_url":"https://a","bind":"127.0.0.1:1","project":"/p/old","pairing_code":"X","started_at":"2026-09-18T00:00:00Z"}"#;
        let read: Serving = serde_json::from_str(older).unwrap();
        assert_eq!(
            read.default_project.as_deref(),
            Some(std::path::Path::new("/p/old"))
        );
    }
}
