//! Native path resolution without the CLI unit suite's Unix-only fixtures.
#![cfg(windows)]
#![allow(dead_code)]
#[path = "../src/service/windows/references.rs"]
mod references;
