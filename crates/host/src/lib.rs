//! Host-side I/O that both `agentd` and `agentdocker` need.
//!
//! `agentdocker-core` stays pure; this crate is where the filesystem, `git`,
//! and the process table are consulted. Nothing here holds state — every
//! function answers a question about the host as it is right now.

pub mod lock;
pub mod procinfo;
pub mod project;
pub mod vcs;

pub mod content;

pub mod command;

pub mod containers;
pub mod engine;

pub mod dirs;
pub mod transport;
