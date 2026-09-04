//! Process identity beyond the pid.
//!
//! A pid is recycled once its process exits, so "pid still exists" is not
//! proof that the agent which reported it is still running — least of all
//! after a reboot. The start time of the process is the cheap identity the
//! OS gives us; the daemon records it at registration and compares it
//! during liveness checks.

use chrono::{DateTime, Utc};

/// When the process with this pid started, if the platform can tell us.
pub fn start_time(pid: u32) -> Option<DateTime<Utc>> {
    imp::start_time(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use chrono::{DateTime, Utc};

    pub fn start_time(pid: u32) -> Option<DateTime<Utc>> {
        let pid = i32::try_from(pid).ok()?;
        // SAFETY: proc_bsdinfo is plain old data, so all-zero is a valid value.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: the buffer is a correctly sized proc_bsdinfo and `size`
        // is its length, so the kernel writes within bounds.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast::<libc::c_void>(),
                size,
            )
        };
        if written != size {
            return None;
        }
        let secs = i64::try_from(info.pbi_start_tvsec).ok()?;
        let nanos = u32::try_from(info.pbi_start_tvusec)
            .ok()?
            .checked_mul(1000)?;
        DateTime::from_timestamp(secs, nanos)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use chrono::{DateTime, Duration, Utc};

    pub fn start_time(pid: u32) -> Option<DateTime<Utc>> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Everything after the parenthesised command name; `starttime` is
        // field 22 overall, so the 20th after the closing parenthesis.
        let after_comm = stat.rsplit_once(')')?.1;
        let ticks: i64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
        // SAFETY: sysconf has no preconditions.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz <= 0 {
            return None;
        }
        let since_boot = Duration::milliseconds(ticks.checked_mul(1000)? / hz);
        Some(boot_time()? + since_boot)
    }

    fn boot_time() -> Option<DateTime<Utc>> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let secs: i64 = stat
            .lines()
            .find_map(|line| line.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        DateTime::from_timestamp(secs, 0)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use chrono::{DateTime, Utc};

    pub fn start_time(_pid: u32) -> Option<DateTime<Utc>> {
        None
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn own_process_has_a_recent_start_time() {
        let started = start_time(std::process::id()).expect("readable on this platform");
        let age = Utc::now() - started;
        assert!(
            age.num_seconds() >= 0,
            "start time in the future: {started}"
        );
        assert!(
            age.num_hours() < 24 * 365,
            "implausible start time: {started}"
        );
    }

    #[test]
    fn dead_process_has_none() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(start_time(pid).is_none());
    }
}
