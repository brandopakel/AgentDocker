//! Bounded terminal input with wakeup on close and no idle polling.
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

pub(super) const MAX_MESSAGES: usize = 32;
pub(super) const MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outbound {
    Keys(Box<[u8]>),
    Resize { cols: u16, rows: u16 },
}

impl Outbound {
    fn bytes(&self) -> usize {
        match self {
            Self::Keys(bytes) => bytes.len(),
            Self::Resize { .. } => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rejected {
    TooLarge,
    Full,
    Closed,
}

impl Rejected {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::TooLarge => "Input was not sent: a single entry must fit within 64 KiB.",
            Self::Full => "Input was not sent: the terminal input queue is full.",
            Self::Closed => "Input was not sent: the terminal connection has closed.",
        }
    }
}

#[derive(Default)]
struct State {
    messages: VecDeque<Outbound>,
    bytes: usize,
    closed: bool,
}

#[derive(Default)]
pub(super) struct Input {
    state: Mutex<State>,
    ready: Condvar,
}

impl Input {
    /// Admit the whole frame or none of it; the UI never waits for capacity.
    pub(super) fn push(&self, message: Outbound) -> Result<(), Rejected> {
        let size = message.bytes();
        if size > MAX_BYTES {
            return Err(Rejected::TooLarge);
        }
        let mut state = super::lock(&self.state);
        if state.closed {
            return Err(Rejected::Closed);
        }
        // Adjacent resizes have no intervening keystrokes to order against.
        if matches!(message, Outbound::Resize { .. })
            && let Some(last @ Outbound::Resize { .. }) = state.messages.back_mut()
        {
            *last = message;
            return Ok(());
        }
        if state.messages.len() == MAX_MESSAGES || size > MAX_BYTES - state.bytes {
            return Err(Rejected::Full);
        }
        state.bytes += size;
        state.messages.push_back(message);
        self.ready.notify_one();
        Ok(())
    }

    pub(super) fn recv(&self) -> Option<Outbound> {
        let mut state = super::lock(&self.state);
        loop {
            if let Some(message) = state.messages.pop_front() {
                state.bytes -= message.bytes();
                return Some(message);
            }
            if state.closed {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Release unsent input and wake the writer even while no keys are queued.
    pub(super) fn close(&self) {
        let mut state = super::lock(&self.state);
        state.closed = true;
        state.messages.clear();
        state.bytes = 0;
        self.ready.notify_all();
    }

    #[cfg(test)]
    pub(super) fn retained(&self) -> (usize, usize) {
        let state = super::lock(&self.state);
        (state.messages.len(), state.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_is_atomic_and_capacity_returns_after_dispatch() {
        let input = Input::default();
        let block = vec![b'x'; MAX_BYTES].into_boxed_slice();
        input.push(Outbound::Keys(block.clone())).unwrap();
        assert_eq!(
            input.push(Outbound::Keys(Box::from(*b"y"))),
            Err(Rejected::Full)
        );
        assert_eq!(input.retained(), (1, MAX_BYTES));
        assert_eq!(input.recv(), Some(Outbound::Keys(block)));
        input.push(Outbound::Keys(Box::from(*b"ok"))).unwrap();
        assert_eq!(input.retained(), (1, 2));
        assert_eq!(
            input.push(Outbound::Keys(vec![0; MAX_BYTES + 1].into_boxed_slice())),
            Err(Rejected::TooLarge)
        );
        assert_eq!(input.retained(), (1, 2));
    }

    #[test]
    fn resize_coalescing_preserves_keystroke_order() {
        let input = Input::default();
        for cols in 80..180 {
            input.push(Outbound::Resize { cols, rows: 24 }).unwrap();
        }
        assert_eq!(input.retained(), (1, 4));
        input.push(Outbound::Keys(Box::from(*b"x"))).unwrap();
        input.push(Outbound::Resize { cols: 80, rows: 25 }).unwrap();
        assert_eq!(
            input.recv(),
            Some(Outbound::Resize {
                cols: 179,
                rows: 24
            })
        );
        assert_eq!(input.recv(), Some(Outbound::Keys(Box::from(*b"x"))));
        assert_eq!(input.recv(), Some(Outbound::Resize { cols: 80, rows: 25 }));
    }

    #[test]
    fn closing_releases_input_and_wakes_an_idle_receiver() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;
        let input = Arc::new(Input::default());
        let receiver = input.clone();
        let (finished, done) = mpsc::channel();
        let worker = std::thread::spawn(move || finished.send(receiver.recv()).unwrap());
        input.close();
        let observed = done.recv_timeout(Duration::from_secs(3));
        worker.join().unwrap();
        assert_eq!(observed.unwrap(), None);
        assert_eq!(
            input.push(Outbound::Resize { cols: 80, rows: 24 }),
            Err(Rejected::Closed)
        );

        let queued = Input::default();
        queued
            .push(Outbound::Keys(vec![0; MAX_BYTES].into_boxed_slice()))
            .unwrap();
        queued.close();
        assert_eq!(queued.retained(), (0, 0));
        assert_eq!(queued.recv(), None);
    }
}
