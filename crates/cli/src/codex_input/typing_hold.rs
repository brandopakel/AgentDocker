//! The bridge's half of [`agentdocker_core::typing`]. Its editor reads the
//! person's keystrokes, so the bridge keeps the clock: a queued message
//! from anyone but the person waits at the head of the queue while they
//! type a line here, and says so to the daemon once a window. Nothing is
//! printed: the terminal is the person's while they type.
use agentdocker_core::{AgentRecord, Envelope, HUMAN, InputReport, typing};
use chrono::{DateTime, Duration, Utc};

use crate::client::Backend;

#[derive(Default)]
pub(super) struct Guard {
    /// The end of the hold last shown to the daemon.
    announced: Option<DateTime<Utc>>,
}

impl Guard {
    /// Whether `message`, at the head of the queue, waits for the person's
    /// typing (`typed_at`, the last keystroke of their unfinished line).
    /// `person` is the person's own record: their lines, from this terminal
    /// or anywhere else, are theirs and never wait. `every` is how often the
    /// bridge asks again. Showing the hold is best effort: a daemon that
    /// cannot record it does not stop it.
    #[allow(clippy::too_many_arguments)]
    pub async fn holds<B: Backend>(
        &mut self,
        backend: &B,
        agent: &AgentRecord,
        person: &str,
        message: &Envelope,
        typed_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
        every: Duration,
    ) -> bool {
        if message.from == HUMAN || message.from == person {
            return false;
        }
        let typing::Hold::Until(until) = typing::decide(typed_at, now) else {
            self.announced = None;
            return false;
        };
        if typing::announce(self.announced, until, now, every)
            && crate::input_status::refresh_report(
                backend,
                agent.id.as_str(),
                agent.process_started_at,
                InputReport::HeldForTyping { until },
            )
            .await
        {
            self.announced = Some(until);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::mock::Mock;
    use agentdocker_core::{AgentSpec, AgentStatus, Destination, Request, Response};

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::milliseconds(millis)
    }

    fn agent() -> AgentRecord {
        let mut agent = AgentRecord::new(
            AgentSpec {
                name: "codex-1".into(),
                runtime: "codex".into(),
                ..Default::default()
            },
            true,
            at(0),
        );
        agent.status = AgentStatus::Running;
        agent.process_started_at = Some(at(0));
        agent
    }

    fn from(sender: &str) -> Envelope {
        Envelope::new(
            sender,
            Destination::Agent("codex-1".into()),
            "chat",
            serde_json::json!("hello"),
            None,
            at(0),
        )
    }

    fn holds(requests: &[Request]) -> Vec<DateTime<Utc>> {
        requests
            .iter()
            .filter_map(|request| match request {
                Request::ReportInput {
                    report: InputReport::HeldForTyping { until },
                    ..
                } => Some(*until),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_peer_waits_while_the_person_types_and_is_shown_once_a_window() {
        let backend = Mock::default();
        let agent = agent();
        let every = Duration::milliseconds(500);
        let mut guard = Guard::default();
        let peer = from("peer-agent");
        // The person typed at 1 s; the bridge looks every half second.
        for now in [1_000, 1_500, 2_000, 2_500, 3_000, 3_500] {
            assert!(
                guard
                    .holds(
                        &backend,
                        &agent,
                        "person",
                        &peer,
                        Some(at(1_000)),
                        at(now),
                        every
                    )
                    .await,
                "{now}"
            );
        }
        assert_eq!(holds(&backend.requests()), vec![at(4_000)], "shown once");
        // Still typing at 3.6 s, as the window shown is about to lapse.
        assert!(
            guard
                .holds(
                    &backend,
                    &agent,
                    "person",
                    &peer,
                    Some(at(3_600)),
                    at(3_600),
                    every
                )
                .await
        );
        assert_eq!(holds(&backend.requests()), vec![at(4_000), at(6_600)]);
        // Quiet for a whole window: it goes, and nothing more is said.
        assert!(
            !guard
                .holds(
                    &backend,
                    &agent,
                    "person",
                    &peer,
                    Some(at(3_600)),
                    at(6_600),
                    every
                )
                .await
        );
        assert_eq!(holds(&backend.requests()).len(), 2);
        // A submitted line holds nothing.
        assert!(
            !guard
                .holds(&backend, &agent, "person", &peer, None, at(7_000), every)
                .await
        );
    }

    #[tokio::test]
    async fn the_person_s_own_words_never_wait_and_an_unrecorded_hold_still_holds() {
        let backend = Mock::default();
        let agent = agent();
        let every = Duration::milliseconds(500);
        let mut guard = Guard::default();
        for sender in [HUMAN, "person"] {
            assert!(
                !guard
                    .holds(
                        &backend,
                        &agent,
                        "person",
                        &from(sender),
                        Some(at(0)),
                        at(0),
                        every
                    )
                    .await
            );
        }
        assert!(backend.requests().is_empty());
        // A daemon that cannot record the hold: it holds all the same, and
        // asks again next time.
        let refusing = Mock::with(vec![
            Response::error(agentdocker_core::ErrorCode::Invalid, "unknown report"),
            Response::Ok,
        ]);
        let peer = from("peer-agent");
        for now in [0, 500] {
            assert!(
                guard
                    .holds(
                        &refusing,
                        &agent,
                        "person",
                        &peer,
                        Some(at(0)),
                        at(now),
                        every
                    )
                    .await
            );
        }
        assert_eq!(holds(&refusing.requests()), vec![at(3_000), at(3_000)]);
    }
}
