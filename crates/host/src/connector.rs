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
    /// When the process that wrote this started, so a later process given
    /// its pid is not taken for it. Absent from a file written before it
    /// was kept, which then says nothing certain of a live pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_started_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelStatus {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When the tunnel's own process started, read while the connector
    /// owned it: its identity, never the connector's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
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
    /// When the connector that wrote this started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_started_at: Option<chrono::DateTime<chrono::Utc>>,
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

/// The connector serving now: [`state`]'s `Serving`.
#[cfg(unix)]
pub fn serving(home: &Path) -> Option<Serving> {
    match state(home) {
        ConnectorState::Serving(serving) => Some(serving),
        _ => None,
    }
}

/// The connector serving now: the file, with its process alive.
#[cfg(windows)]
pub fn serving(home: &Path) -> Option<Serving> {
    read_status(home).filter(Serving::alive)
}

/// The lock a serving connector holds for its life, and records itself in.
pub fn lock_path(home: &Path) -> PathBuf {
    home.join("connector").join("serve.lock")
}

/// The largest record file read: far more than any connector writes.
#[cfg(unix)]
const MAX_RECORD: u64 = 64 * 1024;

/// Where the connector stands, from its files alone, read without
/// creating or locking anything ([`state`]).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectorState {
    /// Nothing of a connector is left here.
    Absent,
    /// A recorded connector process is alive and has published no serving
    /// status, or none that is confirmed: the evidence is a hint about a
    /// process, never a reading of its lock.
    Starting { pid: u32, from: Evidence },
    /// Its status file names a live process, born when recorded.
    Serving(Serving),
    /// The connector that wrote what is left is gone, and this may still be
    /// open after it.
    Stopped {
        last: Option<Serving>,
        tunnel_left: TunnelLeft,
    },
    /// Nothing certain: a file that cannot be read, or a process whose
    /// identity cannot be told.
    Unknown { why: String, last: Option<Serving> },
}

/// Which file a `Starting` rests on.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// `serve.lock`'s owner record.
    Owner,
    /// A Funnel record its writer left.
    Funnel,
}

/// What a stopped connector may have left open.
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TunnelLeft {
    /// A gone connector's Funnel record: tailscaled may still publish it.
    pub funnel: Option<FunnelRecord>,
    pub process: TunnelProcess,
}

/// A recorded tunnel process, by its own identity.
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TunnelProcess {
    /// None was recorded, or the recorded one is gone.
    #[default]
    None,
    Live {
        provider: String,
        pid: u32,
    },
    Unknown {
        provider: String,
        pid: u32,
        why: String,
    },
}

/// A process identity, pid and birth.
#[cfg(unix)]
type Identity = (u32, Option<chrono::DateTime<chrono::Utc>>);

/// Each identity judged in one look, with what it was judged.
#[cfg(unix)]
type Judged = Vec<(Identity, crate::presence::Liveness)>;

/// One file read: missing, unreadable, or a record whose process is
/// judged once.
#[cfg(unix)]
enum Read<T> {
    Missing,
    Failed(String),
    Record(T, crate::presence::Liveness),
}

#[cfg(unix)]
fn read_record<T: serde::de::DeserializeOwned>(
    path: &Path,
    identity: impl Fn(&T) -> Identity,
    judged: &mut Judged,
) -> Read<T> {
    match crate::presence::read_small(path, MAX_RECORD) {
        Ok(None) => Read::Missing,
        Err(why) => Read::Failed(why),
        Ok(Some(bytes)) => match serde_json::from_slice::<T>(&bytes) {
            Err(_) => Read::Failed(format!("{} cannot be read", path.display())),
            Ok(record) => {
                let key = identity(&record);
                let liveness = judge(key, judged);
                Read::Record(record, liveness)
            }
        },
    }
}

/// Each process identity is judged once per look, so two readings of one
/// process never disagree.
#[cfg(unix)]
fn judge(key: Identity, judged: &mut Judged) -> crate::presence::Liveness {
    if let Some((_, liveness)) = judged.iter().find(|(seen, _)| *seen == key) {
        return liveness.clone();
    }
    let liveness = crate::presence::liveness(key.0, key.1);
    judged.push((key, liveness.clone()));
    liveness
}

/// Where the connector stands, read from `serve.json`, `serve.lock`'s owner
/// record and `funnel.json`, without creating or locking anything. First
/// match wins: positive evidence of a live process outranks every read
/// error; any read error or uncertain identity outranks Stopped and Absent;
/// Absent is only what is left. An empty owner record (an older connector
/// never wrote one) is no evidence either way.
#[cfg(unix)]
pub fn state(home: &Path) -> ConnectorState {
    use crate::presence::Liveness;
    let mut judged = Vec::new();
    let status = read_record::<Serving>(
        &status_path(home),
        |s| (s.pid, s.process_started_at),
        &mut judged,
    );
    let owner = match crate::presence::owner_record(&lock_path(home)) {
        Ok(None) => Read::Missing,
        Err(why) => Read::Failed(why),
        Ok(Some(record)) => {
            let liveness = judge((record.pid, record.started_at), &mut judged);
            Read::Record(record, liveness)
        }
    };
    let funnel = read_record::<FunnelRecord>(
        &funnel_path(home),
        |f| (f.pid, f.process_started_at),
        &mut judged,
    );
    let last = match &status {
        Read::Record(serving, _) => Some(serving.clone()),
        _ => None,
    };
    // 1. Serving.
    if let Read::Record(serving, Liveness::Live) = &status {
        return ConnectorState::Serving(serving.clone());
    }
    // 2. A live recorded owner without a live status.
    if let Read::Record(record, Liveness::Live) = &owner {
        return ConnectorState::Starting {
            pid: record.pid,
            from: Evidence::Owner,
        };
    }
    // 3. A live Funnel writer without a live status.
    if let Read::Record(record, Liveness::Live) = &funnel {
        return ConnectorState::Starting {
            pid: record.pid,
            from: Evidence::Funnel,
        };
    }
    // 4. Anything uncertain.
    let uncertain = [
        ("serve.json", read_why(&status)),
        ("serve.lock", read_why(&owner)),
        ("funnel.json", read_why(&funnel)),
    ]
    .into_iter()
    .find_map(|(file, why)| why.map(|why| format!("{file}: {why}")));
    if let Some(why) = uncertain {
        return ConnectorState::Unknown { why, last };
    }
    let left_funnel = match funnel {
        Read::Record(record, Liveness::Gone) => Some(record),
        _ => None,
    };
    // 5. A gone status.
    if let Read::Record(serving, Liveness::Gone) = status {
        let process = tunnel_process(serving.tunnel.as_ref(), &mut judged);
        return ConnectorState::Stopped {
            last: Some(serving),
            tunnel_left: TunnelLeft {
                funnel: left_funnel,
                process,
            },
        };
    }
    // 6. A gone Funnel writer, no status.
    if let Some(funnel) = left_funnel {
        return ConnectorState::Stopped {
            last: None,
            tunnel_left: TunnelLeft {
                funnel: Some(funnel),
                process: TunnelProcess::None,
            },
        };
    }
    // 7. Nothing left.
    ConnectorState::Absent
}

/// Why a read is uncertain, if it is.
#[cfg(unix)]
fn read_why<T>(read: &Read<T>) -> Option<String> {
    match read {
        Read::Failed(why) => Some(why.clone()),
        Read::Record(_, crate::presence::Liveness::Unknown { why }) => Some(why.clone()),
        _ => None,
    }
}

/// A recorded tunnel process, judged by its own pid and birth.
#[cfg(unix)]
fn tunnel_process(tunnel: Option<&TunnelStatus>, judged: &mut Judged) -> TunnelProcess {
    use crate::presence::Liveness;
    let Some((tunnel, pid)) = tunnel.and_then(|t| t.pid.map(|pid| (t, pid))) else {
        return TunnelProcess::None;
    };
    match judge((pid, tunnel.started_at), judged) {
        Liveness::Live => TunnelProcess::Live {
            provider: tunnel.provider.clone(),
            pid,
        },
        Liveness::Gone => TunnelProcess::None,
        Liveness::Unknown { why } => TunnelProcess::Unknown {
            provider: tunnel.provider.clone(),
            pid,
            why,
        },
    }
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
            process_started_at: crate::procinfo::start_time(pid),
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

    /// A process that has ended, and its pid.
    #[cfg(unix)]
    fn ended_pid() -> u32 {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[cfg(unix)]
    struct Home(tempfile::TempDir);

    #[cfg(unix)]
    impl Home {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(home.path().join("connector")).unwrap();
            Home(home)
        }
        fn path(&self) -> &Path {
            self.0.path()
        }
        fn status(&self, pid: u32, born: Option<chrono::DateTime<chrono::Utc>>) -> Serving {
            let mut record = serving(pid);
            record.process_started_at = born;
            std::fs::write(
                status_path(self.path()),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            record
        }
        fn owner(&self, text: &str) {
            std::fs::write(lock_path(self.path()), text).unwrap();
        }
        fn owner_of(&self, pid: u32, born: Option<chrono::DateTime<chrono::Utc>>) {
            let record = crate::presence::OwnerRecord {
                pid,
                started_at: born,
            };
            self.owner(&serde_json::to_string(&record).unwrap());
        }
        fn funnel(&self, pid: u32, born: Option<chrono::DateTime<chrono::Utc>>) -> FunnelRecord {
            let record = FunnelRecord {
                pid,
                https_port: 443,
                target: "127.0.0.1:8765".into(),
                process_started_at: born,
            };
            std::fs::write(
                funnel_path(self.path()),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            record
        }
        fn broken(&self, path: PathBuf) {
            std::fs::write(path, "{").unwrap();
        }
        fn state(&self) -> ConnectorState {
            state(self.path())
        }
    }

    /// The precedence table: positive evidence of a live process first,
    /// then anything uncertain, then what a gone process left, and Absent
    /// only for what is left.
    #[cfg(unix)]
    #[test]
    fn where_the_connector_stands_is_read_in_one_order() {
        let me = std::process::id();
        let born = crate::procinfo::start_time(me);
        let gone = ended_pid();
        // Nothing, an empty owner record, a clean stop's gone record.
        let home = Home::new();
        assert_eq!(home.state(), ConnectorState::Absent);
        home.owner("");
        assert_eq!(
            home.state(),
            ConnectorState::Absent,
            "an empty record is no evidence"
        );
        home.owner_of(gone, None);
        assert_eq!(home.state(), ConnectorState::Absent, "a clean stop");
        // A live recorded owner, no status: starting.
        home.owner_of(me, born);
        assert_eq!(
            home.state(),
            ConnectorState::Starting {
                pid: me,
                from: Evidence::Owner
            }
        );
        // An owner record that cannot be trusted, with no status: unknown.
        for broken in ["{", &"x".repeat(300)] {
            home.owner(broken);
            assert!(
                matches!(home.state(), ConnectorState::Unknown { .. }),
                "{broken:.5}"
            );
        }
        home.owner_of(me, None);
        assert!(
            matches!(home.state(), ConnectorState::Unknown { .. }),
            "an uncertain identity"
        );
        // Serving outranks an unreadable Funnel record.
        let home = Home::new();
        let serving_now = home.status(me, born);
        home.broken(funnel_path(home.path()));
        assert_eq!(home.state(), ConnectorState::Serving(serving_now.clone()));
        assert_eq!(super::serving(home.path()), Some(serving_now));
        // A live owner outranks an unreadable Funnel record too.
        let home = Home::new();
        home.owner_of(me, born);
        home.broken(funnel_path(home.path()));
        assert_eq!(
            home.state(),
            ConnectorState::Starting {
                pid: me,
                from: Evidence::Owner
            }
        );
        // Judged once per identity: a status of the same pid without its
        // birth is another identity, and the live owner still says starting.
        home.status(me, None);
        assert_eq!(
            home.state(),
            ConnectorState::Starting {
                pid: me,
                from: Evidence::Owner
            }
        );
        // A gone status beside an unreadable owner: unknown, not stopped.
        let home = Home::new();
        let old = home.status(gone, None);
        home.owner("{");
        assert_eq!(
            home.state(),
            ConnectorState::Unknown {
                why: match home.state() {
                    ConnectorState::Unknown { why, .. } => why,
                    other => panic!("{other:?}"),
                },
                last: Some(old)
            }
        );
        // A live Funnel writer, no status, an owner missing, empty or
        // uncertain: starting, from the Funnel.
        for owner in [None, Some(""), Some("uncertain")] {
            let home = Home::new();
            home.funnel(me, born);
            match owner {
                Some("uncertain") => home.owner_of(me, None),
                Some(text) => home.owner(text),
                None => {}
            }
            assert_eq!(
                home.state(),
                ConnectorState::Starting {
                    pid: me,
                    from: Evidence::Funnel
                },
                "{owner:?}"
            );
        }
        // A FIFO where the status belongs: unknown, never a hang.
        let home = Home::new();
        let fifo = std::ffi::CString::new(status_path(home.path()).as_os_str().as_encoded_bytes())
            .unwrap();
        // SAFETY: a NUL-terminated path and a mode.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(matches!(home.state(), ConnectorState::Unknown { .. }));
    }

    /// What a gone connector may have left, each process by its own
    /// identity: a Funnel record, and a tunnel child alive, reused, or of
    /// unknown identity.
    #[cfg(unix)]
    #[test]
    fn a_stopped_connector_says_what_it_may_have_left() {
        let gone = ended_pid();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let child_pid = child.id();
        let child_born = crate::procinfo::start_time(child_pid);
        let with_tunnel = |home: &Home, born: Option<chrono::DateTime<chrono::Utc>>| {
            let mut record = serving(gone);
            record.process_started_at = None;
            record.tunnel = Some(TunnelStatus {
                provider: "cloudflared".into(),
                pid: Some(child_pid),
                name: None,
                started_at: born,
            });
            std::fs::write(
                status_path(home.path()),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            record
        };
        let home = Home::new();
        let last = with_tunnel(&home, child_born);
        let funnel = home.funnel(gone, None);
        assert_eq!(
            home.state(),
            ConnectorState::Stopped {
                last: Some(last),
                tunnel_left: TunnelLeft {
                    funnel: Some(funnel),
                    process: TunnelProcess::Live {
                        provider: "cloudflared".into(),
                        pid: child_pid,
                    },
                },
            }
        );
        // The same pid with another birth: a reused pid, nothing left.
        let home = Home::new();
        with_tunnel(
            &home,
            child_born.map(|born| born - chrono::Duration::days(1)),
        );
        assert!(matches!(
            home.state(),
            ConnectorState::Stopped {
                tunnel_left: TunnelLeft {
                    process: TunnelProcess::None,
                    ..
                },
                ..
            }
        ));
        // An older status with no tunnel birth: unknown, never "no tunnel".
        let home = Home::new();
        with_tunnel(&home, None);
        assert!(matches!(
            home.state(),
            ConnectorState::Stopped {
                tunnel_left: TunnelLeft {
                    process: TunnelProcess::Unknown { .. },
                    ..
                },
                ..
            }
        ));
        // A gone Funnel writer and no status: stopped, the Funnel left.
        let home = Home::new();
        let funnel = home.funnel(gone, None);
        assert_eq!(
            home.state(),
            ConnectorState::Stopped {
                last: None,
                tunnel_left: TunnelLeft {
                    funnel: Some(funnel),
                    process: TunnelProcess::None
                },
            }
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// An older status file, with no birth, reads as written.
    #[test]
    fn an_older_status_file_reads_with_no_birth() {
        let text = r#"{"pid":7,"public_url":"https://x","bind":"127.0.0.1:1","pairing_code":"A","started_at":"2026-01-01T00:00:00Z","tunnel":{"provider":"cloudflared","pid":8}}"#;
        let record: Serving = serde_json::from_str(text).unwrap();
        assert_eq!(record.process_started_at, None);
        assert_eq!(record.tunnel.unwrap().started_at, None);
    }

    #[test]
    fn a_funnel_record_is_cleared_only_by_its_own_pid() {
        let home = tempfile::tempdir().unwrap();
        let record = FunnelRecord {
            pid: std::process::id(),
            https_port: 443,
            target: "127.0.0.1:8765".into(),
            process_started_at: None,
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
