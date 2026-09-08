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
    /// Print this binary's version, platform and state schema as JSON, then exit without opening state.
    #[arg(long)]
    build_info: bool,

    /// Directory for the socket, logs and state.
    #[arg(long, env = "AGENTDOCKER_HOME", default_value_os_t = agentdocker_host::dirs::home())]
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
    let socket = args.socket.unwrap_or_else(|| paths::socket_path(&home));
    agentdocker_host::dirs::check_socket_parent(&socket)?;
    let lock_path = paths::daemon_lock(&home, &socket);
    if let Some(parent) = lock_path.parent() {
        if parent == paths::socket_dir(&home) && parent != home {
            agentdocker_host::dirs::ensure_private_dir(parent)?;
        } else {
            std::fs::create_dir_all(parent)?;
        }
    }
    let Some(_lock) = lock::try_exclusive(&lock_path)? else {
        info!(lock = %lock_path.display(), "another agentd holds the lock; exiting");
        return Ok(());
    };
    let daemon = Arc::new(Daemon::open(home, socket)?);

    daemon.reload_policies();
    // Bind before any restored command can execute. Poll serving alongside
    // restoration so an agent's first hook/MCP request can receive a reply.
    let listener = server::bind(&daemon).await?;
    daemon.expect_watcher();
    watcher::spawn(daemon.clone());
    daemon.notify_desktop();

    let maintenance = async {
        // Liveness and lease expiration must not retire restore candidates
        // while their identities and protection are being recovered.
        daemon.restore_agents().await;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut ticks: u64 = 0;
        loop {
            ticker.tick().await;
            daemon.reconcile_containers();
            daemon.expire_leases();
            daemon.check_liveness();
            // A `stat` per policy file, so editing one takes effect
            // within a second without a restart or a signal.
            daemon.reload_policies();
            ticks += 1;
            if ticks.is_multiple_of(5) {
                daemon.refresh_project_checkouts().await;
                daemon.refresh_vcs(None).await;
                let _ = daemon.scan_agents().await;
            }
            if ticks.is_multiple_of(60) {
                daemon.prune_events();
                daemon.prune_changes();
                daemon.evict_journal_rings();
            }
        }
    };

    let restricted = paths::container_socket(&daemon.home);
    let result = tokio::select! {
        served = server::serve(daemon.clone(), listener) => served,
        () = maintenance => Ok(()),
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

async fn shutdown_signal() {
    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}
