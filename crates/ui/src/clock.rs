//! Times as this computer shows them: in its own timezone, on the 12- or
//! 24-hour clock the person chose for it.

use std::sync::OnceLock;

use chrono::{DateTime, Local, TimeZone, Utc};

/// Whether the person reads a 12-hour clock: what the system says, read
/// once.
fn hour12() -> bool {
    static HOUR12: OnceLock<bool> = OnceLock::new();
    *HOUR12.get_or_init(|| {
        let (force24, force12, locale) = system_clock();
        hour12_from(force24, force12, locale.as_deref())
    })
}

/// macOS keeps an explicit choice (`AppleICUForce24HourTime`,
/// `AppleICUForce12HourTime`) and, without one, follows the region of
/// `AppleLocale`; elsewhere the region of `LC_ALL`, `LC_TIME` or `LANG`.
fn system_clock() -> (bool, bool, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        let read = |key: &str| {
            std::process::Command::new("/usr/bin/defaults")
                .args(["read", "-g", key])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        };
        let set = |key: &str| read(key).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        (
            set("AppleICUForce24HourTime"),
            set("AppleICUForce12HourTime"),
            read("AppleLocale"),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let locale = ["LC_ALL", "LC_TIME", "LANG"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty());
        (false, false, locale)
    }
}

/// The clock a choice and a locale (`en_US`, `en_US.UTF-8`, `de-DE`) call
/// for: an explicit choice wins; otherwise the regions that write the
/// time on a 12-hour clock.
fn hour12_from(force24: bool, force12: bool, locale: Option<&str>) -> bool {
    if force24 {
        return false;
    }
    if force12 {
        return true;
    }
    let region = locale
        .and_then(|l| l.split(['.', '@']).next())
        .and_then(|l| l.split(['_', '-']).nth(1))
        .unwrap_or("");
    matches!(
        region.to_ascii_uppercase().as_str(),
        "US" | "CA" | "AU" | "NZ" | "IN" | "PH" | "PK" | "BD" | "EG" | "SA" | "MY" | "CO"
    )
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
    fn an_explicit_choice_wins_and_the_region_decides_otherwise() {
        assert!(hour12_from(false, false, Some("en_US")));
        assert!(hour12_from(false, false, Some("en_US.UTF-8")));
        assert!(!hour12_from(false, false, Some("de_DE")));
        assert!(!hour12_from(false, false, Some("en_GB")));
        assert!(hour12_from(false, false, Some("en-AU")));
        assert!(!hour12_from(true, false, Some("en_US")), "forced 24-hour");
        assert!(hour12_from(false, true, Some("fr_FR")), "forced 12-hour");
        assert!(!hour12_from(false, false, None));
        assert!(!hour12_from(false, false, Some("C")));
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
