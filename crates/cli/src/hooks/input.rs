//! Bounded stdin decoding shared by provider hook adapters.

use anyhow::{Context, Result, ensure};

const MAX_INPUT: usize = 1024 * 1024;

/// Windows has no `poll` on a console or pipe handle: a thread reads stdin
/// to its end and the deadline is kept by waiting for that thread. A read
/// that outlives the deadline is abandoned with the process, which exits
/// right after; nothing partial is ever acknowledged.
#[cfg(windows)]
pub(super) fn read<T: serde::de::DeserializeOwned>(
    fd: i32,
    timeout: std::time::Duration,
) -> Result<T> {
    use std::io::Read;
    ensure!(fd == 0, "hook input is read from stdin only");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let outcome = std::io::stdin()
            .lock()
            .take(MAX_INPUT as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(outcome);
    });
    let bytes = match receiver.recv_timeout(timeout) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => anyhow::bail!("hook input failed: {error}"),
        Err(_) => anyhow::bail!("hook input timed out or is unavailable"),
    };
    ensure!(bytes.len() <= MAX_INPUT, "hook input exceeds 1 MiB");
    serde_json::from_slice(&bytes).context("invalid provider hook event")
}

#[cfg(unix)]
pub(super) fn read<T: serde::de::DeserializeOwned>(
    fd: i32,
    timeout: std::time::Duration,
) -> Result<T> {
    let deadline = std::time::Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        ensure!(!remaining.is_zero(), "hook input timed out");
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized, borrowed descriptor; no fd ownership changes.
        let ready = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().min(i32::MAX as u128).max(1) as i32,
            )
        };
        if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(ready > 0, "hook input timed out or is unavailable");
        let mut chunk = [0u8; 4096];
        // SAFETY: this invocation is the sole stdin reader; poll established
        // readability, and chunk is valid for the requested length.
        let count = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if count < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(count >= 0, "hook input failed");
        if count == 0 {
            break;
        }
        ensure!(
            bytes.len() + count as usize <= MAX_INPUT,
            "hook input exceeds 1 MiB"
        );
        bytes.extend_from_slice(&chunk[..count as usize]);
    }
    serde_json::from_slice(&bytes).context("invalid provider hook event")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn a_trickling_writer_cannot_restart_the_input_deadline() {
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        writer
            .set_write_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let writing = active.clone();
        writer.write_all(b" ").unwrap();
        let thread = std::thread::spawn(move || {
            while writing.load(Ordering::Relaxed) {
                if writer.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let started = Instant::now();
        let result = read::<serde_json::Value>(reader.as_raw_fd(), Duration::from_millis(40));
        active.store(false, Ordering::Relaxed);
        thread.join().unwrap();
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn shared_decoder_accepts_actual_claude_event_fields() {
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        writer
            .write_all(
                br#"{"hook_event_name":"UserPromptSubmit","session_id":"fixture","cwd":"/tmp"}"#,
            )
            .unwrap();
        drop(writer);
        let event: super::super::HookInput =
            read(reader.as_raw_fd(), Duration::from_secs(1)).unwrap();
        assert_eq!(event.hook_event_name, "UserPromptSubmit");
        assert_eq!(event.session_id, "fixture");
    }
}
