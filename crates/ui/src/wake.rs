//! Snapshot replies share a bounded wake; terminal and user actions wake immediately.
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::time::Instant;

const SNAPSHOT_QUIET: Duration = Duration::from_millis(16);
const SNAPSHOT_MAX_WAIT: Duration = Duration::from_millis(50);

#[derive(Default)]
struct Signals {
    changed: tokio::sync::Notify,
    pending: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    urgent: bool,
    snapshot: Option<Burst>,
}

struct Burst {
    first: Instant,
    last: Instant,
}

#[derive(Clone)]
pub struct Wake(Arc<Signals>);
impl Default for Wake {
    fn default() -> Self {
        static SIGNALS: OnceLock<Arc<Signals>> = OnceLock::new();
        Self(SIGNALS.get_or_init(Default::default).clone())
    }
}
impl Wake {
    pub fn request_repaint(&self) {
        self.0
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .urgent = true;
        self.0.changed.notify_one();
    }

    /// Only successful read snapshots may wait for their neighbouring replies.
    /// A busy producer cannot extend the wait past the first reply's deadline.
    pub fn request_snapshot_repaint(&self) {
        let mut pending = self
            .0
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let burst = pending.snapshot.get_or_insert(Burst {
            first: now,
            last: now,
        });
        burst.last = now;
        drop(pending);
        self.0.changed.notify_one();
    }

    /// The window's one subscription waits here. Producers do not spawn timers
    /// or sleep, and an urgent wake also covers the pending snapshot messages:
    /// the same UI tick drains their shared queue.
    pub async fn notified(&self) {
        loop {
            let deadline = {
                let mut pending = self
                    .0
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let deadline = pending.snapshot.as_ref().map(|burst| {
                    (burst.last + SNAPSHOT_QUIET).min(burst.first + SNAPSHOT_MAX_WAIT)
                });
                if pending.urgent || deadline.is_some_and(|at| at <= Instant::now()) {
                    *pending = Pending::default();
                    return;
                }
                deadline
            };
            // Notify retains a permit if a producer runs between releasing the
            // mutex and registering this waiter. Recheck the state after every
            // signal, including a permit left over from the preceding burst.
            if let Some(deadline) = deadline {
                tokio::select! {
                    () = self.0.changed.notified() => {},
                    () = tokio::time::sleep_until(deadline) => {},
                }
            } else {
                self.0.changed.notified().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated() -> Wake {
        Wake(Arc::new(Signals::default()))
    }

    fn waiter(wake: &Wake) -> tokio::task::JoinHandle<Instant> {
        let wake = wake.clone();
        tokio::spawn(async move {
            wake.notified().await;
            Instant::now()
        })
    }

    #[tokio::test(start_paused = true)]
    async fn snapshot_replies_share_one_wake_after_the_quiet_period() {
        let wake = isolated();
        let start = Instant::now();
        let waiting = waiter(&wake);
        wake.request_snapshot_repaint();
        tokio::task::yield_now().await;
        for _ in 1..8 {
            tokio::time::advance(Duration::from_millis(2)).await;
            wake.request_snapshot_repaint();
            tokio::task::yield_now().await;
            assert!(!waiting.is_finished());
        }
        tokio::time::advance(SNAPSHOT_QUIET - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(waiting.await.unwrap() - start, Duration::from_millis(30));

        // Old Notify permits must not create another UI update for this burst.
        let next = waiter(&wake);
        tokio::time::advance(SNAPSHOT_MAX_WAIT).await;
        tokio::task::yield_now().await;
        assert!(!next.is_finished());
        wake.request_repaint();
        assert_eq!(next.await.unwrap(), Instant::now());
    }

    #[tokio::test(start_paused = true)]
    async fn urgent_work_interrupts_and_consumes_a_pending_snapshot_wake() {
        let wake = isolated();
        let start = Instant::now();
        let waiting = waiter(&wake);
        wake.request_snapshot_repaint();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(4)).await;
        wake.request_repaint();
        assert_eq!(waiting.await.unwrap() - start, Duration::from_millis(4));

        let next = waiter(&wake);
        tokio::time::advance(SNAPSHOT_MAX_WAIT).await;
        tokio::task::yield_now().await;
        assert!(
            !next.is_finished(),
            "urgent work already covered that snapshot"
        );
        let again = Instant::now();
        wake.request_snapshot_repaint();
        assert_eq!(next.await.unwrap() - again, SNAPSHOT_QUIET);
    }

    #[tokio::test(start_paused = true)]
    async fn continuous_snapshot_replies_cannot_starve_the_window() {
        let wake = isolated();
        let start = Instant::now();
        let waiting = waiter(&wake);
        wake.request_snapshot_repaint();
        tokio::task::yield_now().await;
        // Another reply arrives every 5 ms, so there is never 16 ms of quiet.
        for _ in 1..10 {
            tokio::time::advance(Duration::from_millis(5)).await;
            wake.request_snapshot_repaint();
            tokio::task::yield_now().await;
            assert!(!waiting.is_finished());
        }
        tokio::time::advance(Duration::from_millis(5)).await;
        assert_eq!(waiting.await.unwrap() - start, SNAPSHOT_MAX_WAIT);

        // A new reply after the flush begins another bounded burst.
        let next = waiter(&wake);
        let again = Instant::now();
        wake.request_snapshot_repaint();
        assert_eq!(next.await.unwrap() - again, SNAPSHOT_QUIET);
    }
}
