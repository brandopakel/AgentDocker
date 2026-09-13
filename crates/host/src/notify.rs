//! Desktop notifications with stable destinations and bounded native posting.
//! macOS notifications must originate from our app bundle. AppleScript notices
//! open Script Editor when clicked, so they are deliberately not a fallback.
//! Posting failure leaves messages in the inbox and reports a bounded reason.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const ACTION_BYTES: usize = 16 * 1024;

/// Distinguish custom daemon endpoints as well as independent homes.
pub fn instance_key(home: &Path, socket: &Path) -> String {
    let mut bytes = home.as_os_str().as_encoded_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(socket.as_os_str().as_encoded_bytes());
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, &bytes)
        .simple()
        .to_string()
}

/// The precise local daemon and destination that produced a notification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub home: PathBuf,
    pub socket: PathBuf,
    pub target: agentdocker_core::NotificationTarget,
}
impl Action {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > ACTION_BYTES {
            return Err("notification destination is too large".into());
        }
        let action: Self = serde_json::from_str(text)
            .map_err(|_| "invalid notification destination".to_owned())?;
        if !action.home.is_absolute() || !action.socket.is_absolute() || !action.target.is_valid() {
            return Err("invalid notification destination".into());
        }
        Ok(action)
    }
}

/// Visible text is independent of activation metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub action: Option<Action>,
}
impl Notification {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > ACTION_BYTES + 4096 {
            return Err("notification is too large".into());
        }
        let notice: Self =
            serde_json::from_str(text).map_err(|_| "invalid notification".to_owned())?;
        if notice.title.len() > 1024 || notice.body.len() > 4096 {
            return Err("notification text is too large".into());
        }
        if let Some(action) = &notice.action {
            Action::parse(&serde_json::to_string(action).map_err(|e| e.to_string())?)?;
        }
        Ok(notice)
    }
}

/// Wait only for posting acceptance, never for dismissal or a human response.
/// No private text is included in the returned failure reason.
pub fn post(notification: &Notification) -> Result<(), String> {
    let commands = candidates(notification)?;
    if commands.is_empty() {
        return Err("AgentDocker notification app is unavailable".into());
    }
    for argv in commands {
        match crate::command::run(Path::new("/"), &argv, Duration::from_secs(5)) {
            Ok(result) if result.success => return Ok(()),
            // Do not copy arbitrary child output: older binaries can echo
            // unknown arguments containing the private notification payload.
            _ => continue,
        }
    }
    Err("Native notification posting failed; check app notification permission and signing. The message remains in Inbox.".into())
}

fn candidates(notification: &Notification) -> Result<Vec<Vec<String>>, String> {
    if cfg!(target_os = "macos") {
        let encoded = serde_json::to_string(notification)
            .map_err(|_| "Cannot encode the notification destination.".to_owned())?;
        Ok(desktop_app()
            .into_iter()
            .map(|app| {
                vec![
                    app.to_string_lossy().into_owned(),
                    "--notify-json".into(),
                    encoded.clone(),
                ]
            })
            .collect())
    } else {
        Ok(vec![vec![
            "notify-send".into(),
            "--app-name=AgentDocker".into(),
            "--".into(),
            notification.title.clone(),
            notification.body.clone(),
        ]])
    }
}

/// Prefer the poster bundled with the running daemon over an older installation.
fn desktop_app() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
        && directory.file_name().is_some_and(|name| name == "MacOS")
        && directory
            .parent()
            .is_some_and(|p| p.file_name().is_some_and(|name| name == "Contents"))
    {
        let sibling = directory.join("agentdocker-ui");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    let roots = std::env::var_os("HOME")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::iter::once(PathBuf::from("/")));
    // The managed store first: the Applications entry is a launcher bundle
    // whose executable is a script, and earlier releases left a symlink.
    roots
        .flat_map(|root| {
            [
                root.join(".local/share/agentdocker/desktop/current/payload/Contents/MacOS/agentdocker-ui"),
                root.join("Applications/AgentDocker.app/Contents/MacOS/agentdocker-ui"),
            ]
        })
        .find(|inner| inner.is_file())
}

/// Trim a message to something a notification can show, on a word
/// boundary where there is one.
pub fn summarise(text: &str, limit: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= limit {
        return text;
    }
    let keep = limit.saturating_sub(1);
    let cut: String = text.chars().take(keep).collect();
    // A cut that already fell between two words needs no trimming back.
    let head = if text.chars().nth(keep) == Some(' ') {
        cut.as_str()
    } else {
        match cut.rsplit_once(' ') {
            // Trimming back to a boundary is only worth it when a useful
            // amount of the text survives; otherwise cut mid-word.
            Some((head, _)) if head.chars().count() >= limit / 2 => head,
            _ => cut.as_str(),
        }
    };
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_destinations_cannot_select_another_state_path() {
        for text in [
            "{}",
            r#"{"home":"relative","socket":"relative","target":null}"#,
            &"x".repeat(ACTION_BYTES + 1),
        ] {
            assert!(Action::parse(text).is_err());
        }
    }

    #[test]
    fn notification_text_stays_data_and_macos_never_invokes_script_editor() {
        let notification = Notification {
            title: "--action=bad".into(),
            body: "quotes \" and $(data)".into(),
            action: None,
        };
        let encoded = serde_json::to_string(&notification).unwrap();
        assert_eq!(Notification::parse(&encoded).unwrap(), notification);
        let candidates = candidates(&notification).unwrap();
        assert!(
            candidates
                .iter()
                .all(|argv| !argv.iter().any(|v| v == "osascript" || v == "-e"))
        );
        for argv in candidates {
            if cfg!(target_os = "macos") {
                assert_eq!(argv[1], "--notify-json");
                assert_eq!(Notification::parse(&argv[2]).unwrap(), notification);
            } else {
                assert_eq!(argv[2], "--");
                assert_eq!(argv[3], notification.title);
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn non_utf8_origin_returns_a_bounded_error_without_posting_or_panicking() {
        use std::os::unix::ffi::OsStringExt;
        let notice = Notification {
            title: "private title".into(),
            body: "private body".into(),
            action: Some(Action {
                home: PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/private-\xff".to_vec())),
                socket: PathBuf::from("/tmp/agentd.sock"),
                target: agentdocker_core::NotificationTarget {
                    message: "message".to_owned().into(),
                    agent: "agent".into(),
                    project: None,
                    channel: None,
                },
            }),
        };
        assert_eq!(
            post(&notice).unwrap_err(),
            "Cannot encode the notification destination."
        );
    }

    #[test]
    fn summarise_trims_on_a_word_boundary() {
        assert_eq!(summarise("short enough", 40), "short enough");
        assert_eq!(
            summarise("the quick brown fox jumps over it", 20),
            "the quick brown fox…"
        );
        assert_eq!(summarise("aaaaaaaaaaaaaaaaaaaa b", 10), "aaaaaaaaa…");
        assert_eq!(summarise("two\n\nlines", 40), "two lines");
    }
}
