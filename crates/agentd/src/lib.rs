//! `agentd`: the AgentDocker daemon.
//!
//! One process per host. It supervises agent processes, keeps the registry,
//! routes messages between agents, and arbitrates leases on shared
//! resources. Clients talk to it over a Unix socket; see
//! `agentdocker_core::protocol`.
//!
//! This is a library so that the `agentdocker` package can ship the `agentd`
//! binary beside the CLI — one `cargo install agentdocker` gets both. The
//! binary is [`main`] and nothing else.

pub mod daemon;
mod owner;
pub mod reconcile;
mod server;
#[cfg(windows)]
mod sqlite_windows;
mod store;
mod supervisor;
#[cfg(unix)]
mod takeover;
mod watcher;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agentdocker_core::{EventKind, paths};
use agentdocker_host::lock;
use clap::Parser;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;
#[cfg(unix)]
use tracing::warn;
use tracing_subscriber::EnvFilter;

use crate::daemon::Daemon;

/// The schema this executable writes when it opens daemon state.
pub const STATE_SCHEMA_VERSION: u32 = store::SCHEMA_VERSION as u32;

/// Prepare the process-wide storage platform before starting worker threads.
/// On Windows this permanently installs SQLite's private-file creation hook;
/// on other platforms it does nothing. Call this before using raw SQLite
/// connections in a process that embeds agentd. AgentDocker's own connection
/// entry points and test fixtures also wait for this initialization.
pub fn initialize_storage_platform() -> anyhow::Result<()> {
    #[cfg(windows)]
    sqlite_windows::initialize()?;
    Ok(())
}

/// Raw fixture databases must wait for the same initialization as Store. In
/// particular, an in-memory fixture must not initialize SQLite concurrently
/// with installation of its process-wide Windows syscall hook.
#[cfg(test)]
mod sqlite_fixture {
    pub(crate) fn open(path: impl AsRef<std::path::Path>) -> anyhow::Result<rusqlite::Connection> {
        super::initialize_storage_platform()?;
        Ok(rusqlite::Connection::open(path)?)
    }

    pub(crate) fn in_memory() -> anyhow::Result<rusqlite::Connection> {
        super::initialize_storage_platform()?;
        Ok(rusqlite::Connection::open_in_memory()?)
    }
}

/// Read the compatibility floor without creating, migrating or opening a
/// daemon. An unreadable existing database is an error, never an absent home.
pub fn stored_state_schema(home: &std::path::Path) -> anyhow::Result<Option<u32>> {
    use anyhow::Context;
    initialize_storage_platform()?;
    let path = home.join("state.db");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => (),
    }
    agentdocker_host::dirs::check_private_dir(home)?;
    agentdocker_host::dirs::read_private_file(&path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut companion = path.as_os_str().to_owned();
        companion.push(suffix);
        let companion = PathBuf::from(companion);
        match std::fs::symlink_metadata(&companion) {
            Ok(_) => {
                agentdocker_host::dirs::read_private_file(&companion)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_millis(250))?;
    let value: String = connection.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    let schema: u32 = value.parse().context("stored state schema is invalid")?;
    anyhow::ensure!(schema > 0, "stored state schema must be positive");
    Ok(Some(schema))
}

#[derive(Parser)]
#[command(
    name = "agentd",
    version,
    about = "AgentDocker daemon: supervises agents, routes messages, arbitrates leases"
)]
pub struct Args {
    /// Print this binary's version, platform and state schema as JSON, then exit without opening state.
    #[arg(long)]
    build_info: bool,

    /// Directory for the socket, logs and state.
    #[arg(long, env = "AGENTDOCKER_HOME", default_value_os_t = agentdocker_host::dirs::home())]
    home: PathBuf,

    /// Unix socket to listen on (default: <home>/agentd.sock).
    #[arg(long, env = "AGENTDOCKER_SOCKET")]
    socket: Option<PathBuf>,

    /// Run as one managed agent's session owner: read the launch from
    /// stdin, hold the child and its terminal, serve the daemon on the
    /// agent's session socket. Started by the daemon, not by hand.
    #[arg(long, hide = true)]
    session_owner: bool,

    /// Take over from a running daemon: receive its listener, lock and
    /// transfer on this inherited descriptor, accept the transfer as the
    /// first write, then serve. Started by the predecessor, not by hand.
    #[arg(long, hide = true, value_name = "FD")]
    take_over: Option<i32>,
}

/// Stack for the thread the daemon runs on: its one future is large in a
/// debug build, and a Windows main thread has 1 MiB against 8 on Unix
/// (the CLI's parser overflowed one on the first Windows runner, #206).
/// Reserving this costs nothing until it is touched.
const MAIN_STACK: usize = 32 << 20;

/// Parse the command line and run the daemon until SIGTERM or Ctrl-C.
pub fn main() -> anyhow::Result<()> {
    agentdocker_host::installation::redirect_managed_launcher()?;
    let args = Args::parse();
    initialize_storage_platform()?;
    let worker = std::thread::Builder::new()
        .name("agentd".into())
        .stack_size(MAIN_STACK)
        .spawn(move || run(args))
        .map_err(|error| anyhow::anyhow!("cannot start the daemon's thread: {error}"))?;
    match worker.join() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Run the daemon until SIGTERM or Ctrl-C. Exits at once, successfully,
/// when another daemon already holds the socket's lock: clients start a
/// daemon when they cannot connect, and two may race to do so.
pub fn run(args: Args) -> anyhow::Result<()> {
    if args.session_owner {
        let launch: owner::Launch = serde_json::from_reader(std::io::stdin().lock())
            .map_err(|error| anyhow::anyhow!("session owner expects a launch on stdin: {error}"))?;
        let code = owner::main(launch)?;
        std::process::exit(code);
    }
    serve(args)
}

#[tokio::main]
async fn serve(args: Args) -> anyhow::Result<()> {
    if args.build_info {
        println!(
            "{}",
            serde_json::json!({
                "format": 1,
                "version": env!("CARGO_PKG_VERSION"),
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "state_schema": store::SCHEMA_VERSION,
                "installation_lock": agentdocker_host::installation::LOCK_FORMAT,
                "launcher_redirect": agentdocker_host::installation::LAUNCHER_REDIRECT_FORMAT,
            })
        );
        return Ok(());
    }
    let _installation_pin = agentdocker_host::installation::pin_current_executable()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // One spelling of the home, whatever it was given as, so the socket
    // directory it derives is the one clients derive.
    let home = agentdocker_host::dirs::canonical_home(args.home);
    agentdocker_host::dirs::secure_state_dir(&home)?;
    let socket = args
        .socket
        .unwrap_or_else(|| agentdocker_host::dirs::socket_path(&home));
    agentdocker_host::dirs::check_socket_parent(&socket)?;
    let lock_path = paths::daemon_lock(&home, &socket);
    if let Some(parent) = lock_path.parent() {
        if parent == paths::socket_dir(&home) && parent != home {
            agentdocker_host::dirs::ensure_private_dir(parent)?;
        } else {
            std::fs::create_dir_all(parent)?;
        }
    }
    // A successor receives the lock and listener from its predecessor
    // instead of taking them; an ordinary daemon takes both itself. Either
    // way the daemon keeps a copy of each to hand on in its turn. Windows
    // hands nothing over: a daemon there always takes both.
    #[cfg(windows)]
    anyhow::ensure!(
        args.take_over.is_none(),
        "a daemon handover is not available on Windows"
    );
    #[cfg(unix)]
    let takeover = args.take_over.map(takeover::receive).transpose()?;
    #[cfg(windows)]
    let takeover: Option<std::convert::Infallible> = None;
    let own_lock = match &takeover {
        #[cfg(unix)]
        Some(handover) => {
            anyhow::ensure!(
                handover.handover.home == home && handover.handover.socket == socket,
                "the handover names a different home or socket than this process was started with"
            );
            None
        }
        #[cfg(windows)]
        Some(never) => match *never {},
        None => {
            let Some(lock) = lock::try_exclusive(&lock_path)? else {
                info!(lock = %lock_path.display(), "another agentd holds the lock; exiting");
                return Ok(());
            };
            Some(lock)
        }
    };
    // A successor opens pending: the schema comes forward, its recorded
    // version only with the acceptance below, so an aborted takeover
    // leaves a database the predecessor still opens.
    let daemon = Arc::new(match &takeover {
        Some(_) => Daemon::open_pending(home, socket)?,
        None => Daemon::open(home, socket)?,
    });
    daemon.reload_policies();
    // A successor is fenced here and takes no pins; it takes them right
    // after accepting, before the predecessor hears it serves.
    daemon.pin_controllers();
    // Bind before any restored command can execute. Poll serving alongside
    // restoration so an agent's first hook/MCP request can receive a reply.
    #[cfg(unix)]
    let (listener, predecessor, inherited_restricted, owners_reattached) = match takeover {
        Some(handover) => {
            let listener = handover.tokio_listener()?;
            let restricted = handover.tokio_restricted()?;
            daemon.hold(daemon::reload::Held {
                listener: handover.listener,
                lock: handover.lock,
                restricted: handover.restricted,
                restricted_unavailable: None,
            });
            // Everything slow happens before the one write that makes this
            // process the coordinator: owners are reattached while still
            // fenced (their exit reports wait as deferred writes), so that
            // acceptance and readiness are the same moment. A successor
            // that fails before this point has written nothing and the
            // predecessor takes authority back; one that fails after it was
            // already serving, and a service manager restarts it.
            daemon.reattach_owners().await;
            if let Err(reason) = daemon.accept_transfer(&handover.handover.transfer) {
                let _ = daemon::reload::answer_async(
                    handover.socket,
                    daemon::reload::Ready::Failed {
                        reason: reason.clone(),
                    },
                )
                .await;
                anyhow::bail!("take-over refused: {reason}");
            }
            // The bindings' releases are held before the predecessor is told
            // and lets go of its own pins: a retained version with a dormant
            // or restarting controller must never be unpinned in between.
            daemon.pin_controllers();
            info!(
                transfer = %handover.handover.transfer,
                "took over from the predecessor; serving on its listener"
            );
            (listener, Some(handover.socket), restricted, true)
        }
        None => {
            let listener = server::bind(&daemon).await?;
            if let Some(lock) = own_lock {
                daemon.hold(daemon::reload::Held {
                    listener: server::listener_fd(&listener)?,
                    lock: lock.into_fd(),
                    restricted: None,
                    restricted_unavailable: None,
                });
            }
            (listener, None, None, false)
        }
    };
    #[cfg(windows)]
    let (listener, predecessor, inherited_restricted, owners_reattached): (
        agentdocker_host::ipc::Listener,
        Option<std::convert::Infallible>,
        Option<agentdocker_host::ipc::Listener>,
        bool,
    ) = {
        let listener = server::bind(&daemon).await?;
        if own_lock.is_some() {
            daemon.hold(daemon::reload::Held {});
        }
        (listener, None, None, false)
    };
    daemon.expect_watcher();
    watcher::spawn(daemon.clone());
    daemon.notify_desktop();
    daemon.reload_webhooks().await;
    daemon.collect_usage();

    let maintenance = async {
        // Liveness and lease expiration must not retire restore candidates
        // while their identities and protection are being recovered.
        if !owners_reattached {
            daemon.reattach_owners().await;
        }
        daemon.restore_agents().await;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut ticks: u64 = 0;
        loop {
            ticker.tick().await;
            daemon.reconcile_containers();
            daemon.expire_leases();
            daemon.flush_notices();
            daemon.check_liveness();
            daemon.resume_restarts();
            daemon.tend_controllers();
            // A `stat` per policy file, so editing one takes effect
            // within a second without a restart or a signal.
            daemon.reload_policies();
            ticks += 1;
            if ticks.is_multiple_of(5) {
                daemon.refresh_project_checkouts().await;
                daemon.refresh_vcs(None).await;
                let _ = daemon.scan_agents().await;
                daemon.reload_webhooks().await;
            }
            if ticks.is_multiple_of(60) {
                daemon.prune_events();
                daemon.prune_changes();
                daemon.apply_journal_retention();
                daemon.apply_message_retention();
                daemon.evict_journal_rings();
            }
        }
    };

    let restricted = paths::container_socket(&daemon.home);
    // A successor tells its predecessor it is serving: the transfer is
    // accepted, owners are reattached, and the inherited listener is
    // bound with its queue drained by the accept loop polled alongside.
    let announcer = async {
        #[cfg(unix)]
        if let Some(socket) = predecessor {
            let answered =
                daemon::reload::answer_async(socket, daemon::reload::Ready::Serving).await;
            if answered.is_err() {
                warn!(
                    ?answered,
                    "could not tell the predecessor we are serving; it will time out and check the store"
                );
            }
        }
        #[cfg(windows)]
        let _ = predecessor;
        std::future::pending::<()>().await
    };
    let result = tokio::select! {
        served = server::serve(daemon.clone(), listener) => served,
        () = maintenance => Ok(()),
        () = announcer => Ok(()),
        () = server::restricted_endpoint(daemon.clone(), restricted.clone(), inherited_restricted) => Ok(()),
        () = shutdown_signal() => {
            info!("shutting down on signal");
            daemon.emit(EventKind::DaemonStopping { reason: "signal".to_owned() });
            Ok(())
        }
        () = daemon.shutdown_requested() => {
            info!("shutting down on request");
            daemon.emit(EventKind::DaemonStopping { reason: "request".to_owned() });
            Ok(())
        }
        () = daemon.transferred_exit() => {
            // The successor owns the agents, the socket and the lock now.
            // Leave all three alone.
            info!("handed over to a successor; exiting without stopping agents");
            return Ok(());
        }
    };

    daemon.stop_all().await;
    let _ = std::fs::remove_file(&daemon.socket);
    let _ = std::fs::remove_file(&restricted);
    result
}

#[cfg(unix)]
async fn shutdown_signal() {
    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

/// Windows has no SIGTERM; a console control event (Ctrl-C, Ctrl-Break, a
/// closing console) is what stops the daemon, besides a `daemon stop`.
#[cfg(windows)]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn schema_probe_reports_newer_state_without_migrating_or_creating_files() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing");
        assert_eq!(stored_state_schema(&missing).unwrap(), None);
        assert!(!missing.exists());
        let path = tmp.path().join("state.db");
        let conn = crate::sqlite_fixture::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES('schema_version','999');").unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert_eq!(stored_state_schema(tmp.path()).unwrap(), Some(999));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::write(&path, b"unreadable schema").unwrap();
        assert!(stored_state_schema(tmp.path()).is_err());
    }
}
