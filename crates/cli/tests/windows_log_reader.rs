// This target runs the actual pipe reader's kernel regression without the
// Unix-only CLI unit fixtures. The Windows workflow selects it explicitly.
#[cfg(windows)]
#[path = "../src/codex_input/external/launch/log_reader.rs"]
mod log_reader;
