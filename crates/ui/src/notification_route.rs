//! One bounded activation queue for native notification responses and CLI opens.
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

#[cfg(any(target_os = "macos", test))]
use agentdocker_core::{Request, Response};
use agentdocker_host::notify::Action;
use serde::{Deserialize, Serialize};
pub mod instance;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    content = "action",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Activation {
    Focus,
    /// Older notifications have no destination and open Inbox.
    Inbox,
    Open(Action),
    /// A reply typed into a notification that did not go: the words
    /// come back to the conversation as its draft, with the reason.
    /// `certain` says the daemon refused it (or nothing was sent);
    /// otherwise the outcome is unknown and the history decides whether
    /// to send again.
    ReplyFailed {
        action: Action,
        text: String,
        reason: String,
        certain: bool,
    },
}

#[derive(Default)]
struct Queue(VecDeque<Activation>);
impl Queue {
    fn push(&mut self, activation: Activation) -> Result<(), String> {
        if self.0.back() == Some(&activation) {
            return Ok(());
        }
        if self.0.len() >= 32 {
            return Err("notification navigation is busy; open Inbox".into());
        }
        self.0.push_back(activation);
        Ok(())
    }
}
fn queue() -> &'static Mutex<Queue> {
    static QUEUE: OnceLock<Mutex<Queue>> = OnceLock::new();
    QUEUE.get_or_init(Default::default)
}

#[cfg(target_os = "macos")]
pub fn receive(action: Option<Action>) -> Result<(), String> {
    if let Some(action) = &action {
        let home = agentdocker_host::dirs::home();
        if action.home != home || action.socket != agentdocker_host::dirs::socket_path(&home) {
            return instance::open_origin(action);
        }
    }
    enqueue(action.map_or(Activation::Inbox, Activation::Open))
}

pub fn enqueue(activation: Activation) -> Result<(), String> {
    if let Activation::Open(action) | Activation::ReplyFailed { action, .. } = &activation {
        Action::parse(&serde_json::to_string(action).map_err(|e| e.to_string())?)?;
    }
    if let Activation::ReplyFailed { text, reason, .. } = &activation
        && (text.chars().count() > RECOVERY_CHARS || reason.chars().count() > RECOVERY_REASON_CHARS)
    {
        return Err("the reply or its reason exceeds recovery limits".into());
    }
    queue()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(activation)?;
    crate::wake::Wake::default().request_repaint();
    Ok(())
}

/// The most a reply from a notification carries: the field is a line
/// or two, and a message this size is refused by the daemon anyway.
#[cfg(any(target_os = "macos", test))]
pub const REPLY_CHARS: usize = 4_000;
/// The most of a failed reply that comes back to the app: what a draft
/// holds. Longer is cut there and said to be.
pub const RECOVERY_CHARS: usize = crate::drafts::MAX_TEXT_CHARS;
pub const RECOVERY_REASON_CHARS: usize = 512;

/// Why a reply did not go, and whether that is known for sure: the
/// daemon refused it (or nothing was ever sent), or the connection went
/// before an answer came and the daemon may have committed it — in
/// which case the history, not a resend, is the next step.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub reason: String,
    pub certain: bool,
}

#[cfg(any(target_os = "macos", test))]
impl Failure {
    fn certain(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            certain: true,
        }
    }
    fn unknown(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            certain: false,
        }
    }
}

/// The words of a reply as they will be sent: trimmed, `None` when there
/// are none (the field's Send with nothing typed is nothing), refused
/// when longer than a notification's field should carry.
#[cfg(any(target_os = "macos", test))]
pub fn reply_text(text: &str) -> Result<Option<String>, Failure> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > REPLY_CHARS {
        return Err(Failure::certain(
            "the reply is longer than a notification carries; answer in the app",
        ));
    }
    Ok(Some(text.to_owned()))
}

/// Where a reply to a message goes: where the message went. A message
/// to the project's everyone or to a channel is answered there, one to
/// the person is answered to whoever wrote it — never inferred from
/// the notification, whose project is set for a direct message too. A
/// question is answered to whoever asked it wherever it was asked: the
/// daemon closes a question only by a reply addressed to its asker, and
/// an answer is not for everyone.
#[cfg(any(target_os = "macos", test))]
pub fn reply_destination(original: &agentdocker_core::Envelope) -> Result<String, Failure> {
    use agentdocker_core::Destination;
    if original.kind == "question" {
        return if original.from == agentdocker_core::conversation::DAEMON {
            Err(Failure::certain("a notice from AgentDocker has no reply"))
        } else {
            Ok(original.from.clone())
        };
    }
    Ok(match &original.to {
        Destination::Project(project) => format!("project:{}", project.as_str()),
        Destination::Channel(channel) => format!("channel:{channel}"),
        Destination::Broadcast => "all".to_owned(),
        Destination::Agent(_) if original.from == agentdocker_core::conversation::DAEMON => {
            return Err(Failure::certain("a notice from AgentDocker has no reply"));
        }
        Destination::Agent(_) => original.from.clone(),
        Destination::Topic(_) => return Err(Failure::certain("a topic post has no reply")),
    })
}

/// The reply as the person's, to where the original went, as a reply to
/// it — so an answer to a question closes it the way the composer's
/// would.
#[cfg(any(target_os = "macos", test))]
pub fn reply_request(to: String, message: &agentdocker_core::MessageId, text: &str) -> Request {
    Request::Send {
        from: agentdocker_core::HUMAN.into(),
        to,
        kind: "chat".into(),
        payload: serde_json::json!({ "text": text }),
        reply_to: Some(message.clone()),
        links: Vec::new(),
    }
}

/// Send a reply through the daemon a notification names: read the
/// original from its archive to learn where it went, then send, and
/// take only `sent` as success. A refusal from the daemon is certain; a
/// connection that fails before an answer is not, since the daemon may
/// have committed the message.
#[cfg(any(target_os = "macos", test))]
pub fn deliver(client: &crate::client::Client, action: &Action, text: &str) -> Result<(), Failure> {
    let original = match client.call(&Request::Thread {
        message: action.target.message.clone(),
        after_seq: None,
        limit: 1,
    }) {
        Ok(Response::Thread { root, .. }) if root.envelope.id == action.target.message => {
            root.envelope
        }
        Ok(_) => {
            return Err(Failure::certain(
                "the daemon did not answer with the message",
            ));
        }
        Err(error) => {
            return Err(Failure::certain(format!(
                "the message could not be read: {error}"
            )));
        }
    };
    let to = reply_destination(&original)?;
    match client.call(&reply_request(to, &action.target.message, text)) {
        Ok(Response::Sent { .. }) => Ok(()),
        Ok(_) => Err(Failure::unknown(
            "the daemon answered with something other than sent",
        )),
        Err(error) if error.downcast_ref::<crate::client::RemoteError>().is_some() => {
            Err(Failure::certain(error.to_string()))
        }
        Err(error) => Err(Failure::unknown(error.to_string())),
    }
}

/// A reply typed into a notification, sent through the daemon it came
/// from, off the thread the notification centre called on. The window
/// is not asked to open: the person answered where they were. A reply
/// that did not go is said where the person is looking — a notification
/// — and comes back to its workspace's window as the conversation's
/// draft with the reason, within the bounds [`report_failure`] states.
#[cfg(target_os = "macos")]
pub fn reply(action: Action, text: String) -> Result<(), String> {
    let text = match reply_text(&text) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(()),
        Err(failure) => {
            report_failure(action, text, failure);
            return Ok(());
        }
    };
    start_reply(
        action,
        text,
        |action, text| {
            std::thread::Builder::new()
                .name("notification-reply".into())
                .spawn(move || {
                    let client =
                        crate::client::Client::at(action.home.clone(), action.socket.clone());
                    if let Err(failure) = deliver(&client, &action, &text) {
                        report_failure(action, text, failure);
                    }
                })
                .map(|_| ())
        },
        report_failure,
    );
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn start_reply(
    action: Action,
    text: String,
    start: impl FnOnce(Action, String) -> std::io::Result<()>,
    recover: impl FnOnce(Action, String, Failure),
) {
    let recovery_action = action.clone();
    let recovery_text = text.clone();
    if let Err(error) = start(action, text) {
        recover(
            recovery_action,
            recovery_text,
            Failure::certain(format!("reply delivery could not start: {error}")),
        );
    }
}

/// Say a reply did not go: the words go back to the app that owns the
/// notification's workspace — this one's queue, or another running
/// instance's — to become the conversation's draft, and a notification
/// says why, and whether the app has the words; when no app could take
/// them, the notification carries what fits. Nothing is claimed that did
/// not happen.
#[cfg(target_os = "macos")]
fn report_failure(action: Action, text: String, failure: Failure) {
    eprintln!("reply from notification not sent: {}", failure.reason);
    let (text, reason) = if text.chars().count() > RECOVERY_CHARS {
        (
            text.chars().take(RECOVERY_CHARS).collect::<String>(),
            format!(
                "{} (the reply was cut to what a draft holds)",
                failure.reason
            ),
        )
    } else {
        (text, failure.reason.clone())
    };
    let ours = action.home == agentdocker_host::dirs::home()
        && action.socket == agentdocker_host::dirs::socket_path(&action.home);
    let (home, socket) = (action.home.clone(), action.socket.clone());
    let activation = Activation::ReplyFailed {
        action,
        text: text.clone(),
        reason: reason.chars().take(RECOVERY_REASON_CHARS).collect(),
        certain: failure.certain,
    };
    let kept = if ours {
        enqueue(activation).map_err(|e| e.to_string())
    } else {
        instance::forward(&home, &socket, &activation).map_err(|e| e.to_string())
    };
    // Handed to the app is not yet held by it: the window may be full of
    // kept replies or still opening, and says what it did.
    let where_it_is = match &kept {
        Ok(()) => "Open the app to recover your reply.".to_owned(),
        Err(reason) => {
            eprintln!("reply from notification could not come back to the app: {reason}");
            format!(
                "The app could not take your reply. You wrote: {}",
                excerpt(&text)
            )
        }
    };
    let (title, body) = if failure.certain {
        (
            "Reply not sent",
            format!("{}. {where_it_is}", excerpt(&failure.reason)),
        )
    } else {
        (
            "Reply may not have been sent",
            format!(
                "{}. Check the conversation's history before sending again. {where_it_is}",
                excerpt(&failure.reason)
            ),
        )
    };
    let _ = crate::notify::post(title, &body);
}

/// The start of what was typed, for a notification's body.
#[cfg(target_os = "macos")]
fn excerpt(text: &str) -> String {
    let mut short: String = text.chars().take(200).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    short
}

pub fn take() -> Vec<Activation> {
    queue()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .0
        .drain(..)
        .collect()
}

/// macOS Hide affects the application, independently of window minimization.
/// Call from explicit activation handling before asking Iced to focus a window.
pub fn unhide_application() {
    #[cfg(target_os = "macos")]
    {
        let _ = objc2_app_kit::NSRunningApplication::currentApplication().unhide();
    }
}

#[cfg(target_os = "macos")]
mod native {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{ClassType, define_class, msg_send};
    use objc2_foundation::{NSObject, NSObjectProtocol, NSString, ns_string};
    use objc2_user_notifications::{
        UNNotification, UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
        UNNotificationResponse, UNTextInputNotificationResponse, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };

    define_class!(
        // SAFETY: NSObject has no additional subclass requirements. Methods
        // share only the synchronized Rust activation queue; no UI state crosses threads.
        #[unsafe(super = NSObject)]
        pub struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion: &block2::DynBlock<dyn Fn()>,
            ) {
                // SAFETY: This is an immutable framework constant. Dismissal
                // and unfamiliar actions must not activate a window.
                if *response.actionIdentifier() == *unsafe { UNNotificationDefaultActionIdentifier }
                {
                    let content = response.notification().request().content();
                    if let Err(reason) = receive_content(&content) {
                        eprintln!("{reason}");
                    }
                } else if response.actionIdentifier().to_string() == crate::notify::REPLY_ACTION
                    && let Some(typed) = response.downcast_ref::<UNTextInputNotificationResponse>()
                {
                    // A reply typed into the notification: sent, not opened.
                    // A notification that cannot say where it came from
                    // cannot take a reply; the person is told, with their
                    // words, where they are looking.
                    let text = typed.userText().to_string();
                    let content = response.notification().request().content();
                    let outcome = match decode_content(&content) {
                        Ok(Some(action)) => super::reply(action, text.clone()),
                        Ok(None) => Err("this notification has no conversation to reply to".into()),
                        Err(reason) => Err(reason),
                    };
                    if let Err(reason) = outcome {
                        eprintln!("reply from notification: {reason}");
                        let _ = crate::notify::post(
                            "Reply not sent",
                            &format!("{reason}. You wrote: {}", super::excerpt(&text)),
                        );
                    }
                }
                completion.call(());
            }

            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                completion.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
            }
        }
    );

    fn receive_content(
        content: &objc2_user_notifications::UNNotificationContent,
    ) -> Result<(), String> {
        super::receive(decode_content(content)?)
    }

    pub(super) fn decode_content(
        content: &objc2_user_notifications::UNNotificationContent,
    ) -> Result<Option<agentdocker_host::notify::Action>, String> {
        let info = content.userInfo();
        let action = match info.objectForKey(ns_string!("agentdocker.action")) {
            Some(value) => {
                let text = value
                    .downcast_ref::<NSString>()
                    .ok_or_else(|| "invalid notification destination".to_owned())?;
                Some(agentdocker_host::notify::Action::parse(&text.to_string())?)
            }
            None => None,
        };
        Ok(action)
    }

    /// Hold the delegate until the application run loop exits. The OS keeps a
    /// weak reference, and registration must precede launching the Iced window.
    pub fn install() -> Option<Retained<Delegate>> {
        if !crate::notify::in_a_bundle() {
            return None;
        }
        // SAFETY: NSObject initialization is valid for this stateless subclass.
        let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::class(), new] };
        UNUserNotificationCenter::currentNotificationCenter()
            .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        crate::notify::register_categories();
        Some(delegate)
    }
}

#[cfg(target_os = "macos")]
pub use native::install;
#[cfg(not(target_os = "macos"))]
pub fn install() -> Option<()> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentId, MessageId, NotificationTarget};

    fn activation(id: usize) -> Activation {
        Activation::Open(Action {
            home: std::path::PathBuf::from("/tmp/state"),
            socket: std::path::PathBuf::from("/tmp/state/agentd.sock"),
            target: NotificationTarget {
                message: MessageId::from(id.to_string()),
                agent: AgentId::from("agent"),
                project: None,
                channel: None,
            },
        })
    }

    #[test]
    fn a_reply_worker_that_cannot_start_returns_the_full_draft_for_recovery() {
        let Activation::Open(action) = activation(7) else {
            unreachable!()
        };
        let text = "日本語 reply\n".repeat(500);
        let expected = (action.clone(), text.clone());
        let mut recovered = None;
        start_reply(
            action,
            text,
            |_, _| Err(std::io::Error::other("worker refused")),
            |action, text, failure| recovered = Some((action, text, failure)),
        );
        let (action, text, failure) = recovered.expect("recovery called");
        assert_eq!((action, text), expected);
        assert!(failure.certain);
        assert!(failure.reason.contains("worker refused"));

        start_reply(
            expected.0,
            expected.1,
            |_, _| Ok(()),
            |_, _, _| panic!("started delivery owns recovery"),
        );
    }

    /// A reply goes where the original went — the project's everyone,
    /// the channel, or back to whoever wrote to the person — as the
    /// person's reply to that message; a notice and a topic post have no
    /// reply; blank is nothing and oversized is refused before any socket.
    #[test]
    fn a_reply_answers_where_the_original_went_as_the_person() {
        use agentdocker_core::{Destination, Envelope};
        let original = |from: &str, to: Destination| {
            Envelope::new(
                from,
                to,
                "chat",
                serde_json::json!({"text": "?"}),
                None,
                chrono::Utc::now(),
            )
        };
        assert_eq!(
            reply_destination(&original(
                "sender-1",
                Destination::Project(agentdocker_core::ProjectId::from("p1"))
            ))
            .unwrap(),
            "project:p1"
        );
        assert_eq!(
            reply_destination(&original(
                "sender-1",
                Destination::Channel(agentdocker_core::ChannelId::from("reviews"))
            ))
            .unwrap(),
            "channel:reviews"
        );
        assert_eq!(
            reply_destination(&original("sender-1", Destination::Broadcast)).unwrap(),
            "all"
        );
        // A question asked of everyone is answered to its asker: that is
        // the only reply the daemon closes it by, and an answer is not
        // for everyone.
        for to in [
            Destination::Broadcast,
            Destination::Project(agentdocker_core::ProjectId::from("p1")),
            Destination::Channel(agentdocker_core::ChannelId::from("reviews")),
            Destination::Agent("user".into()),
        ] {
            let mut question = original("asker-1", to);
            question.kind = "question".into();
            assert_eq!(reply_destination(&question).unwrap(), "asker-1");
        }
        assert_eq!(
            reply_destination(&original("sender-1", Destination::Agent("user".into()))).unwrap(),
            "sender-1"
        );
        assert!(
            reply_destination(&original(
                agentdocker_core::conversation::DAEMON,
                Destination::Agent("user".into())
            ))
            .unwrap_err()
            .certain
        );
        assert!(
            reply_destination(&original("sender-1", Destination::Topic("t".into())))
                .unwrap_err()
                .certain
        );
        match reply_request(
            "project:p1".into(),
            &MessageId::from("7".to_owned()),
            "on it",
        ) {
            Request::Send {
                from,
                to,
                kind,
                payload,
                reply_to,
                ..
            } => {
                assert_eq!(from, "user");
                assert_eq!(to, "project:p1");
                assert_eq!(kind, "chat");
                assert_eq!(payload["text"], "on it");
                assert_eq!(reply_to, Some(MessageId::from("7".to_owned())));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(reply_text("  on it  ").unwrap().as_deref(), Some("on it"));
        assert_eq!(reply_text("   ").unwrap(), None);
        assert!(
            reply_text(&"x".repeat(REPLY_CHARS + 1))
                .unwrap_err()
                .certain
        );
    }

    /// Against a daemon that answers as told: the original is read to
    /// learn where it went and the reply follows it; only `sent` is
    /// success; a refusal is certain; a connection that closes before an
    /// answer is not, so the person is sent to the history, not to a
    /// resend.
    #[test]
    #[cfg(unix)]
    fn a_reply_takes_only_sent_as_success_and_tells_a_refusal_from_an_unknown_outcome() {
        use agentdocker_core::{ArchivedMessage, Destination, Envelope};
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let Activation::Open(action) = activation(7) else {
            unreachable!()
        };
        let root = || {
            let mut envelope = Envelope::new(
                "sender-1",
                Destination::Project(agentdocker_core::ProjectId::from("p1")),
                "chat",
                serde_json::json!({"text": "?"}),
                None,
                chrono::Utc::now(),
            );
            envelope.id = MessageId::from("7".to_owned());
            ArchivedMessage {
                seq: 1,
                conversation: agentdocker_core::ConversationId::of(&envelope).unwrap(),
                envelope,
                replies: 0,
            }
        };
        #[derive(Clone, Copy)]
        enum Answer {
            Sent,
            Refused,
            Silence,
            Missing,
        }
        for (answer, expected) in [
            (Answer::Sent, Ok(())),
            (
                Answer::Refused,
                Err(Failure::certain("recipient is paused")),
            ),
            (
                Answer::Silence,
                Err(Failure::unknown(
                    "agentd closed the connection without answering",
                )),
            ),
            (
                Answer::Missing,
                Err(Failure::certain(
                    "the message could not be read: no such message",
                )),
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let socket = tmp.path().join("agentd.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = seen.clone();
            let served = answer;
            let server = std::thread::spawn(move || {
                for turn in 0..2 {
                    let Ok((stream, _)) = listener.accept() else {
                        return;
                    };
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let request: Request = serde_json::from_str(&line).unwrap();
                    log.lock().unwrap().push(request.clone());
                    let reply = match (turn, &request, &served) {
                        (0, Request::Thread { .. }, Answer::Missing) => Response::Error {
                            code: agentdocker_core::ErrorCode::NotFound,
                            message: "no such message".into(),
                            details: None,
                        },
                        (0, Request::Thread { .. }, _) => Response::Thread {
                            root: root(),
                            replies: Vec::new(),
                        },
                        (1, Request::Send { .. }, Answer::Sent) => Response::Sent {
                            message: MessageId::from("8".to_owned()),
                            subscribers: 1,
                            recipient_readiness: None,
                        },
                        (1, Request::Send { .. }, Answer::Refused) => Response::Error {
                            code: agentdocker_core::ErrorCode::Forbidden,
                            message: "recipient is paused".into(),
                            details: None,
                        },
                        (1, Request::Send { .. }, Answer::Silence) => return,
                        other => panic!("unexpected turn {:?}", other.1),
                    };
                    serde_json::to_writer(reader.get_mut(), &reply).unwrap();
                    reader.get_mut().write_all(b"\n").unwrap();
                    if matches!(served, Answer::Missing) {
                        return;
                    }
                }
            });
            let client = crate::client::Client::at(tmp.path().to_owned(), socket);
            let mut action = action.clone();
            action.home = tmp.path().to_owned();
            action.socket = client.socket().to_owned();
            let outcome = deliver(&client, &action, "on it");
            server.join().unwrap();
            assert_eq!(outcome, expected);
            let seen = seen.lock().unwrap();
            assert!(matches!(&seen[0], Request::Thread { message, .. } if message.as_str() == "7"));
            if !matches!(answer, Answer::Missing) {
                assert!(
                    matches!(&seen[1], Request::Send { to, reply_to, from, .. }
                        if to == "project:p1" && reply_to.as_ref().map(|m| m.as_str()) == Some("7") && from == "user"),
                    "the reply follows the original to the project's everyone: {:?}",
                    seen[1]
                );
            }
        }
    }

    #[test]
    fn repeated_clicks_coalesce_and_pressure_never_evicts_accepted_navigation() {
        let mut queue = Queue::default();
        for i in 0..32 {
            queue.push(activation(i)).unwrap();
            queue.push(activation(i)).unwrap();
        }
        assert!(queue.push(activation(32)).is_err());
        assert_eq!(queue.0.len(), 32);
        for i in 0..32 {
            assert_eq!(queue.0.pop_front(), Some(activation(i)));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_content_preserves_destination_separately_from_visible_text() {
        let Activation::Open(action) = activation(7) else {
            unreachable!()
        };
        let mut notice = agentdocker_host::notify::Notification {
            title: "An agent asks".into(),
            body: "quotes \" and emoji 💬".into(),
            action: Some(action.clone()),
        };
        let content = crate::notify::notification_content(&notice).unwrap();
        assert_eq!(content.title().to_string(), notice.title);
        assert_eq!(content.body().to_string(), notice.body);
        assert_eq!(native::decode_content(&content).unwrap(), Some(action));
        notice.action = None;
        let legacy = crate::notify::notification_content(&notice).unwrap();
        assert_eq!(native::decode_content(&legacy).unwrap(), None);
    }
}
