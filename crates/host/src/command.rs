//! Bounded local command execution for Git integration operations.
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGroup {
    child: Child,
    reaped: bool,
}
impl Drop for ChildGroup {
    fn drop(&mut self) {
        if !self.reaped {
            // SAFETY: the unreaped child still reserves its PID. Never signal
            // its cached group number once that identity can be reused.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}

pub struct Output {
    pub success: bool,
    /// Standard output only, for structured engine responses.
    pub stdout: String,
    /// Standard output followed by standard error, for diagnostics.
    pub text: String,
}

/// Capture at most 4 MiB and stop the entire subprocess group on timeout.
pub fn run(root: &Path, argv: &[String], timeout: Duration) -> io::Result<Output> {
    let program = argv
        .first()
        .ok_or_else(|| io::Error::other("empty command"))?;
    let mut log = tempfile::tempfile()?;
    let mut errors = tempfile::tempfile()?;
    let mut command = Command::new(program);
    command
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(errors.try_clone()?)
        .process_group(0);
    let mut child = ChildGroup {
        child: command.spawn()?,
        reaped: false,
    };
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            child.reaped = true;
            break status;
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "command timed out after {timeout:?}"
            )));
        }
        if log
            .metadata()?
            .len()
            .saturating_add(errors.metadata()?.len())
            > 4 * 1024 * 1024
        {
            return Err(io::Error::other("command output exceeded 4 MiB"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    use std::io::{Seek, SeekFrom};
    log.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    log.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(io::Error::other("command output exceeded 4 MiB"));
    }
    errors.seek(SeekFrom::Start(0))?;
    let mut stderr = Vec::new();
    errors.take(4 * 1024 * 1024 + 1).read_to_end(&mut stderr)?;
    if bytes.len().saturating_add(stderr.len()) > 4 * 1024 * 1024 {
        return Err(io::Error::other("command output exceeded 4 MiB"));
    }
    let stdout = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Output {
        success: status.success(),
        text: format!("{stdout}{}", String::from_utf8_lossy(&stderr)),
        stdout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_and_output_limits_report_distinct_causes() {
        let tmp = tempfile::tempdir().unwrap();
        let timeout = run(
            tmp.path(),
            &["sh".into(), "-c".into(), "sleep 10".into()],
            Duration::from_millis(20),
        )
        .err()
        .unwrap();
        assert!(timeout.to_string().contains("timed out"));
        let output = run(
            tmp.path(),
            &["sh".into(), "-c".into(), "head -c 5000000 /dev/zero".into()],
            Duration::from_secs(5),
        )
        .err()
        .unwrap();
        assert!(output.to_string().contains("output exceeded"));
    }
}
