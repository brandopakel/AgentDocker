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

mod daemon;
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
use tracing::info;
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
    #[arg(long, env = "AGENTDOCKER_HOME", default_value_os_t = paths::default_home())]
    home: PathBuf,

    /// Unix socket to listen on (default: <home>/agentd.sock).
    #[arg(long, env = "AGENTDOCKER_SOCKET")]
    socket: Option<PathBuf>,
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

    let socket = args
        .socket
        .unwrap_or_else(|| paths::socket_path(&args.home));
    let lock_path = paths::lock_path(&socket);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let Some(_lock) = lock::try_exclusive(&lock_path)? else {
        info!(lock = %lock_path.display(), "another agentd holds the lock; exiting");
        return Ok(());
    };
    let daemon = Arc::new(Daemon::open(args.home, socket)?);

    let reaper = daemon.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut ticks: u64 = 0;
        loop {
            ticker.tick().await;
            reaper.expire_leases();
            reaper.check_liveness();
            ticks += 1;
            if ticks.is_multiple_of(60) {
                reaper.prune_events();
                reaper.prune_changes();
            }
        }
    });

    watcher::spawn(daemon.clone());

    let result = tokio::select! {
        served = server::serve(daemon.clone()) => served,
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

    daemon.stop_all();
    let _ = std::fs::remove_file(&daemon.socket);
    result
}

async fn shutdown_signal() {
    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}
