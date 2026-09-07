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
mod server;
mod store;
mod supervisor;
mod watcher;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agentdocker_core::{EventKind, paths};
use agentdocker_host::lock;
use clap::Parser;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::daemon::Daemon;

#[derive(Parser)]
#[command(
    name = "agentd",
    version,
    about = "AgentDocker daemon: supervises agents, routes messages, arbitrates leases"
)]
pub struct Args {
    /// Directory for the socket, logs and state.
    #[arg(long, env = "AGENTDOCKER_HOME", default_value_os_t = agentdocker_host::dirs::home())]
    home: PathBuf,

    /// Unix socket to listen on (default: <home>/agentd.sock).
    #[arg(long, env = "AGENTDOCKER_SOCKET")]
    socket: Option<PathBuf>,

    /// Take over the terminals a daemon being replaced is holding, from
    /// this private socket. Set by `agentdocker daemon reload`; not
    /// something to run by hand.
    #[arg(long, hide = true)]
    receive_handoff: Option<PathBuf>,
}

/// Parse the command line and run the daemon until SIGTERM or Ctrl-C.
pub fn main() -> anyhow::Result<()> {
    run(Args::parse())
}

/// Run the daemon until SIGTERM or Ctrl-C. Exits at once, successfully,
/// when another daemon already holds the socket's lock: clients start a
/// daemon when they cannot connect, and two may race to do so.
#[tokio::main]
pub async fn run(args: Args) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // One spelling of the home, whatever it was given as, so the socket
    // directory it derives is the one clients derive.
    let home = agentdocker_host::dirs::canonical_home(args.home);
    let socket = args.socket.unwrap_or_else(|| paths::socket_path(&home));
    agentdocker_host::dirs::check_socket_parent(&socket)?;
    let lock_path = paths::lock_path(&socket);
    if let Some(parent) = lock_path.parent() {
        if parent == paths::socket_dir(&home) && parent != home {
            agentdocker_host::dirs::ensure_private_dir(parent)?;
        } else {
            std::fs::create_dir_all(parent)?;
        }
    }
    // The terminals come across *before* the lock, and this order is
    // forced: the daemon being replaced still holds the lock while it
    // sends them, and only lets go once they are across. A replacement
    // that took the lock first would find it held and exit, and the
    // handoff would wait for a connection that never came.
    let carried = match &args.receive_handoff {
        Some(socket) => match daemon::reload::collect(socket) {
            Ok(carried) => Some(carried),
            Err(error) => {
                // Not fatal. The agents are still running; what is lost
                // is the ability to attach to them, which is what the
                // old daemon's exit would have cost anyway.
                warn!(%error, "could not take the previous daemon's terminals");
                None
            }
        },
        None => None,
    };
    // Waiting, not failing, when a handoff is on the way: the daemon
    // being replaced drops the lock as it exits, moments from now.
    let Some(_lock) = acquire_lock(&lock_path, carried.is_some()).await? else {
        info!(lock = %lock_path.display(), "another agentd holds the lock; exiting");
        return Ok(());
    };
    let daemon = Arc::new(Daemon::open(home, socket)?);

    // Installed before anything is served: an `attach` must never arrive
    // between the socket opening and the terminals existing.
    if let Some(carried) = carried {
        daemon.install_handoff(carried);
    }
    daemon.reload_policies();
    daemon.notify_desktop();
    // Before the reaper: an agent that was running is still marked live,
    // and its leases with it. Retiring it first would take them away.
    daemon.restore_agents().await;

    let containers = daemon.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            ticker.tick().await;
            containers.reconcile_containers();
        }
    });

    let reaper = daemon.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut ticks: u64 = 0;
        loop {
            ticker.tick().await;
            reaper.expire_leases();
            reaper.check_liveness();
            // A `stat` per policy file, so editing one takes effect
            // within a second without a restart or a signal.
            reaper.reload_policies();
            ticks += 1;
            if ticks.is_multiple_of(5) {
                reaper.refresh_vcs(None).await;
                let _ = reaper.scan_agents().await;
            }
            if ticks.is_multiple_of(60) {
                reaper.prune_events();
                reaper.prune_changes();
                reaper.evict_journal_rings();
            }
        }
    });

    daemon.expect_watcher();
    watcher::spawn(daemon.clone());

    let restricted = paths::container_socket(&daemon.home);
    let result = tokio::select! {
        served = server::serve(daemon.clone()) => served,
        () = server::restricted_endpoint(daemon.clone(), restricted.clone()) => Ok(()),
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
    };

    daemon.stop_all().await;
    let _ = std::fs::remove_file(&daemon.socket);
    let _ = std::fs::remove_file(&restricted);
    result
}

/// Take the daemon's lock, waiting a little when a handoff is on the
/// way because the daemon being replaced is about to let go of it.
async fn acquire_lock(
    path: &std::path::Path,
    replacing: bool,
) -> anyhow::Result<Option<lock::Lock>> {
    if let Some(held) = lock::try_exclusive(path)? {
        return Ok(Some(held));
    }
    if !replacing {
        return Ok(None);
    }
    // Long enough for the predecessor to finish exiting, short enough
    // that a stuck one is reported rather than waited on forever.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Some(held) = lock::try_exclusive(path)? {
            return Ok(Some(held));
        }
    }
    anyhow::bail!(
        "took the terminals but the daemon being replaced still holds {}; \
         it is still running and the agents are unharmed",
        path.display()
    )
}

async fn shutdown_signal() {
    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}
