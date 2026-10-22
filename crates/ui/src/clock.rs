//! Times as this computer shows them: in its own timezone, on the 12- or
//! 24-hour clock the person chose for it.

use std::sync::OnceLock;

use chrono::{DateTime, Local, TimeZone, Utc};

/// Whether the person reads a 12-hour clock: what the system's own time
/// format says, asked once. Unknown means the 24-hour clock.
fn hour12() -> bool {
    static HOUR12: OnceLock<bool> = OnceLock::new();
    *HOUR12.get_or_init(|| system_pattern().is_some_and(|p| hour12_from(&p)))
}

/// Ask for the clock early, off the window's thread, so the first message
/// drawn does not wait for the system to answer.
pub fn warm() {
    std::thread::spawn(hour12);
}

/// The system's time pattern for this person, as its own formatter writes
/// it: Foundation's for the current locale on macOS (which honours the
/// 12/24-hour choice in System Settings), the C library's `t_fmt` for the
/// time locale elsewhere on Unix, and the regional `sTimeFormat` on
/// Windows.
fn system_pattern() -> Option<String> {
    let run = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|p| !p.is_empty())
    };
    #[cfg(target_os = "macos")]
    {
        run(
            "/usr/bin/osascript",
            &[
                "-l",
                "JavaScript",
                "-e",
                "ObjC.import('Foundation'); $.NSDateFormatter.dateFormatFromTemplateOptionsLocale('jmm', 0, $.NSLocale.currentLocale).js",
            ],
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // `t_fmt="%r"`: the strftime pattern, quoted.
        run("locale", &["-k", "t_fmt"]).and_then(|line| {
            line.split_once('=')
                .map(|(_, value)| value.trim_matches('"').to_owned())
        })
    }
    #[cfg(windows)]
    {
        // `    sTimeFormat    REG_SZ    h:mm:ss tt`
        run(
            "reg",
            &[
                "query",
                r"HKCU\Control Panel\International",
                "/v",
                "sTimeFormat",
            ],
        )
        .and_then(|out| {
            out.lines()
                .find_map(|line| line.split_once("REG_SZ").map(|(_, v)| v.trim().to_owned()))
        })
    }
}

/// Whether a system time pattern is a 12-hour clock: Unicode patterns
/// (macOS) and Windows patterns write a 12-hour hour as `h` or `K`, a
/// 24-hour one as `H` or `k`; strftime (`t_fmt`) writes it as `%I`, `%l`
/// or `%r`, and `%H`, `%k` or `%T` otherwise.
fn hour12_from(pattern: &str) -> bool {
    if pattern.contains('%') {
        return ["%I", "%l", "%r"].iter().any(|p| pattern.contains(p));
    }
    // Quoted literals ('at') are not part of the hour.
    let mut quoted = false;
    for c in pattern.chars() {
        match c {
            '\'' => quoted = !quoted,
            'h' | 'K' if !quoted => return true,
            'H' | 'k' if !quoted => return false,
            _ => {}
        }
    }
    false
}

fn clock_of<Tz: TimeZone>(at: &DateTime<Tz>, hour12: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if hour12 {
        at.format("%-I:%M %p").to_string()
    } else {
        at.format("%H:%M").to_string()
    }
}

/// The time of day `at` happened, here: `9:18 PM` or `21:18`.
pub fn time(at: DateTime<Utc>) -> String {
    clock_of(&at.with_timezone(&Local), hour12())
}

/// The day and time `at` happened, here: `Tue 21 Oct, 9:18 PM`.
pub fn moment(at: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    format!(
        "{}, {}",
        local.format("%a %-d %b"),
        clock_of(&local, hour12())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn the_hour_is_read_from_the_systems_own_pattern() {
        // Foundation's patterns for en_US, zh_Hant_TW, en_SG, fr_CA,
        // en_GB and de_DE.
        for (pattern, twelve) in [
            ("h:mm a", true),
            ("ah:mm", true),
            ("HH:mm", false),
            ("H:mm", false),
            ("K:mm a", true),
            ("'at' HH:mm", false),
        ] {
            assert_eq!(hour12_from(pattern), twelve, "{pattern}");
        }
        // strftime patterns from `locale -k t_fmt`.
        assert!(hour12_from("%r"));
        assert!(hour12_from("%I:%M:%S %p"));
        assert!(!hour12_from("%H:%M:%S"));
        assert!(!hour12_from("%T"));
        // Windows sTimeFormat.
        assert!(hour12_from("h:mm:ss tt"));
        assert!(!hour12_from("HH:mm:ss"));
    }

    #[test]
    fn a_time_is_read_in_the_zone_it_is_shown_in() {
        let at: DateTime<Utc> = "2026-10-22T04:18:00Z".parse().unwrap();
        let pacific = at.with_timezone(&FixedOffset::west_opt(7 * 3600).unwrap());
        assert_eq!(clock_of(&pacific, true), "9:18 PM");
        assert_eq!(clock_of(&pacific, false), "21:18");
        let berlin = at.with_timezone(&FixedOffset::east_opt(2 * 3600).unwrap());
        assert_eq!(clock_of(&berlin, false), "06:18");
    }
}
