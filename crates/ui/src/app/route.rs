//! A notification's route: where a click on a notification stands.
//!
//! A click on a notification does not open its message at once. The
//! window may not have the data that says where the message is, so the
//! route waits for it; then its destination opens and the message's row
//! is marked; then, when the row is in an archive page that is not here,
//! the pages before are read back for it; then the row is scrolled to.
//!
//! That is one thing with one life, and it was four fields that each
//! handler remembered to clear or did not. Here it is one value, moved
//! only by the functions below, so that ending it ends all of it.
//!
//! What ends it is not decided here: the person going somewhere else
//! does, whatever took them there, and `App::update` sees that by
//! comparing the place before and after each of their moves.

use std::time::Instant;

use agentdocker_core::MessageId;
use agentdocker_host::notify::Action;

use super::Seek;

/// Where a click on a notification stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Route {
    /// No notification is being followed.
    #[default]
    Idle,
    /// The click waits for the data that says where its message is, since
    /// `started`: after long enough it gives up and says so.
    Routing { action: Action, started: Instant },
    /// The destination is open and `message`'s row is marked.
    Shown { message: MessageId, reveal: Reveal },
}

/// What is still owed to a shown message's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reveal {
    /// Nothing: the row is marked, and on view if its page is.
    Done,
    /// Its page is not here: the archive is being read back for it.
    Seeking(Seek),
    /// Its page is here: the next tick scrolls to the row.
    Scroll,
}

impl Route {
    /// A notification was clicked. Whatever route there was is replaced,
    /// with its mark and its search: the window follows one at a time.
    pub fn begin(&mut self, action: Action, now: Instant) {
        *self = Route::Routing {
            action,
            started: now,
        };
    }

    /// The waiting click and when it began.
    pub fn waiting(&self) -> Option<(&Action, Instant)> {
        match self {
            Route::Routing { action, started } => Some((action, *started)),
            _ => None,
        }
    }

    pub fn is_routing(&self) -> bool {
        matches!(self, Route::Routing { .. })
    }

    /// The message a waiting click is on its way to.
    pub fn target(&self) -> Option<&MessageId> {
        self.waiting().map(|(action, _)| &action.target.message)
    }

    /// The message whose row is marked, once its destination is open.
    pub fn message(&self) -> Option<&MessageId> {
        match self {
            Route::Shown { message, .. } => Some(message),
            _ => None,
        }
    }

    /// The destination opened: the message is shown and marked.
    pub fn resolve(&mut self, message: MessageId) {
        *self = Route::Shown {
            message,
            reveal: Reveal::Done,
        };
    }

    /// The search through the archive, while one runs.
    pub fn seek(&self) -> Option<&Seek> {
        match self {
            Route::Shown {
                reveal: Reveal::Seeking(seek),
                ..
            } => Some(seek),
            _ => None,
        }
    }

    pub fn seek_mut(&mut self) -> Option<&mut Seek> {
        match self {
            Route::Shown {
                reveal: Reveal::Seeking(seek),
                ..
            } => Some(seek),
            _ => None,
        }
    }

    /// The archive is read back for the shown message, or a page further
    /// back is asked for. A seek is always the shown message's: the mark
    /// and the search cannot name two messages.
    pub fn start_seek(&mut self, seek: Seek) {
        *self = Route::Shown {
            message: seek.message.clone(),
            reveal: Reveal::Seeking(seek),
        };
    }

    /// The page that holds the message arrived: the scroll is owed.
    pub fn found(&mut self) {
        if let Route::Shown { reveal, .. } = self
            && matches!(reveal, Reveal::Seeking(_))
        {
            *reveal = Reveal::Scroll;
        }
    }

    /// The row to scroll to, once: taking it pays the scroll.
    pub fn take_scroll(&mut self) -> Option<MessageId> {
        match self {
            Route::Shown { message, reveal } if *reveal == Reveal::Scroll => {
                *reveal = Reveal::Done;
                Some(message.clone())
            }
            _ => None,
        }
    }

    /// Whether a scroll is owed, without paying it: for the tests, which
    /// look before the tick that pays it.
    #[cfg(test)]
    pub fn scroll(&self) -> Option<&MessageId> {
        match self {
            Route::Shown {
                message,
                reveal: Reveal::Scroll,
            } => Some(message),
            _ => None,
        }
    }

    /// The search cannot go on (the daemon is lost, the archive's start
    /// was reached, a page was refused, the bound was met): the message
    /// stays marked and nothing more is looked for or scrolled to. A
    /// waiting click is not a search, and waits on.
    pub fn end_search(&mut self) {
        if let Route::Shown { reveal, .. } = self {
            *reveal = Reveal::Done;
        }
    }

    /// The route is over: the wait, the mark, the search and the scroll.
    pub fn end(&mut self) {
        *self = Route::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str) -> MessageId {
        MessageId::from(id.to_owned())
    }

    fn seek(id: &str) -> Seek {
        Seek {
            conversation: "dm:agent".to_owned(),
            message: message(id),
            pages: 0,
            before: None,
            refresh_pending: true,
        }
    }

    /// The life of a route, by its transitions: each state answers the
    /// questions the window asks of it, and only of it.
    #[test]
    fn a_route_goes_from_waiting_to_shown_to_sought_to_scrolled() {
        let mut route = Route::default();
        assert_eq!(route, Route::Idle);
        assert!(!route.is_routing() && route.message().is_none() && route.seek().is_none());

        route.resolve(message("m1"));
        assert_eq!(route.message(), Some(&message("m1")));
        assert!(route.seek().is_none() && route.scroll().is_none());
        assert_eq!(route.take_scroll(), None, "no scroll is owed yet");

        route.start_seek(seek("m1"));
        assert_eq!(route.message(), Some(&message("m1")), "still marked");
        assert_eq!(route.seek().map(|seek| seek.pages), Some(0));
        route.seek_mut().unwrap().refresh_pending = false;
        assert!(!route.seek().unwrap().refresh_pending);

        route.found();
        assert!(route.seek().is_none());
        assert_eq!(route.scroll(), Some(&message("m1")));
        assert_eq!(route.take_scroll(), Some(message("m1")));
        assert_eq!(route.take_scroll(), None, "paid once");
        assert_eq!(route.message(), Some(&message("m1")), "the mark stays");
    }

    /// Ending the search keeps the mark; ending the route keeps nothing;
    /// and a seek names the message that is shown, whatever was before.
    #[test]
    fn ending_a_search_keeps_the_mark_and_ending_a_route_keeps_nothing() {
        let mut route = Route::Idle;
        route.start_seek(seek("m2"));
        assert_eq!(route.message(), Some(&message("m2")));
        route.end_search();
        assert_eq!(
            route,
            Route::Shown {
                message: message("m2"),
                reveal: Reveal::Done
            }
        );
        route.start_seek(seek("m2"));
        route.found();
        route.end_search();
        assert_eq!(route.take_scroll(), None, "the owed scroll went too");
        route.found();
        assert_eq!(route.scroll(), None, "nothing was being looked for");
        route.end();
        assert_eq!(route, Route::Idle);
        route.end_search();
        assert_eq!(route, Route::Idle);
    }
}
