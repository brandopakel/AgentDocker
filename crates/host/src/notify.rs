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

/// The one button a notification carries, where exactly one action
/// exists, beside the **Reply** field a message has: a question is
/// answered, a command prompt allowed, a stopped agent's terminal brought
/// forward. A notification with no button is opened by clicking it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Button {
    /// **Answer**: opens the window at the question the action names.
    Answer,
    /// **Allow once**: answers the command prompt the action names with
    /// `Allow`, through its daemon, without opening the window.
    AllowOnce,
    /// **Go to Terminal**: brings forward the terminal `agent` stopped
    /// in, through the window of the workspace at `home`.
    GoToTerminal {
        home: PathBuf,
        socket: PathBuf,
        agent: String,
    },
}

impl Button {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > ACTION_BYTES {
            return Err("notification button is too large".into());
        }
        let button: Self =
            serde_json::from_str(text).map_err(|_| "invalid notification button".to_owned())?;
        if !button.is_valid() {
            return Err("invalid notification button".into());
        }
        Ok(button)
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::Answer | Self::AllowOnce => true,
            Self::GoToTerminal {
                home,
                socket,
                agent,
            } => {
                home.is_absolute()
                    && socket.is_absolute()
                    && agentdocker_core::notification::valid_id(agent)
            }
        }
    }
}

/// What a notice is about, for asking whether it is already in front of
/// the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    /// A message or question from `agent`: in `project`'s chat, or in a
    /// `channel`.
    Message {
        agent: String,
        project: Option<String>,
        channel: Option<String>,
    },
    /// `agent` itself: it stopped for the person, or finished.
    Agent { agent: String },
}

impl From<&agentdocker_core::NotificationTarget> for Subject {
    fn from(target: &agentdocker_core::NotificationTarget) -> Self {
        Self::Message {
            agent: target.agent.to_string(),
            project: target.project.as_ref().map(|p| p.as_str().to_owned()),
            channel: target.channel.as_ref().map(|c| c.as_str().to_owned()),
        }
    }
}

/// Something the app has in front of the person that may already show
/// what a notice is about. The focused window is one; a surface a later
/// build adds (an open panel) is another implementation, consulted the
/// same way by [`shown`], never a further check inside a poster.
pub trait Surface {
    fn shows(&self, subject: &Subject, now: chrono::DateTime<chrono::Utc>) -> bool;
}

/// The surfaces of the workspace at `home`, as their records say: the
/// window's `viewing.json` today. A later surface's record is read here.
pub fn surfaces(home: &Path) -> Vec<Box<dyn Surface>> {
    Viewing::read(home)
        .into_iter()
        .map(|viewing| Box::new(viewing) as Box<dyn Surface>)
        .collect()
}

/// Whether a surface of the workspace at `home` has `subject` in front
/// of the person, so a notice about it would say what they see: the one
/// check a poster makes.
pub fn shown(home: &Path, subject: &Subject, now: chrono::DateTime<chrono::Utc>) -> bool {
    surfaces(home)
        .iter()
        .any(|surface| surface.shows(subject, now))
}

/// What the desktop window shows, so a notice about it is not also
/// posted: every agent app studied stays silent about the session on
/// screen. The window writes it as it changes (`viewing.json` in the
/// AgentDocker home); the poster reads it. A stale or missing record
/// claims nothing, and the notice is posted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewing {
    /// The window has the person's focus.
    pub focused: bool,
    /// The agent whose session, terminal or conversation is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The project whose chat is open, as its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// How long a viewing record counts without being written again; the
/// window rewrites it well within this while focused.
pub const VIEWING_FRESH: chrono::TimeDelta = chrono::TimeDelta::seconds(90);

pub fn viewing_path(home: &Path) -> PathBuf {
    home.join("viewing.json")
}

impl Viewing {
    pub fn read(home: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(viewing_path(home)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Whether a notice about `subject` would tell the person what is
    /// already in front of them: a message from the agent whose session
    /// is open, or in the chat that is open; and, while the window is in
    /// front at all, any agent that stopped or finished, since its
    /// Needs-you strip and badges say so on every screen.
    pub fn shows(&self, subject: &Subject, now: chrono::DateTime<chrono::Utc>) -> bool {
        let fresh = now >= self.at && now - self.at < VIEWING_FRESH;
        if !fresh || !self.focused {
            return false;
        }
        match subject {
            Subject::Message {
                agent,
                project,
                channel,
            } => {
                self.agent.as_deref() == Some(agent.as_str())
                    || (channel.is_none()
                        && self.chat.is_some()
                        && self.chat.as_deref() == project.as_deref())
            }
            Subject::Agent { .. } => true,
        }
    }
}

impl Surface for Viewing {
    fn shows(&self, subject: &Subject, now: chrono::DateTime<chrono::Utc>) -> bool {
        Viewing::shows(self, subject, now)
    }
}

/// Visible text is independent of activation metadata.
/// The tag a question's notification is posted under: its own, not its
/// asker's, so taking back an answered question can never remove a newer
/// one from the same agent, whoever posted it and in whatever order.
pub fn question_tag(message: &str) -> String {
    format!("question.{message}")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub action: Option<Action>,
    /// What a newer notification with the same tag replaces, so one
    /// subject keeps one notification rather than a backlog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The one button it carries, where one action exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<Button>,
}
impl Notification {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 2 * ACTION_BYTES + 4096 {
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
        if let Some(button) = &notice.button {
            Button::parse(&serde_json::to_string(button).map_err(|e| e.to_string())?)?;
        }
        Ok(notice)
    }

    /// What it is about, for the surfaces: the message its action names,
    /// or the agent whose terminal its button goes to.
    pub fn subject(&self) -> Option<Subject> {
        match (&self.action, &self.button) {
            (Some(action), _) => Some(Subject::from(&action.target)),
            (None, Some(Button::GoToTerminal { agent, .. })) => Some(Subject::Agent {
                agent: agent.clone(),
            }),
            (None, _) => None,
        }
    }

    /// The workspace it belongs to, from its action or its button.
    pub fn home(&self) -> Option<&Path> {
        match (&self.action, &self.button) {
            (Some(action), _) => Some(&action.home),
            (None, Some(Button::GoToTerminal { home, .. })) => Some(home),
            (None, _) => None,
        }
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
            tag: None,
            button: None,
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
            tag: None,
            button: None,
        };
        assert_eq!(
            post(&notice).unwrap_err(),
            "Cannot encode the notification destination."
        );
    }

    #[test]
    fn a_notice_about_what_is_on_screen_is_not_posted() {
        let now = chrono::Utc::now();
        let target = |agent: &str, project: Option<&str>, channel: bool| {
            Subject::from(&agentdocker_core::NotificationTarget {
                message: "0123456789abcdef".to_owned().into(),
                agent: agent.into(),
                project: project.map(Into::into),
                channel: channel.then(|| "0123456789abcdef".to_owned().into()),
            })
        };
        let viewing = Viewing {
            focused: true,
            agent: Some("worker".into()),
            chat: Some("project-a".into()),
            at: now,
        };
        assert!(viewing.shows(&target("worker", None, false), now));
        assert!(viewing.shows(&target("other", Some("project-a"), false), now));
        assert!(!viewing.shows(&target("other", Some("project-b"), false), now));
        assert!(
            !viewing.shows(&target("other", Some("project-a"), true), now),
            "a channel is not the project chat"
        );
        assert!(
            !Viewing {
                focused: false,
                ..viewing.clone()
            }
            .shows(&target("worker", None, false), now),
            "an unfocused window shows nothing"
        );
        assert!(
            !viewing.shows(&target("worker", None, false), now + VIEWING_FRESH),
            "a stale record claims nothing"
        );
        // An agent that stopped or finished is said by the window itself,
        // whichever screen is up, so a window in front shows every one.
        let stopped = Subject::Agent {
            agent: "other".into(),
        };
        assert!(viewing.shows(&stopped, now));
        assert!(
            !Viewing {
                focused: false,
                ..viewing.clone()
            }
            .shows(&stopped, now)
        );
        let surfaces: Vec<Box<dyn Surface>> = vec![Box::new(viewing)];
        assert!(surfaces.iter().any(|s| s.shows(&stopped, now)));
    }

    /// A notice's subject follows its action, else its terminal button;
    /// a button carries only what the receiver can trust.
    #[test]
    fn buttons_are_bounded_and_name_their_subject() {
        let terminal = Button::GoToTerminal {
            home: PathBuf::from("/fixture/home"),
            socket: PathBuf::from("/fixture/home/agentd.sock"),
            agent: "agent-1".into(),
        };
        let encoded = serde_json::to_string(&terminal).unwrap();
        assert_eq!(Button::parse(&encoded).unwrap(), terminal);
        for text in [
            r#"{"kind":"go_to_terminal","home":"relative","socket":"/s","agent":"a"}"#,
            r#"{"kind":"go_to_terminal","home":"/h","socket":"/s","agent":"../x"}"#,
            r#"{"kind":"open_anything"}"#,
            &"x".repeat(ACTION_BYTES + 1),
        ] {
            assert!(Button::parse(text).is_err(), "{text}");
        }
        assert_eq!(Button::parse(r#"{"kind":"answer"}"#).unwrap(), Button::Answer);
        assert_eq!(
            Button::parse(r#"{"kind":"allow_once"}"#).unwrap(),
            Button::AllowOnce
        );
        let notice = Notification {
            title: "Agent wants permission".into(),
            body: "It is waiting in its terminal.".into(),
            action: None,
            tag: Some("window.agent-1".into()),
            button: Some(terminal),
        };
        assert_eq!(
            Notification::parse(&serde_json::to_string(&notice).unwrap()).unwrap(),
            notice
        );
        assert_eq!(
            notice.subject(),
            Some(Subject::Agent {
                agent: "agent-1".into()
            })
        );
        assert_eq!(notice.home(), Some(Path::new("/fixture/home")));
        let bad = r#"{"title":"t","body":"b","button":{"kind":"go_to_terminal","home":"h","socket":"/s","agent":"a"}}"#;
        assert!(Notification::parse(bad).is_err());
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
