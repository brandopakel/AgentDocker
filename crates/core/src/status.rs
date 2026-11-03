//! What an agent is up to, in three parts, and how well that is known.
//!
//! One word was never enough. "Idle" said both *between turns, nothing to
//! do* and *stopped at a prompt nobody has answered*; "working" said both
//! *on a task* and *a turn is running this second*. Claude Code's own view
//! of an agent splits them: a **state** — where it stands with its task
//! (working on it, blocked, done, failed, stopped) — a **status** — what
//! it is doing this moment (busy, waiting, idle) — and, when it waits,
//! **what for** from the person (a permission prompt, input, a sandbox
//! request, a question). This module is that split for AgentDocker, from
//! the observations the daemon already records, and nothing else.
//!
//! Every observation is also ranked by how good it is. A provider's own
//! lifecycle event — a permission prompt opening, a question dialog, a
//! turn ending — is **strong**: first-hand, from the process itself. An
//! idle timer, herdr's reading of a terminal, or the absence of any fresh
//! report is **weak**: an inference. [`derive`] ranks the inputs so that
//! strong evidence stands over weak, and a weak sign of idleness never
//! becomes a wait for the person's input: only a strong report, a standing
//! screen judgment or an open question does that.
//!
//! Pure: no clock of its own, no I/O. The daemon passes `now` and what it
//! recorded, and the same function answers for `activity`, for the
//! `agent_status_changed` event and for every chip the app shows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AgentStatus;
use crate::judgment::ScreenJudgment;
use crate::wait::{ActivityObservation, ReportedActivity, WaitReason};

/// Where an agent stands with its task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Live and on its task — whatever it is doing this second.
    Working,
    /// Live, and held up by something another agent holds: a lease it
    /// waits in the queue for.
    Blocked,
    /// Its process ended cleanly.
    Done,
    /// It failed, or its process ended with an error.
    Failed,
    /// It was stopped: a stop was asked, or its process ended on a
    /// signal.
    Stopped,
}

impl State {
    /// The word on its chip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Blocked => "Blocked",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }

    /// The word for a column.
    pub fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    /// Whether the agent is still there.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Working | Self::Blocked)
    }
}

/// What an agent is doing this moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A turn is running.
    Busy,
    /// Stopped for something: the person (then [`WaitingFor`] says what)
    /// or, with nothing to wait for from them, another agent's lease.
    Waiting,
    /// Between turns, or gone.
    Idle,
}

impl Status {
    /// The word on its chip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Busy => "Busy",
            Self::Waiting => "Waiting",
            Self::Idle => "Idle",
        }
    }

    /// The word for a column.
    pub fn word(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
        }
    }
}

/// What an agent waits for from the person, when it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingFor {
    /// A permission prompt: a tool run it wants allowed.
    Permission,
    /// Input in its own terminal: a question dialog, a choice, words left
    /// unsent at its prompt.
    Input,
    /// A sandbox request: something its sandbox will not let it do until
    /// the person allows it.
    Sandbox,
    /// An answer to a question it asked through AgentDocker.
    Question,
    /// Nothing from the person.
    None,
}

impl WaitingFor {
    /// The words on its chip; empty for nothing, which has no chip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Permission => "Wants permission",
            Self::Input => "Needs input",
            Self::Sandbox => "Sandbox request",
            Self::Question => "Has a question",
            Self::None => "",
        }
    }

    /// The word for a column.
    pub fn word(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Input => "input",
            Self::Sandbox => "sandbox",
            Self::Question => "question",
            Self::None => "-",
        }
    }

    /// What a reported wait is for, as this model names it: a wait whose
    /// reason the adapter could not tell, a question dialog and an unsent
    /// draft all want the person's input in the agent's terminal.
    pub fn from_reason(reason: Option<WaitReason>) -> Self {
        match reason {
            Some(WaitReason::Permission) => Self::Permission,
            Some(WaitReason::Sandbox) => Self::Sandbox,
            Some(WaitReason::Question | WaitReason::Draft | WaitReason::Other) | None => {
                Self::Input
            }
        }
    }
}

/// How good the word on an agent is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// First-hand: the provider's own lifecycle event (a prompt opening, a
    /// turn ending), the conversation's own server, a question the daemon
    /// holds, a lease it granted, a screen the judge read with confidence.
    Strong,
    /// Inferred: an idle timer, a terminal's look as herdr reads it,
    /// contact with the daemon, or no fresh report at all.
    Weak,
}

impl Evidence {
    pub fn word(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Weak => "weak",
        }
    }
}

/// The three-part answer, and how good it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdentityStatus {
    pub state: State,
    pub status: Status,
    pub waiting_for: WaitingFor,
    /// How good the evidence behind `status` and `waiting_for` is: that of
    /// whichever input decided them.
    pub evidence: Evidence,
}

/// The agent's lifecycle as the registry keeps it, reduced to what the
/// model needs of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    /// Registered, no process yet.
    Starting,
    /// Running.
    Live,
    /// Exited cleanly.
    Done,
    /// Failed, or exited with an error.
    Failed,
    /// Stopped by the person, or ended on a signal.
    Stopped,
}

impl From<&AgentStatus> for Lifecycle {
    fn from(status: &AgentStatus) -> Self {
        match status {
            AgentStatus::Created => Self::Starting,
            AgentStatus::Running => Self::Live,
            AgentStatus::Stopping | AgentStatus::Exited { code: None } => Self::Stopped,
            AgentStatus::Exited { code: Some(0) } => Self::Done,
            AgentStatus::Exited { code: Some(_) } | AgentStatus::Failed { .. } => Self::Failed,
        }
    }
}

impl Lifecycle {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Starting | Self::Live)
    }
}

/// What the daemon has recorded of one agent: every input [`derive`]
/// ranks. Each is a fact the daemon holds, none a reading of the agent's
/// words.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    pub lifecycle: Lifecycle,
    /// It waits in the lease queue.
    pub blocked: bool,
    /// It asked the person a question through AgentDocker that is still
    /// open.
    pub open_question: bool,
    /// The observation that counts for it — its own adapter's, herdr's or
    /// Codex's shared server's — whether or not it is still current at
    /// `now`. An expired one says only that nothing fresh is known.
    pub observation: Option<&'a ActivityObservation>,
    /// A screen judgment that still stands for it.
    pub judgment: Option<&'a ScreenJudgment>,
    /// It acted through the daemon recently, and no observation counts
    /// for it: the only sign of work there is.
    pub contact: bool,
    pub now: DateTime<Utc>,
}

/// The three-part answer from what was recorded, first match wins.
///
/// **What it waits for from the person.** An open question first: the
/// daemon holds it, so nothing outranks it, and it stays until answered
/// or expired even once the agent is gone. An agent that is gone waits
/// for nothing else. Then a current observation that says *waiting*, by
/// its reason, with the observation's own evidence (strong from the
/// agent's adapter or the conversation's server, weak from herdr). Then
/// a standing screen judgment, which the judge records only when its
/// reading cleared its confidence bar, so it is strong. Otherwise
/// nothing.
///
/// **State.** The lifecycle's word once the agent has ended; a live agent
/// in the lease queue is blocked; any other live agent is working.
///
/// **Status.** Waiting whenever it waits for the person or for a lease
/// (then with nothing from the person); idle once gone; busy while
/// starting, weakly, since nothing has reported yet. Then the current
/// observation's own word — a turn running is busy, a turn ended is
/// idle — with its evidence; then contact alone as weak busy; and no
/// fresh word at all as weak idle, which is what *unknown* amounts to
/// here and why the evidence travels with the answer.
///
/// So a weak idle — an idle timer, a quiet terminal — can only ever make
/// *idle*: it never puts the agent in *waiting* for anything, and an
/// adapter's current strong word keeps its own weak one off the record in
/// the first place ([`ActivityObservation::keeps_off`]).
pub fn derive(inputs: &Inputs<'_>) -> IdentityStatus {
    let Inputs {
        lifecycle,
        blocked,
        open_question,
        observation,
        judgment,
        contact,
        now,
    } = *inputs;
    let live = lifecycle.is_live();
    let current = observation.filter(|observation| observation.current(now).is_some());
    let (waiting_for, wait_evidence) = if open_question {
        (WaitingFor::Question, Evidence::Strong)
    } else if !live {
        (WaitingFor::None, Evidence::Strong)
    } else if let Some(wait) =
        current.filter(|observation| observation.activity == ReportedActivity::Waiting)
    {
        (WaitingFor::from_reason(wait.reason), wait.evidence())
    } else if let Some(judgment) = judgment {
        (
            WaitingFor::from_reason(Some(judgment.reason)),
            judgment.evidence(),
        )
    } else {
        (WaitingFor::None, Evidence::Strong)
    };
    let state = match lifecycle {
        Lifecycle::Done => State::Done,
        Lifecycle::Failed => State::Failed,
        Lifecycle::Stopped => State::Stopped,
        Lifecycle::Starting | Lifecycle::Live if blocked => State::Blocked,
        Lifecycle::Starting | Lifecycle::Live => State::Working,
    };
    let (status, evidence) = if waiting_for != WaitingFor::None {
        (Status::Waiting, wait_evidence)
    } else if !live {
        (Status::Idle, Evidence::Strong)
    } else if blocked {
        (Status::Waiting, Evidence::Strong)
    } else if lifecycle == Lifecycle::Starting {
        (Status::Busy, Evidence::Weak)
    } else if let Some(observation) = current {
        let status = match observation.activity {
            ReportedActivity::Working => Status::Busy,
            ReportedActivity::Idle => Status::Idle,
            // Unreachable while a wait is handled above; said anyway.
            ReportedActivity::Waiting => Status::Waiting,
        };
        (status, observation.evidence())
    } else if contact {
        (Status::Busy, Evidence::Weak)
    } else {
        (Status::Idle, Evidence::Weak)
    };
    IdentityStatus {
        state,
        status,
        waiting_for,
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wait::ActivitySource;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn observed(
        activity: ReportedActivity,
        reason: Option<WaitReason>,
        source: Option<ActivitySource>,
        evidence: Option<Evidence>,
        age_secs: i64,
    ) -> ActivityObservation {
        ActivityObservation {
            activity,
            observed_at: now() - chrono::Duration::seconds(age_secs),
            reason,
            detail: None,
            source,
            evidence,
        }
    }

    fn judged(reason: WaitReason, permille: u16) -> ScreenJudgment {
        ScreenJudgment {
            reason,
            permille,
            since: now(),
            judged_at: now(),
            process_started_at: None,
            screen: "digest".into(),
            model: "model".into(),
        }
    }

    fn live<'a>(observation: Option<&'a ActivityObservation>) -> Inputs<'a> {
        Inputs {
            lifecycle: Lifecycle::Live,
            blocked: false,
            open_question: false,
            observation,
            judgment: None,
            contact: false,
            now: now(),
        }
    }

    fn triple(
        state: State,
        status: Status,
        waiting_for: WaitingFor,
        evidence: Evidence,
    ) -> IdentityStatus {
        IdentityStatus {
            state,
            status,
            waiting_for,
            evidence,
        }
    }

    /// The lifecycle alone decides the state of an agent that has ended,
    /// and such an agent is idle with strong evidence, waiting for nothing
    /// — unless a question of its is still open.
    #[test]
    fn ended_agents_take_their_state_from_the_lifecycle() {
        use ReportedActivity::Working;
        let working = observed(Working, None, None, None, 1);
        for (lifecycle, state) in [
            (Lifecycle::Done, State::Done),
            (Lifecycle::Failed, State::Failed),
            (Lifecycle::Stopped, State::Stopped),
        ] {
            // Even a fresh working report and a judgment say nothing once
            // the process is gone.
            let judgment = judged(WaitReason::Permission, 950);
            let inputs = Inputs {
                lifecycle,
                observation: Some(&working),
                judgment: Some(&judgment),
                blocked: false,
                contact: true,
                ..live(None)
            };
            assert_eq!(
                derive(&inputs),
                triple(state, Status::Idle, WaitingFor::None, Evidence::Strong)
            );
            assert_eq!(
                derive(&Inputs {
                    open_question: true,
                    ..inputs
                }),
                triple(
                    state,
                    Status::Waiting,
                    WaitingFor::Question,
                    Evidence::Strong
                ),
                "a question stays open after its asker ended"
            );
        }
        assert_eq!(Lifecycle::from(&AgentStatus::Created), Lifecycle::Starting);
        assert_eq!(Lifecycle::from(&AgentStatus::Running), Lifecycle::Live);
        assert_eq!(Lifecycle::from(&AgentStatus::Stopping), Lifecycle::Stopped);
        assert_eq!(
            Lifecycle::from(&AgentStatus::Exited { code: Some(0) }),
            Lifecycle::Done
        );
        assert_eq!(
            Lifecycle::from(&AgentStatus::Exited { code: Some(2) }),
            Lifecycle::Failed
        );
        assert_eq!(
            Lifecycle::from(&AgentStatus::Exited { code: None }),
            Lifecycle::Stopped
        );
        assert_eq!(
            Lifecycle::from(&AgentStatus::Failed {
                reason: "spawn".into()
            }),
            Lifecycle::Failed
        );
    }

    /// The ranking table for a live agent, row by row: what the person is
    /// waited for, strongest claim first, and the status under it.
    #[test]
    fn the_ranking_table_for_a_live_agent() {
        use ReportedActivity::{Idle, Waiting, Working};
        let permission = observed(Waiting, Some(WaitReason::Permission), None, None, 1);
        let dialog = observed(Waiting, Some(WaitReason::Question), None, None, 1);
        let unsaid = observed(Waiting, None, None, None, 1);
        let sandbox = observed(Waiting, Some(WaitReason::Sandbox), None, None, 1);
        let herdr_wait = observed(
            Waiting,
            Some(WaitReason::Other),
            Some(ActivitySource::Herdr),
            None,
            1,
        );
        let server_wait = observed(
            Waiting,
            Some(WaitReason::Permission),
            Some(ActivitySource::CodexServer),
            None,
            1,
        );
        let working = observed(Working, None, None, None, 1);
        let stopped = observed(Idle, None, None, None, 1);
        let timer = observed(Idle, None, None, Some(Evidence::Weak), 1);
        let herdr_idle = observed(Idle, None, Some(ActivitySource::Herdr), None, 1);
        let herdr_busy = observed(Working, None, Some(ActivitySource::Herdr), None, 1);
        let expired = observed(Working, None, None, None, 6 * 60);
        let judgment = judged(WaitReason::Draft, 800);

        let w = State::Working;
        // An open question outranks every other claim, a running turn included.
        assert_eq!(
            derive(&Inputs {
                open_question: true,
                ..live(Some(&working))
            }),
            triple(w, Status::Waiting, WaitingFor::Question, Evidence::Strong)
        );
        // The adapter's own waits are strong, by reason.
        assert_eq!(
            derive(&live(Some(&permission))),
            triple(w, Status::Waiting, WaitingFor::Permission, Evidence::Strong)
        );
        assert_eq!(
            derive(&live(Some(&dialog))),
            triple(w, Status::Waiting, WaitingFor::Input, Evidence::Strong)
        );
        assert_eq!(
            derive(&live(Some(&unsaid))),
            triple(w, Status::Waiting, WaitingFor::Input, Evidence::Strong),
            "a wait with no reason still wants the person in its terminal"
        );
        assert_eq!(
            derive(&live(Some(&sandbox))),
            triple(w, Status::Waiting, WaitingFor::Sandbox, Evidence::Strong)
        );
        // The conversation's own server is as good as the adapter; herdr's
        // reading of a terminal is not.
        assert_eq!(
            derive(&live(Some(&server_wait))),
            triple(w, Status::Waiting, WaitingFor::Permission, Evidence::Strong)
        );
        assert_eq!(
            derive(&live(Some(&herdr_wait))),
            triple(w, Status::Waiting, WaitingFor::Input, Evidence::Weak)
        );
        // A standing judgment is a confident read: it stands over a
        // working or idle word, as it always has.
        for quiet in [
            Some(&working),
            Some(&stopped),
            Some(&timer),
            Some(&expired),
            None,
        ] {
            assert_eq!(
                derive(&Inputs {
                    judgment: Some(&judgment),
                    ..live(quiet)
                }),
                triple(w, Status::Waiting, WaitingFor::Input, Evidence::Strong),
                "{quiet:?}"
            );
        }
        // But a first-hand wait, by its own reason, over the judgment's.
        assert_eq!(
            derive(&Inputs {
                judgment: Some(&judgment),
                ..live(Some(&permission))
            })
            .waiting_for,
            WaitingFor::Permission
        );
        // The lease queue: blocked, waiting for nothing from the person.
        assert_eq!(
            derive(&Inputs {
                blocked: true,
                ..live(Some(&working))
            }),
            triple(
                State::Blocked,
                Status::Waiting,
                WaitingFor::None,
                Evidence::Strong
            )
        );
        // In the queue and at a prompt at once: both are said.
        assert_eq!(
            derive(&Inputs {
                blocked: true,
                ..live(Some(&permission))
            }),
            triple(
                State::Blocked,
                Status::Waiting,
                WaitingFor::Permission,
                Evidence::Strong
            )
        );
        // Starting: busy on trust.
        assert_eq!(
            derive(&Inputs {
                lifecycle: Lifecycle::Starting,
                ..live(Some(&working))
            }),
            triple(w, Status::Busy, WaitingFor::None, Evidence::Weak)
        );
        // A turn running, a turn ended: the adapter's word, strong.
        assert_eq!(
            derive(&live(Some(&working))),
            triple(w, Status::Busy, WaitingFor::None, Evidence::Strong)
        );
        assert_eq!(
            derive(&live(Some(&stopped))),
            triple(w, Status::Idle, WaitingFor::None, Evidence::Strong)
        );
        // Weak idles are idle, weakly, and never a wait for input.
        for weak in [&timer, &herdr_idle] {
            assert_eq!(
                derive(&live(Some(weak))),
                triple(w, Status::Idle, WaitingFor::None, Evidence::Weak)
            );
        }
        assert_eq!(
            derive(&live(Some(&herdr_busy))),
            triple(w, Status::Busy, WaitingFor::None, Evidence::Weak)
        );
        // An expired word is no word: contact alone is weak busy, and
        // nothing at all is weak idle.
        assert_eq!(
            derive(&Inputs {
                contact: true,
                ..live(Some(&expired))
            }),
            triple(w, Status::Busy, WaitingFor::None, Evidence::Weak)
        );
        assert_eq!(
            derive(&live(Some(&expired))),
            triple(w, Status::Idle, WaitingFor::None, Evidence::Weak)
        );
        assert_eq!(
            derive(&live(None)),
            triple(w, Status::Idle, WaitingFor::None, Evidence::Weak)
        );
    }

    /// Every reported reason has a place in the model, and none of them
    /// is a question put through AgentDocker.
    #[test]
    fn reasons_map_onto_what_is_waited_for() {
        assert_eq!(
            WaitingFor::from_reason(Some(WaitReason::Permission)),
            WaitingFor::Permission
        );
        assert_eq!(
            WaitingFor::from_reason(Some(WaitReason::Sandbox)),
            WaitingFor::Sandbox
        );
        for reason in [
            Some(WaitReason::Question),
            Some(WaitReason::Draft),
            Some(WaitReason::Other),
            None,
        ] {
            assert_eq!(WaitingFor::from_reason(reason), WaitingFor::Input);
        }
    }

    /// The wire names, and the words every surface prints.
    #[test]
    fn the_words_and_the_wire() {
        let status = triple(
            State::Blocked,
            Status::Waiting,
            WaitingFor::Sandbox,
            Evidence::Weak,
        );
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "state": "blocked",
                "status": "waiting",
                "waiting_for": "sandbox",
                "evidence": "weak",
            })
        );
        assert_eq!(serde_json::to_value(WaitingFor::None).unwrap(), "none");
        assert_eq!(State::Working.label(), "Working");
        assert_eq!(Status::Busy.word(), "busy");
        assert_eq!(WaitingFor::Input.label(), "Needs input");
        assert_eq!(WaitingFor::None.label(), "");
        assert_eq!(WaitingFor::None.word(), "-");
        assert_eq!(Evidence::Strong.word(), "strong");
        assert!(State::Blocked.is_live() && !State::Stopped.is_live());
    }
}
