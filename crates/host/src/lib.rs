//! Host-side I/O that both `agentd` and `agentdocker` need.
//!
//! `agentdocker-core` stays pure; this crate is where the filesystem, `git`,
//! and the process table are consulted. Nothing here holds state — every
//! function answers a question about the host as it is right now.

// The gate is a fork that holds the child before exec and a socketpair
// carrying the exec-error handshake: `pre_exec`, raw descriptors and a
// process-group `kill`, none of which Windows has. Windows starts a
// suspended process and resumes it, which is the same idea and a
// different implementation; until that exists the module is Unix-only.
#[cfg(unix)]
pub mod launch;
pub mod lock;
pub mod procinfo;
pub mod project;
pub mod vcs;

pub mod content;

pub mod command;

pub mod containers;
pub mod engine;
pub mod runtimes;

pub mod dirs;
// Passing an open descriptor to another process is `SCM_RIGHTS` over a
// Unix socket, and Windows has no equivalent — it duplicates handles
// into a named target process instead, which is a different mechanism
// with a different security model. `daemon reload` is Unix-only until
// that is built, so the module is too.
#[cfg(unix)]
pub mod handoff;
pub mod multiplexer;
pub mod notify;
#[cfg(unix)]
pub mod pty;
#[cfg_attr(windows, path = "transport/windows.rs")]
pub mod transport;

pub mod relay;

pub mod files;

pub mod ipc;
