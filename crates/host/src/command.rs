//! Bounded local command execution for Git integration operations.
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGroup(Child);
impl Drop for ChildGroup {
    fn drop(&mut self) {
        // SAFETY: only the dedicated group allocated to this child is signalled.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

pub struct Output {
    pub success: bool,
    pub text: String,
}

/// Capture at most 4 MiB and stop the entire subprocess group on timeout.
pub fn run(root: &Path, argv: &[String], timeout: Duration) -> io::Result<Output> {
    let program = argv
        .first()
        .ok_or_else(|| io::Error::other("empty command"))?;
    let mut log = tempfile::tempfile()?;
    let mut command = Command::new(program);
    command
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .process_group(0);
    let mut child = ChildGroup(command.spawn()?);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout || log.metadata()?.len() > 4 * 1024 * 1024 {
            return Err(io::Error::other("command exceeded time or output limit"));
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
    Ok(Output {
        success: status.success(),
        text: String::from_utf8_lossy(&bytes).into_owned(),
    })
}
