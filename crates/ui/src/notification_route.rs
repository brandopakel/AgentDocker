//! One bounded activation queue for native notification responses and CLI opens.
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

#[cfg(target_os = "macos")]
use agentdocker_core::Request;
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
    if let Activation::Open(action) = &activation {
        Action::parse(&serde_json::to_string(action).map_err(|e| e.to_string())?)?;
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
#[cfg(target_os = "macos")]
pub const REPLY_CHARS: usize = 4_000;

/// What a reply typed into a notification sends: from the person, to the
/// notification's conversation — the channel it was in, else the agent
/// who wrote — as a reply to that message, so an answer to a question
/// closes it the way the composer's would. Blank is nothing to send.
#[cfg(target_os = "macos")]
pub fn reply_request(action: &Action, text: &str) -> Result<Request, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing to send".into());
    }
    if text.chars().count() > REPLY_CHARS {
        return Err("the reply is too long for a notification; answer in the app".into());
    }
    let to = match &action.target.channel {
        Some(channel) => format!("channel:{channel}"),
        None => action.target.agent.to_string(),
    };
    Ok(Request::Send {
        from: agentdocker_core::HUMAN.into(),
        to,
        kind: "chat".into(),
        payload: serde_json::json!({ "text": text }),
        reply_to: Some(action.target.message.clone()),
    })
}

/// Send a reply typed into a notification through the daemon it came
/// from, off the thread the notification centre called on. The window
/// is not asked to open: the person answered where they were. A reply
/// that could not be sent is said as a notification, since that is
/// where the person is looking.
#[cfg(target_os = "macos")]
pub fn reply(action: Action, text: String) -> Result<(), String> {
    let request = reply_request(&action, &text)?;
    std::thread::Builder::new()
        .name("notification-reply".into())
        .spawn(move || {
            let client = crate::client::Client::at(action.home.clone(), action.socket.clone());
            if let Err(reason) = client.call(&request) {
                eprintln!("reply from notification not sent: {reason}");
                let _ = crate::notify::post(
                    "Reply not sent",
                    &format!("{reason}. The message is still in the app."),
                );
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
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
                    let content = response.notification().request().content();
                    let outcome = match decode_content(&content) {
                        Ok(Some(action)) => super::reply(action, typed.userText().to_string()),
                        Ok(None) => Err("this notification has no conversation to reply to".into()),
                        Err(reason) => Err(reason),
                    };
                    if let Err(reason) = outcome {
                        eprintln!("reply from notification: {reason}");
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

    /// A reply goes from the person to the notification's conversation
    /// — the channel it was in, else the agent who wrote — as a reply to
    /// that message; blank or oversized is refused before any socket.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_reply_answers_the_notifications_conversation_as_the_person() {
        let Activation::Open(mut action) = activation(7) else {
            unreachable!()
        };
        match reply_request(&action, "  on it  ").unwrap() {
            Request::Send {
                from,
                to,
                kind,
                payload,
                reply_to,
            } => {
                assert_eq!(from, "user");
                assert_eq!(to, "agent");
                assert_eq!(kind, "chat");
                assert_eq!(payload["text"], "on it");
                assert_eq!(reply_to, Some(MessageId::from("7".to_owned())));
            }
            other => panic!("{other:?}"),
        }
        action.target.channel = Some(agentdocker_core::ChannelId::from("reviews"));
        assert!(matches!(
            reply_request(&action, "seen").unwrap(),
            Request::Send { to, .. } if to == "channel:reviews"
        ));
        assert!(reply_request(&action, "   ").is_err());
        assert!(reply_request(&action, &"x".repeat(REPLY_CHARS + 1)).is_err());
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
