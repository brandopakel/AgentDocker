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
    let path = status_path(home);
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("status path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".serve.{}.tmp", serving.pid));
    std::fs::write(&temporary, serde_json::to_vec_pretty(serving)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temporary, &path)
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
            bind: "127.0.0.1:1".into(),
            default_project: Some("/p/keel".into()),
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
