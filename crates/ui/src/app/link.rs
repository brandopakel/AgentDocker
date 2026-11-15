//! The window's link to the background service.
//!
//! It was a `Result<(), String>` that began as `Err("connecting…")`, so a
//! window that had never reached a daemon and one that had lost it were
//! the same thing and both said "Reconnecting…", and the reason was kept
//! and never shown. What a daemon said of itself lived beside it in
//! fields with lives of their own: its version stayed after the daemon
//! had gone and been replaced, the question whether to restart it
//! outlived the daemon it was about, and whether it keeps conversations
//! was asked until the first "no" and never again, so a window taken
//! through an upgrade stayed in the legacy Inbox until it was reopened.
//!
//! Here the link is one value. What a daemon said belongs to the
//! connection it said it on: a new connection starts with nothing known
//! and asks again, and shows meanwhile what the last one said, so that
//! nothing on view jumps while the answer is on its way.

use super::DaemonInfo;

/// The link to the background service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// The window has not reached a daemon since it opened. `reason` is
    /// why the last try failed, once one has.
    Connecting { reason: Option<String> },
    /// A daemon answers.
    Up(Service),
    /// A daemon answered and no longer does. What it had said is kept
    /// for what the window goes on showing.
    Down { reason: String, was: Service },
}

/// What a daemon said of itself, on the connection it said it on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Service {
    /// Its version and where it runs from, once its ping has answered.
    pub info: Option<DaemonInfo>,
    /// Whether it keeps conversations: Messages, or the legacy Inbox.
    pub conversations: Capability,
    /// The person is being asked whether to restart this daemon.
    pub confirm_restart: bool,
}

/// Something a daemon either does or does not do, asked on each
/// connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Capability {
    /// This connection has not answered. `last` is what the connection
    /// before said, which is what the window shows meanwhile.
    Unasked { last: Option<bool> },
    /// This connection's answer.
    Answered(bool),
}

impl Default for Capability {
    fn default() -> Self {
        Capability::Unasked { last: None }
    }
}

impl Capability {
    /// What to show: this connection's answer, else the last one's.
    pub fn shown(self) -> Option<bool> {
        match self {
            Capability::Unasked { last } => last,
            Capability::Answered(answer) => Some(answer),
        }
    }

    /// Whether the daemon is still to be asked, or is to go on being
    /// asked: all but a daemon that said no on this connection.
    pub fn ask(self) -> bool {
        self != Capability::Answered(false)
    }
}

impl Default for Link {
    fn default() -> Self {
        Link::Connecting { reason: None }
    }
}

impl Link {
    /// A link that is up with nothing yet known of its daemon.
    #[cfg(test)]
    pub fn up() -> Self {
        Link::Up(Service::default())
    }

    pub fn is_up(&self) -> bool {
        matches!(self, Link::Up(_))
    }

    /// A daemon answered. Returns whether that is a new connection, on
    /// which everything is asked again. What the last connection's
    /// daemon said is not this one's: its version and the question
    /// about restarting it go, and its capabilities stay only as what to
    /// show until this one answers.
    pub fn connected(&mut self) -> bool {
        let last = match self {
            Link::Up(_) => return false,
            Link::Connecting { .. } => None,
            Link::Down { was, .. } => was.conversations.shown(),
        };
        *self = Link::Up(Service {
            info: None,
            conversations: Capability::Unasked { last },
            confirm_restart: false,
        });
        true
    }

    /// No daemon answers. A window that never reached one is still
    /// connecting, now with a reason; one that had is down, and keeps
    /// what its daemon said, but for the question about restarting it,
    /// which was about a daemon that is no longer there to restart.
    pub fn lost(&mut self, reason: String) {
        *self = match std::mem::take(self) {
            Link::Connecting { .. } => Link::Connecting {
                reason: Some(reason),
            },
            Link::Up(service) | Link::Down { was: service, .. } => Link::Down {
                reason,
                was: Service {
                    confirm_restart: false,
                    ..service
                },
            },
        };
    }

    /// What the daemon said: the one that answers, or the last one that
    /// did.
    pub fn service(&self) -> Option<&Service> {
        match self {
            Link::Up(service) | Link::Down { was: service, .. } => Some(service),
            Link::Connecting { .. } => None,
        }
    }

    /// The daemon that answers, to record what it says. `None` while
    /// none does: an answer that arrives after its daemon has gone is
    /// about nothing the window is linked to.
    pub fn serving(&mut self) -> Option<&mut Service> {
        match self {
            Link::Up(service) => Some(service),
            _ => None,
        }
    }

    /// The daemon's version and place, as last known.
    pub fn info(&self) -> Option<&DaemonInfo> {
        self.service()?.info.as_ref()
    }

    /// Whether the daemon keeps conversations, as far as is known.
    pub fn conversations(&self) -> Capability {
        self.service()
            .map(|service| service.conversations)
            .unwrap_or_default()
    }

    /// Whether the person is being asked about restarting the daemon.
    pub fn confirming_restart(&self) -> bool {
        matches!(self, Link::Up(service) if service.confirm_restart)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(version: &str) -> DaemonInfo {
        DaemonInfo {
            version: version.to_owned(),
            executable: None,
        }
    }

    /// A window that has never reached a daemon is connecting, with or
    /// without a reason, and is not "down": nothing was lost. It was the
    /// same `Err` as a lost connection.
    #[test]
    fn a_window_that_never_reached_a_daemon_is_connecting_not_down() {
        let mut link = Link::default();
        assert_eq!(link, Link::Connecting { reason: None });
        assert!(!link.is_up());
        link.lost("no socket".to_owned());
        assert_eq!(
            link,
            Link::Connecting {
                reason: Some("no socket".to_owned())
            }
        );
        assert!(link.service().is_none() && link.info().is_none());
        assert!(link.connected(), "the first connection is a new one");
        assert!(link.is_up());
        assert!(!link.connected(), "already up: nothing to ask again");
    }

    /// What a daemon said belongs to its connection. Lost, it is kept
    /// for what the window shows. On the next connection the version and
    /// the restart question are gone, and the capability is to be asked
    /// again, showing the last answer until this daemon gives its own.
    #[test]
    fn what_a_daemon_said_goes_with_its_connection_and_is_shown_until_the_next_answers() {
        let mut link = Link::up();
        let service = link.serving().unwrap();
        service.info = Some(info("1.0.0"));
        service.conversations = Capability::Answered(false);
        service.confirm_restart = true;
        assert!(link.confirming_restart());
        assert!(!link.conversations().ask(), "it said no: not asked again");

        link.lost("event stream ended".to_owned());
        assert!(!link.is_up());
        assert!(matches!(&link, Link::Down { reason, .. } if reason == "event stream ended"));
        assert_eq!(link.info(), Some(&info("1.0.0")), "kept while down");
        assert_eq!(link.conversations().shown(), Some(false));
        assert!(!link.confirming_restart(), "no daemon to restart");
        assert!(link.serving().is_none(), "nothing answers to record on");
        // Lost again while down: the first daemon's word is still kept.
        link.lost("still gone".to_owned());
        assert!(matches!(&link, Link::Down { reason, .. } if reason == "still gone"));
        assert_eq!(link.info(), Some(&info("1.0.0")));

        // Another daemon answers, an upgraded one perhaps.
        assert!(link.connected());
        assert_eq!(link.info(), None, "this daemon has not said who it is");
        assert_eq!(
            link.conversations(),
            Capability::Unasked { last: Some(false) }
        );
        assert_eq!(link.conversations().shown(), Some(false), "shown meanwhile");
        assert!(link.conversations().ask(), "and asked again");
        assert!(!link.confirming_restart());
        link.serving().unwrap().conversations = Capability::Answered(true);
        assert_eq!(link.conversations().shown(), Some(true));
    }

    #[test]
    fn a_capability_is_asked_until_this_connection_says_no() {
        assert!(Capability::default().ask());
        assert_eq!(Capability::default().shown(), None);
        assert!(Capability::Unasked { last: Some(false) }.ask());
        assert!(Capability::Answered(true).ask(), "its list is kept current");
        assert!(!Capability::Answered(false).ask());
    }
}
