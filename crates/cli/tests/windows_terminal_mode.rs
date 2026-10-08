//! Actual console-mode regressions without the Unix-only CLI unit fixtures.
#![cfg(windows)]
#[path = "../src/codex_input/terminal/mode/windows.rs"]
mod windows_mode;
