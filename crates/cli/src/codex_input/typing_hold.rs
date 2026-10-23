//! The bridge's half of [`agentdocker_core::typing`]. Its editor reads the
//! person's keystrokes, so the bridge keeps the clock: a queued message
//! from anyone but the person waits at the head of the queue while they
//! type a line here, and says so to the daemon once a window. Nothing is
//! printed: the terminal is the person's while they type.
//!
//! The terminal is read while the bridge waits on its other calls
//! ([`Keyboard::during`]) and drained before every decision, and a new
//! turn's own preparation is followed by a second decision, so a line
//! started while a call is in flight is never missed.
use agentdocker_core::{AgentRecord, Envelope, HUMAN, InputReport, typing};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use std::future::Future;
use tokio::io::AsyncBufRead;

use super::terminal::Keyboard;
use crate::client::Backend;

pub(super) struct Guard<C> {
    /// The person's own record: their lines, from this terminal or anywhere
    /// else, are theirs and never wait.
    person: String,
    /// How often the bridge looks at its queue.
    every: Duration,
    clock: C,
    /// The end of the hold last shown to the daemon.
    announced: Option<DateTime<Utc>>,
}

impl<C: Fn() -> DateTime<Utc>> Guard<C> {
    pub fn new(person: String, every: Duration, clock: C) -> Self {
        Self {
            person,
            every,
            clock,
            announced: None,
        }
    }

    /// Whether `message`, at the head of the queue, waits for the person's
    /// typing. What has arrived on the terminal is read first, so the
    /// decision is made on every keystroke so far. Showing the hold is best
    /// effort: a daemon that cannot record it does not stop it.
    pub async fn hold<B: Backend, R: AsyncBufRead + Unpin>(
        &mut self,
        backend: &B,
        keyboard: &mut Keyboard<R>,
        agent: &AgentRecord,
        message: &Envelope,
    ) -> bool {
        if message.from == HUMAN || message.from == self.person {
            return false;
        }
        keyboard.drain().await;
        let now = (self.clock)();
        let typing::Hold::Until(until) = typing::decide(keyboard.typed_at(), now) else {
            self.announced = None;
            return false;
        };
        if typing::announce(self.announced, until, now, self.every)
            && keyboard
                .during(crate::input_status::refresh_report(
                    backend,
                    agent.id.as_str(),
                    agent.process_started_at,
                    InputReport::HeldForTyping { until },
                ))
                .await
        {
            self.announced = Some(until);
        }
        true
    }

    /// Whether a new turn may start with `message` now. The person's typing
    /// is checked before `prepare` (the start's own calls) and again after
    /// it, with the terminal read throughout, since they may start a line
    /// while it runs; then `abandon` takes back what `prepare` reported and
    /// the message stays at the head of the queue. Nothing is awaited
    /// between a true answer and the caller sending the turn.
    pub async fn ready_to_start<B, R, P, A>(
        &mut self,
        backend: &B,
        keyboard: &mut Keyboard<R>,
        agent: &AgentRecord,
        message: &Envelope,
        prepare: P,
        abandon: A,
    ) -> Result<bool>
    where
        B: Backend,
        R: AsyncBufRead + Unpin,
        P: Future<Output = Result<()>>,
        A: Future<Output = Result<()>>,
    {
        if self.hold(backend, keyboard, agent, message).await {
            return Ok(false);
        }
        keyboard.during(prepare).await?;
        if self.hold(backend, keyboard, agent, message).await {
            keyboard.during(abandon).await?;
            return Ok(false);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::mock::Mock;
    use agentdocker_core::{AgentSpec, AgentStatus, Destination, MessageId, Request, Response};
    use std::{
        cell::Cell,
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };
    use tokio::{
        io::{AsyncWriteExt, BufReader, DuplexStream},
        sync::Semaphore,
        time::sleep,
    };

    const EVERY: Duration = Duration::milliseconds(500);

    fn base() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn agent() -> AgentRecord {
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: "codex-1".into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            true,
            base(),
        );
        agent.status = AgentStatus::Running;
        agent.process_started_at = Some(base());
        agent
    }

    fn from(sender: &str, text: &str) -> Envelope {
        Envelope::new(
            sender,
            Destination::Agent("codex-1".into()),
            "chat",
            serde_json::json!(text),
            None,
            base(),
        )
    }

    /// The daemon as the bridge sees it: its queue, whose reads can be held
    /// back until the test lets one through, and everything else answered
    /// by the mock.
    struct Daemon {
        mock: Mock,
        queue: Mutex<VecDeque<Envelope>>,
        slow: AtomicBool,
        gate: Semaphore,
    }

    impl Daemon {
        fn with(messages: Vec<Envelope>, mock: Mock) -> Self {
            Self {
                mock,
                queue: Mutex::new(messages.into()),
                slow: AtomicBool::new(false),
                gate: Semaphore::new(0),
            }
        }

        /// The queue read the bridge makes.
        async fn read(&self) -> Vec<Envelope> {
            match self
                .call(Request::ProviderInbox {
                    agent: "codex-1".into(),
                    acknowledge: Vec::new(),
                    token: None,
                })
                .await
                .unwrap()
            {
                Response::Messages { messages } => messages,
                other => panic!("{other:?}"),
            }
        }

        /// A receipt acknowledged: the head, and only the head, leaves.
        fn acknowledge(&self, id: &MessageId) {
            let mut queue = self.queue.lock().unwrap();
            assert_eq!(queue.front().map(|m| &m.id), Some(id), "the head, in order");
            queue.pop_front();
        }

        fn holds_reported(&self) -> usize {
            self.mock
                .requests()
                .iter()
                .filter(|request| {
                    matches!(
                        request,
                        Request::ReportInput {
                            report: InputReport::HeldForTyping { .. },
                            ..
                        }
                    )
                })
                .count()
        }
    }

    impl Backend for Daemon {
        async fn call(&self, request: Request) -> Result<Response> {
            if let Request::ProviderInbox { .. } = request {
                if self.slow.load(Ordering::SeqCst) {
                    self.gate.acquire().await.unwrap().forget();
                }
                let messages = self.queue.lock().unwrap().iter().cloned().collect();
                return Ok(Response::Messages { messages });
            }
            self.mock.call(request).await
        }
    }

    fn keyboard() -> (DuplexStream, Keyboard<BufReader<DuplexStream>>) {
        let (keys, terminal) = tokio::io::duplex(1024);
        (keys, Keyboard::new(BufReader::new(terminal), true, true))
    }

    /// Bytes typed while a call is in flight; the call is let through only
    /// after they have had time to arrive.
    async fn type_during(keys: &mut DuplexStream, bytes: &[u8], release: impl FnOnce()) {
        keys.write_all(bytes).await.unwrap();
        sleep(std::time::Duration::from_millis(20)).await;
        release();
    }

    /// The start path: the person starts a line while the queue read is
    /// in flight, and types again while the start's own preparation is;
    /// neither starts a turn. Once they press Enter, each message starts
    /// one, in order, once, and their line is kept to be queued.
    #[tokio::test]
    async fn typing_while_a_queue_read_or_a_preparation_is_in_flight_holds_a_new_turn() {
        let (a, b) = (from("peer", "first"), from("peer", "second"));
        let daemon = Daemon::with(vec![a.clone(), b.clone()], Mock::default());
        let agent = agent();
        // How far the guard's clock runs ahead of the keystrokes' own.
        let ahead = Cell::new(Duration::zero());
        let mut guard = Guard::new("person".into(), EVERY, || Utc::now() + ahead.get());
        let (mut keys, mut keyboard) = keyboard();
        let prepared = Cell::new(0);
        let abandoned = Cell::new(0);

        // 1. Nothing typed when the read begins; a line starts while it is
        //    held back, and it returns only after.
        daemon.slow.store(true, Ordering::SeqCst);
        let (messages, ()) = tokio::join!(
            keyboard.during(daemon.read()),
            type_during(&mut keys, b"half a li", || daemon.gate.add_permits(1)),
        );
        daemon.slow.store(false, Ordering::SeqCst);
        let go = guard
            .ready_to_start(
                &daemon,
                &mut keyboard,
                &agent,
                &messages[0],
                async {
                    prepared.set(prepared.get() + 1);
                    Ok(())
                },
                async { Ok(()) },
            )
            .await
            .unwrap();
        assert!(!go, "held: the line began while the read was in flight");
        assert_eq!(prepared.get(), 0, "nothing of the start was begun");
        assert_eq!(daemon.holds_reported(), 1);

        // 2. A window on, quiet: the preparation runs, and the person types
        //    again while it is in flight. The last check sees it: no turn,
        //    and what the preparation reported is taken back.
        ahead.set(typing::QUIET);
        let messages = daemon.read().await;
        let gate = Semaphore::new(0);
        let (go, ()) = tokio::join!(
            guard.ready_to_start(
                &daemon,
                &mut keyboard,
                &agent,
                &messages[0],
                async {
                    prepared.set(prepared.get() + 1);
                    ahead.set(Duration::zero());
                    gate.acquire().await.unwrap().forget();
                    Ok(())
                },
                async {
                    abandoned.set(abandoned.get() + 1);
                    Ok(())
                },
            ),
            type_during(&mut keys, b"ne", || gate.add_permits(1)),
        );
        assert!(!go.unwrap(), "held: typed while the preparation ran");
        assert_eq!((prepared.get(), abandoned.get()), (1, 1));

        // 3. Enter: the line is the person's and holds nothing. Each message
        //    then starts its turn, in order, once.
        keys.write_all(b"\n").await.unwrap();
        sleep(std::time::Duration::from_millis(20)).await;
        let mut started: Vec<MessageId> = Vec::new();
        for _ in 0..4 {
            let messages = daemon.read().await;
            let Some(head) = messages.first() else { break };
            if guard
                .ready_to_start(
                    &daemon,
                    &mut keyboard,
                    &agent,
                    head,
                    async { Ok(()) },
                    async { Ok(()) },
                )
                .await
                .unwrap()
            {
                started.push(head.id.clone());
                daemon.acknowledge(&head.id);
            }
        }
        assert_eq!(started, vec![a.id, b.id]);
        assert_eq!(keyboard.next_line().as_deref(), Some("half a line"));
        assert_eq!(keyboard.next_line(), None);
    }

    /// The steer path: a line started while the queue read is in flight
    /// holds the steer, without a second report within the window; once
    /// the person has stopped, each message steers, in order, once.
    #[tokio::test]
    async fn typing_while_a_queue_read_is_in_flight_holds_a_steer() {
        let (a, b) = (from("peer", "first"), from("agentd", "second"));
        let daemon = Daemon::with(vec![a.clone(), b.clone()], Mock::default());
        let agent = agent();
        let ahead = Cell::new(Duration::zero());
        let mut guard = Guard::new("person".into(), EVERY, || Utc::now() + ahead.get());
        let (mut keys, mut keyboard) = keyboard();

        daemon.slow.store(true, Ordering::SeqCst);
        let (messages, ()) = tokio::join!(
            keyboard.during(daemon.read()),
            type_during(&mut keys, b"wait, one more th", || daemon
                .gate
                .add_permits(1)),
        );
        daemon.slow.store(false, Ordering::SeqCst);
        for _ in 0..2 {
            assert!(
                guard
                    .hold(&daemon, &mut keyboard, &agent, &messages[0])
                    .await,
                "held: typed while the read was in flight"
            );
        }
        assert_eq!(daemon.holds_reported(), 1, "once a window");

        ahead.set(typing::QUIET);
        let mut steered: Vec<MessageId> = Vec::new();
        for _ in 0..4 {
            let messages = daemon.read().await;
            let Some(head) = messages.first() else { break };
            if !guard.hold(&daemon, &mut keyboard, &agent, head).await {
                steered.push(head.id.clone());
                daemon.acknowledge(&head.id);
            }
        }
        assert_eq!(steered, vec![a.id, b.id]);
        assert_eq!(keyboard.next_line(), None, "the line is still a draft");
    }

    #[tokio::test]
    async fn the_person_s_own_words_never_wait_and_an_unrecorded_hold_still_holds() {
        let agent = agent();
        let mut guard = Guard::new("person".into(), EVERY, Utc::now);
        let (mut keys, mut keyboard) = keyboard();
        keys.write_all(b"typing").await.unwrap();
        sleep(std::time::Duration::from_millis(20)).await;
        let daemon = Daemon::with(Vec::new(), Mock::default());
        for sender in [HUMAN, "person"] {
            assert!(
                !guard
                    .hold(&daemon, &mut keyboard, &agent, &from(sender, "mine"))
                    .await
            );
        }
        assert!(daemon.mock.requests().is_empty());
        // A daemon that cannot record the hold: it holds all the same, and
        // asks again next time; once recorded, not again this window.
        let refusing = Daemon::with(
            Vec::new(),
            Mock::with(vec![
                Response::error(agentdocker_core::ErrorCode::Invalid, "unknown report"),
                Response::Ok,
            ]),
        );
        let peer = from("peer", "theirs");
        for _ in 0..3 {
            assert!(guard.hold(&refusing, &mut keyboard, &agent, &peer).await);
        }
        assert_eq!(refusing.holds_reported(), 2);
    }
}
