//! Post a desktop notification that carries our own icon.
//!
//! macOS attributes a notification to the bundle of the process that
//! posts it, and every way of overriding that is closed: the
//! `UserNotifications` framework refuses a spoofed bundle identifier,
//! which is why `terminal-notifier`'s `-sender` was withdrawn, and an
//! `osascript` notification belongs to Script Editor. So the only
//! process that can post a notification with the AgentDocker mark on it
//! is AgentDocker.
//!
//! The daemon has no bundle of its own, so it runs this: the app's own
//! executable, from inside the app's own bundle, in a mode that posts
//! one notification and exits. That is the whole trick.

/// How long to wait for the notification centre to accept the request.
///
/// This process exists only to post, so exiting before the framework has
/// taken the request would drop it. A second is far longer than the
/// handoff needs and short enough that a wedged notification daemon does
/// not hold up whatever asked.
#[cfg(target_os = "macos")]
const ACCEPT_WITHIN: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether this process is inside an application bundle.
///
/// Asked before anything else here, because
/// `UNUserNotificationCenter.currentNotificationCenter` does not return
/// nil when it is not — it raises `NSInternalInconsistencyException`,
/// "bundleProxyForCurrentProcess is nil". An Objective-C exception is
/// not a Rust panic, so the `catch_unwind` that used to stand here
/// caught nothing and the process aborted before it had drawn a frame:
/// `cargo run`, `target/release/agentdocker-ui`, and the bare
/// `agentdocker-ui` a Homebrew *formula* installs all died on startup,
/// and the graphical acceptance run is what found it.
///
/// A bundle is exactly what has an identifier. A loose executable's
/// `mainBundle` is its own directory and has none.
#[cfg(target_os = "macos")]
pub(crate) fn in_a_bundle() -> bool {
    objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .is_some()
}

/// Ask, once, for permission to notify.
///
/// Called from the running window rather than from the one-shot poster,
/// because macOS will not register a notification client that has no
/// run loop and no foreground presence — a process that starts, asks and
/// exits is told "notifications are not allowed for this application"
/// and gets no prompt. The window has both, so the prompt appears there
/// and the answer is remembered for the bundle.
#[cfg(target_os = "macos")]
pub fn request_permission() {
    use block2::RcBlock;
    use objc2_foundation::NSError;
    use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};

    if !in_a_bundle() {
        return; // nothing to register a notification client against
    }
    let centre = UNUserNotificationCenter::currentNotificationCenter();
    let handler = RcBlock::new(|granted: objc2::runtime::Bool, error: *mut NSError| {
        if let Some(reason) = describe(error) {
            eprintln!("notifications unavailable: {reason}");
        } else if !granted.as_bool() {
            eprintln!("notifications declined; questions will still queue in the app");
        }
    });
    centre.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &handler,
    );
}

#[cfg(not(target_os = "macos"))]
pub fn request_permission() {}

pub fn post(title: &str, body: &str) -> Result<(), String> {
    post_notification(&agentdocker_host::notify::Notification {
        title: title.into(),
        body: body.into(),
        action: None,
    })
}

#[cfg(target_os = "macos")]
pub fn post_notification(notice: &agentdocker_host::notify::Notification) -> Result<(), String> {
    use block2::RcBlock;
    use objc2_foundation::{NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNNotificationRequest, UNUserNotificationCenter,
    };
    use std::sync::{Arc, Mutex};

    // Refused when the executable is not inside an application bundle,
    // which is exactly when the icon would have been wrong anyway.
    if !in_a_bundle() {
        return Err("not running inside an application bundle".to_owned());
    }
    let centre = UNUserNotificationCenter::currentNotificationCenter();

    // Asking is idempotent and answered from the user's earlier choice
    // after the first time. A refusal is not an error here: the caller
    // has already decided a notification is warranted, and whether one
    // appears is the person's business, not ours.
    let asked: Outcome = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    let done = asked.clone();
    let handler = RcBlock::new(move |granted: objc2::runtime::Bool, error: *mut NSError| {
        let said = describe(error).unwrap_or_else(|| {
            if granted.as_bool() {
                String::new()
            } else {
                "authorisation was refused".to_owned()
            }
        });
        *done.0.lock().unwrap() = Some(said);
        done.1.notify_all();
    });
    centre.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &handler,
    );
    match wait_for(&asked, ACCEPT_WITHIN) {
        Some(reason) if reason.is_empty() => {}
        Some(reason) => return Err(format!("notifications are not permitted: {reason}")),
        None => return Err("notification authorization did not answer".into()),
    }

    let content = notification_content(notice)?;
    let id = NSString::from_str(&format!(
        "dev.agentdocker.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);

    let accepted: Outcome = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    let settled = accepted.clone();
    let handler = RcBlock::new(move |error: *mut NSError| {
        *settled.0.lock().unwrap() = Some(describe(error).unwrap_or_default());
        settled.1.notify_all();
    });
    centre.addNotificationRequest_withCompletionHandler(&request, Some(&handler));
    match wait_for(&accepted, ACCEPT_WITHIN) {
        Some(reason) if reason.is_empty() => Ok(()),
        Some(reason) => Err(reason),
        None => Err("the notification centre did not answer".to_owned()),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn notification_content(
    notice: &agentdocker_host::notify::Notification,
) -> Result<objc2::rc::Retained<objc2_user_notifications::UNMutableNotificationContent>, String> {
    use objc2_foundation::NSString;
    let content = objc2_user_notifications::UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&notice.title));
    content.setBody(&NSString::from_str(&notice.body));
    if let Some(action) = &notice.action {
        let encoded = serde_json::to_string(action).map_err(|e| e.to_string())?;
        agentdocker_host::notify::Action::parse(&encoded)?;
        let key = NSString::from_str("agentdocker.action");
        let value = NSString::from_str(&encoded);
        let info = objc2_foundation::NSDictionary::from_slices(&[&*key], &[&*value]);
        // SAFETY: Erase only the dictionary's static generic parameters.
        // The retained Objective-C object and its immutable strings are unchanged.
        let info =
            unsafe { objc2::rc::Retained::cast_unchecked::<objc2_foundation::NSDictionary>(info) };
        unsafe { content.setUserInfo(&info) };
    }
    Ok(content)
}

/// The shared slot a completion handler drops its verdict into: `None`
/// until it fires, then `Some("")` for success or `Some(reason)`.
#[cfg(target_os = "macos")]
type Outcome = std::sync::Arc<(std::sync::Mutex<Option<String>>, std::sync::Condvar)>;

/// An `NSError` as something a person can read, or `None` for no error.
#[cfg(target_os = "macos")]
fn describe(error: *mut objc2_foundation::NSError) -> Option<String> {
    if error.is_null() {
        return None;
    }
    // SAFETY: a non-null NSError from a framework completion handler,
    // borrowed only for as long as this call.
    let error = unsafe { &*error };
    Some(format!(
        "{} ({})",
        error.localizedDescription(),
        error.code()
    ))
}

/// Wait for a completion handler, pumping the run loop while we do.
///
/// The handlers are delivered on the main queue, so a thread that simply
/// blocks would wait for something that cannot arrive until it stops
/// waiting. Draining the run loop is what lets them land.
#[cfg(target_os = "macos")]
fn wait_for(slot: &Outcome, within: std::time::Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if let Some(said) = slot.0.lock().unwrap().clone() {
            return Some(said);
        }
        pump_run_loop(std::time::Duration::from_millis(10));
    }
    slot.0.lock().unwrap().clone()
}

#[cfg(target_os = "macos")]
fn pump_run_loop(for_: std::time::Duration) {
    unsafe extern "C" {
        fn CFRunLoopRunInMode(
            mode: *const std::ffi::c_void,
            seconds: f64,
            return_after_source_handled: u8,
        ) -> i32;
        static kCFRunLoopDefaultMode: *const std::ffi::c_void;
    }
    // SAFETY: the mode constant is a framework global and the call only
    // runs the current thread's run loop for a bounded time.
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, for_.as_secs_f64(), 0);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn post_notification(_notice: &agentdocker_host::notify::Notification) -> Result<(), String> {
    Err("posting from the app is a macOS arrangement; \
         other platforms let the daemon post directly"
        .to_owned())
}

#[cfg(test)]
mod tests {
    /// Neither of these aborts when there is no bundle around them.
    ///
    /// The test binary is a loose executable, which is the case that
    /// used to take the whole process down: `currentNotificationCenter`
    /// raises an Objective-C exception rather than returning nil, and
    /// `catch_unwind` does not catch those. So this test is the case —
    /// if the guard goes, this does not fail, it aborts.
    #[test]
    fn asking_outside_a_bundle_is_refused_rather_than_fatal() {
        super::request_permission();
        let refused = super::post_notification(&agentdocker_host::notify::Notification {
            title: "AgentDocker".into(),
            body: "test".into(),
            action: None,
        })
        .unwrap_err();
        assert!(!refused.is_empty(), "a refusal says why");
        #[cfg(target_os = "macos")]
        assert!(!super::in_a_bundle(), "the test binary is not a bundle");
    }
}
