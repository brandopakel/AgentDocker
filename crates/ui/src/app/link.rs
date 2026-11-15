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
//!
//! # Whose answer it is
//!
//! Two threads report on the daemon, each in its own time: the event
//! stream, and the worker that makes the calls. A call to a daemon that
//! is being replaced can return after the stream has already reported
//! its successor, so nothing about the order in which their words arrive
//! says which daemon a late answer came from.
//!
//! So the link counts its connections ([`Connection`]: one more each time
//! it comes up or goes down), the worker reads that count when it starts
//! a call ([`Watch`]), and what the call brings back carries it: the
//! answer, and the call's own word that the link is up or down
//! ([`Report::Call`]). The link takes only what was asked on the
//! connection it has now. A call that starts on a connection reaches the
//! daemon that began it or one that has taken its place since, never one
//! from before, since a daemon's socket passes only forward. What a
//! replaced daemon says late is therefore never its successor's.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::DaemonInfo;

/// The link to the background service: what state it is in, and which
/// connection that is.
#[derive(Debug, Default)]
pub(crate) struct Link {
    state: State,
    /// One more each time the link comes up or goes down. Shared with the
    /// worker, which reads it as it starts a call.
    connection: Arc<AtomicU64>,
}

/// What the link is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// The window has not reached a daemon since it opened. `reason` is
    /// why the last try failed, once one has.
    Connecting { reason: Option<String> },
    /// A daemon answers.
    Up(Service),
    /// A daemon answered and no longer does. What it had said is kept
    /// for what the window goes on showing.
    Down { reason: String, was: Service },
}

/// One stretch of the link being up, or of its being down: the window's
/// own count, which says when something was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Connection(u64);

/// The link's connection as another thread reads it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Watch(Arc<AtomicU64>);

impl Watch {
    /// The connection the link has now: what a call that starts now is
    /// asked on.
    pub fn now(&self) -> Connection {
        Connection(self.0.load(Ordering::SeqCst))
    }
}

/// Who says the link came up or went down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Report {
    /// The event stream. It is one connection to one daemon, and it
    /// reports in order, so what it says is about the link as it is.
    Stream,
    /// A call, asked on this connection: it speaks for that one alone.
    /// One that returns after the link has moved on says nothing of the
    /// connection there is now.
    Call(Connection),
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

impl Default for State {
    fn default() -> Self {
        State::Connecting { reason: None }
    }
}

impl Link {
    /// A link that is up with nothing yet known of its daemon.
    #[cfg(test)]
    pub fn up() -> Self {
        Link {
            state: State::Up(Service::default()),
            connection: Arc::default(),
        }
    }

    /// The event stream says a daemon answers: what a test starts from.
    #[cfg(test)]
    pub fn open(&mut self) {
        self.connected(Report::Stream);
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn is_up(&self) -> bool {
        matches!(self.state, State::Up(_))
    }

    /// The connection there is now.
    pub fn connection(&self) -> Connection {
        Connection(self.connection.load(Ordering::SeqCst))
    }

    /// The connection, for a thread that asks on it.
    pub fn watch(&self) -> Watch {
        Watch(self.connection.clone())
    }

    /// Whether a report is about the link as it is: the stream's always,
    /// a call's when it was asked on the connection there is now.
    fn hears(&self, from: Report) -> bool {
        match from {
            Report::Stream => true,
            Report::Call(asked) => asked == self.connection(),
        }
    }

    /// The link came up or went down: what is asked from now on is asked
    /// on another connection.
    fn turn(&mut self) {
        self.connection.fetch_add(1, Ordering::SeqCst);
    }

    /// A daemon answered. Returns whether that is a new connection, on
    /// which everything is asked again. What the last connection's
    /// daemon said is not this one's: its version and the question
    /// about restarting it go, and its capabilities stay only as what to
    /// show until this one answers.
    ///
    /// A call asked on an earlier connection does not bring the link up:
    /// it reached the daemon of that time.
    pub fn connected(&mut self, from: Report) -> bool {
        if !self.hears(from) {
            return false;
        }
        let last = match &self.state {
            State::Up(_) => return false,
            State::Connecting { .. } => None,
            State::Down { was, .. } => was.conversations.shown(),
        };
        self.state = State::Up(Service {
            info: None,
            conversations: Capability::Unasked { last },
            confirm_restart: false,
        });
        self.turn();
        true
    }

    /// No daemon answers. Returns whether the report was taken: a call
    /// asked on an earlier connection failed against the daemon of that
    /// time and says nothing of this one.
    ///
    /// A window that never reached a daemon is still connecting, now
    /// with a reason; one that had is down, and keeps what its daemon
    /// said, but for the question about restarting it, which was about a
    /// daemon that is no longer there to restart.
    pub fn lost(&mut self, from: Report, reason: String) -> bool {
        if !self.hears(from) {
            return false;
        }
        let was_up = self.is_up();
        self.state = match std::mem::take(&mut self.state) {
            State::Connecting { .. } => State::Connecting {
                reason: Some(reason),
            },
            State::Up(service) | State::Down { was: service, .. } => State::Down {
                reason,
                was: Service {
                    confirm_restart: false,
                    ..service
                },
            },
        };
        if was_up {
            self.turn();
        }
        true
    }

    /// What the daemon said: the one that answers, or the last one that
    /// did.
    pub fn service(&self) -> Option<&Service> {
        match &self.state {
            State::Up(service) | State::Down { was: service, .. } => Some(service),
            State::Connecting { .. } => None,
        }
    }

    /// The daemon that answers, for what the person does about it. `None`
    /// while none does.
    pub fn serving(&mut self) -> Option<&mut Service> {
        match &mut self.state {
            State::Up(service) => Some(service),
            _ => None,
        }
    }

    /// The daemon that answers, to record what it said to something
    /// asked on `asked`. `None` when that was another connection, or
    /// none answers: the answer is about a daemon the window is no
    /// longer linked to, and this one is asked its own.
    pub fn answering(&mut self, asked: Connection) -> Option<&mut Service> {
        if asked != self.connection() {
            return None;
        }
        self.serving()
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
        matches!(&self.state, State::Up(service) if service.confirm_restart)
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
        assert_eq!(link.state(), &State::Connecting { reason: None });
        assert!(!link.is_up());
        assert!(link.lost(Report::Stream, "no socket".to_owned()));
        assert_eq!(
            link.state(),
            &State::Connecting {
                reason: Some("no socket".to_owned())
            }
        );
        assert!(link.service().is_none() && link.info().is_none());
        assert!(
            link.connected(Report::Stream),
            "the first connection is a new one"
        );
        assert!(link.is_up());
        assert!(
            !link.connected(Report::Stream),
            "already up: nothing to ask again"
        );
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

        link.lost(Report::Stream, "event stream ended".to_owned());
        assert!(!link.is_up());
        assert!(
            matches!(link.state(), State::Down { reason, .. } if reason == "event stream ended")
        );
        assert_eq!(link.info(), Some(&info("1.0.0")), "kept while down");
        assert_eq!(link.conversations().shown(), Some(false));
        assert!(!link.confirming_restart(), "no daemon to restart");
        assert!(link.serving().is_none(), "nothing answers to record on");
        // Lost again while down: the first daemon's word is still kept.
        link.lost(Report::Stream, "still gone".to_owned());
        assert!(matches!(link.state(), State::Down { reason, .. } if reason == "still gone"));
        assert_eq!(link.info(), Some(&info("1.0.0")));

        // Another daemon answers, an upgraded one perhaps.
        assert!(link.connected(Report::Stream));
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

    /// The connection changes each time the link comes up or goes down,
    /// and not otherwise, and another thread reads the same count.
    #[test]
    fn the_connection_changes_when_the_link_comes_up_or_goes_down() {
        let mut link = Link::default();
        let watch = link.watch();
        let opening = link.connection();
        assert_eq!(watch.now(), opening);
        // Still connecting: a failed try is not a change of connection,
        // so a call that began before it can still bring the link up.
        link.lost(Report::Stream, "no socket".to_owned());
        assert_eq!(link.connection(), opening);
        assert!(link.connected(Report::Call(opening)));
        let first = link.connection();
        assert_ne!(first, opening);
        assert_eq!(watch.now(), first, "the worker reads what the link has");
        // Up already: nothing changes.
        assert!(!link.connected(Report::Stream));
        assert_eq!(link.connection(), first);

        link.lost(Report::Stream, "gone".to_owned());
        let down = link.connection();
        assert_ne!(down, first);
        // Down already: another failure is the same stretch of being down.
        link.lost(Report::Call(down), "still gone".to_owned());
        assert_eq!(link.connection(), down);
        link.connected(Report::Stream);
        assert_ne!(link.connection(), down);
    }

    /// A call to daemon A returns after the event stream has reported A
    /// gone and B there. What it brings back was asked on A's
    /// connection: B's service takes none of it, neither the answer nor
    /// the call's word that the link is up or down, and takes B's own.
    #[test]
    fn what_a_replaced_daemon_says_late_is_not_its_successors() {
        let mut link = Link::up();
        let on_a = link.connection();
        link.serving().unwrap().info = Some(info("1.0.0"));

        link.lost(Report::Stream, "event stream ended".to_owned());
        // A's call returns while nothing answers. Its answer has nowhere
        // to go, and its "the link is up" is about A: it makes no fresh
        // service for whatever A said next to land on.
        assert!(link.answering(on_a).is_none());
        assert!(!link.connected(Report::Call(on_a)));
        assert!(!link.is_up(), "an old call does not bring the link up");

        assert!(link.connected(Report::Stream), "B answers");
        let on_b = link.connection();
        let fresh = link.service().cloned().unwrap();
        assert_eq!(fresh.info, None);

        // And A's call returns now, after B is there.
        assert!(link.answering(on_a).is_none(), "A's answer is not B's");
        assert!(!link.connected(Report::Call(on_a)));
        assert!(
            !link.lost(Report::Call(on_a), "A closed the connection".to_owned()),
            "nor does A's failure take B down"
        );
        assert!(link.is_up());
        assert_eq!(link.connection(), on_b);
        assert_eq!(link.service(), Some(&fresh), "B's service is untouched");

        // B's own answer is taken.
        link.answering(on_b).expect("asked on B's connection").info = Some(info("2.0.0"));
        assert_eq!(link.info(), Some(&info("2.0.0")));
        // And a call asked on B's connection that fails does take it down.
        assert!(link.lost(Report::Call(on_b), "B is gone".to_owned()));
        assert!(!link.is_up());
    }
}
