//! The dock icon's badge: how many questions are waiting on the person.
//!
//! Every comparable app marks its dock icon when an agent is waiting on a
//! person; a badge is what reaches them while the window is behind
//! another one. It counts questions only, the same rows Needs you holds,
//! so it never shows a number nobody can clear.

/// The badge's words for `count` waiting questions: nothing at zero.
pub(crate) fn label(count: usize) -> Option<String> {
    match count {
        0 => None,
        1..=99 => Some(count.to_string()),
        _ => Some("99+".to_owned()),
    }
}

/// Show `count` on the dock icon. AppKit's dock tile belongs to the main
/// thread, where the app's update runs; called anywhere else it does
/// nothing rather than touch it from the wrong thread.
#[cfg(target_os = "macos")]
pub(crate) fn badge(count: usize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let text = label(count).map(|text| NSString::from_str(&text));
    NSApplication::sharedApplication(main)
        .dockTile()
        .setBadgeLabel(text.as_deref());
}

/// Other platforms have no dock badge here.
#[cfg(not(target_os = "macos"))]
pub(crate) fn badge(_count: usize) {}

#[cfg(test)]
mod tests {
    #[test]
    fn the_badge_says_nothing_at_zero_and_stays_short() {
        assert_eq!(super::label(0), None);
        assert_eq!(super::label(1).as_deref(), Some("1"));
        assert_eq!(super::label(99).as_deref(), Some("99"));
        assert_eq!(super::label(100).as_deref(), Some("99+"));
    }
}
