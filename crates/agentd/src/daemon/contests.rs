//! Contests: several agents attempt one task, and the evidence decides.
//!
//! The daemon's part is small and it is the part that has to be right.
//! It fixes the measure when the contest opens, so nobody can choose the
//! flattering number afterwards. It refuses any entry whose validation
//! did not pass, or whose validation belongs to another agent or another
//! checkout, so "best" can never mean "fastest to produce something
//! broken" or "quickest to borrow somebody else's green run". And where
//! the measure is one it took itself — how long a validation ran, which
//! it timed — it uses its own number and does not ask.
//!
//! Everything else is the channel's. A margin inside the declared noise
//! floor is a tie, and a tie is settled by what the other agents say
//! about the work, which is what row 21 is for.

use super::*;
use agentdocker_core::channel::{Channel, ChannelId, ChannelSubject};
use agentdocker_core::contest::{Contest, ContestId, Entry, EntryError, Measure, Metric, Standing};
use agentdocker_core::recovery::Validation;

impl State {
    fn contest(&mut self, id: &ContestId) -> Option<Contest> {
        self.store_op("contest", |store| store.document("contest", id.as_str()))
            .flatten()
    }

    fn save_contest(&mut self, contest: &Contest) {
        self.persist("contest", |store| {
            store.put_document("contest", contest.id.as_str(), contest)
        });
    }
}

impl Daemon {
    /// `contest_open`: announce the task and fix the measure.
    pub(super) fn contest_open(
        self: &Arc<Self>,
        reference: &str,
        project: Option<String>,
        task: String,
        metric: Metric,
        entrants: Vec<String>,
        with_channel: bool,
    ) -> Response {
        let task = task.trim().to_owned();
        if task.is_empty() {
            return Response::error(ErrorCode::Invalid, "a contest needs a task");
        }
        if !metric.noise.is_finite() || metric.noise < 0.0 {
            return Response::error(
                ErrorCode::Invalid,
                "the noise floor must be a finite, non-negative number",
            );
        }
        if let Measure::Reported { name } = &metric.measure
            && name.trim().is_empty()
        {
            return Response::error(
                ErrorCode::Invalid,
                "a reported measure needs a name, so every entrant reports the same one",
            );
        }
        let mut state = lock(&self.state);
        let opener = match state.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(record) = state.registry.get(&opener).cloned() else {
            return Response::error(ErrorCode::NotFound, "agent vanished");
        };
        let project = match project {
            Some(reference) => match state.registry.resolve_project(&reference) {
                Ok(id) => id,
                Err(err) => return registry_error(err),
            },
            None => match record.project.as_ref().map(ProjectRef::id) {
                Some(id) => id,
                None => return Response::error(ErrorCode::Invalid, "the agent is in no project"),
            },
        };

        let mut ids = vec![opener.clone()];
        for reference in &entrants {
            match state.resolve(reference) {
                Ok(id) if !ids.contains(&id) => ids.push(id),
                Ok(_) => {}
                Err(e) => return *e,
            }
        }
        let mut contest = Contest::new(
            project.clone(),
            task.clone(),
            metric.clone(),
            opener.to_string(),
            ids.clone(),
            Utc::now(),
        );

        // A room from the start: a tie has to be argued somewhere, and
        // asking for one after the numbers are in looks like a loser
        // asking for a rematch.
        if with_channel {
            let records: Vec<AgentRecord> = ids
                .iter()
                .filter_map(|id| state.registry.get(id).cloned())
                .collect();
            let channel = Channel {
                id: ChannelId::generate(),
                project: project.clone(),
                subject: ChannelSubject::Task {
                    task: format!("contest: {task}"),
                },
                members: ids.clone(),
                opened_by: Some(opener.clone()),
                opened_at: Utc::now(),
                reviews: Vec::new(),
                closed_at: None,
                resolution: None,
            };
            state.install_channel(channel.clone(), &records);
            state.tell_channel(
                &channel,
                format!(
                    "{} opened a contest: {task}. Ranked by {} ({}), anything within {} of the \
                     best is a tie for this channel to settle. Submit with a passing validation.",
                    record.spec.name,
                    metric.measure.name(),
                    match metric.direction {
                        agentdocker_core::contest::Direction::Lower => "lower is better",
                        agentdocker_core::contest::Direction::Higher => "higher is better",
                    },
                    metric.noise
                ),
            );
            contest.channel = Some(channel.id);
        }

        state.save_contest(&contest);
        state.emit(EventKind::ContestOpened {
            contest: contest.id.clone(),
            task,
            measure: metric.measure.name().to_owned(),
            entrants: ids,
        });
        if let Some(error) = state.storage_failure() {
            return error;
        }
        let standing = contest.standing();
        Response::Contest { contest, standing }
    }

    /// `contest_enter`: join one that is open.
    pub(super) fn contest_enter(self: &Arc<Self>, reference: &str, id: &ContestId) -> Response {
        let mut state = lock(&self.state);
        let agent = match state.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(mut contest) = state.contest(id) else {
            return Response::error(ErrorCode::NotFound, format!("no contest {id}"));
        };
        if !contest.is_open() {
            return Response::error(ErrorCode::Invalid, "the contest is closed");
        }
        if contest.enter(agent.clone()) {
            state.save_contest(&contest);
            state.emit(EventKind::ContestEntered {
                contest: contest.id.clone(),
                agent: agent.clone(),
            });
            if let Some(channel) = contest.channel.clone()
                && let Some(mut room) = state.channels.get(&channel).cloned()
                && room.admit(agent)
            {
                state.persist("channel", |store| {
                    store.put_document("channel", room.id.as_str(), &room)
                });
                state.channels.insert(room.id.clone(), room);
            }
        }
        if let Some(error) = state.storage_failure() {
            return error;
        }
        let standing = contest.standing();
        Response::Contest { contest, standing }
    }

    /// `contest_submit`: an attempt, with the evidence that it works.
    pub(super) fn contest_submit(
        self: &Arc<Self>,
        reference: &str,
        id: &ContestId,
        validation: &str,
        reported: Option<f64>,
    ) -> Response {
        let mut state = lock(&self.state);
        let agent = match state.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(mut contest) = state.contest(id) else {
            return Response::error(ErrorCode::NotFound, format!("no contest {id}"));
        };
        let evidence: Option<Validation> = state
            .store_op("validation", |store| {
                store.document("validation", validation)
            })
            .flatten();
        let Some(evidence) = evidence else {
            return Response::error(
                ErrorCode::NotFound,
                format!("no validation {validation}; run `validate` first"),
            );
        };
        // Provenance: the evidence has to be this agent's own run, in the
        // checkout it is submitting. Otherwise an entrant could hand in
        // somebody else's green result.
        if evidence.agent != agent {
            return Response::error(
                ErrorCode::Forbidden,
                format!(
                    "validation {validation} belongs to {}, not to you",
                    evidence.agent.short()
                ),
            );
        }
        if !evidence.passed() {
            return Response::error(
                ErrorCode::Invalid,
                format!(
                    "validation {validation} did not pass, so the entry cannot be ranked: {}",
                    describe_failure(&evidence)
                ),
            );
        }

        // A measure the daemon took itself is not open to report.
        let score = match &contest.metric.measure {
            Measure::ValidationSeconds => {
                (evidence.finished_at - evidence.started_at).num_milliseconds() as f64 / 1000.0
            }
            Measure::Reported { name } => match reported {
                Some(score) if score.is_finite() => score,
                Some(_) => {
                    return Response::error(ErrorCode::Invalid, "the score must be a real number");
                }
                None => {
                    return Response::error(
                        ErrorCode::Invalid,
                        format!("this contest is ranked by {name}; give a score"),
                    );
                }
            },
        };

        let entry = Entry {
            agent: agent.clone(),
            checkout: evidence.checkout.clone(),
            head: evidence.head.clone(),
            validation: validation.to_owned(),
            score,
            submitted_at: Utc::now(),
        };
        if let Err(err) = contest.submit(entry, true) {
            return Response::error(
                match err {
                    EntryError::NotEntered => ErrorCode::Forbidden,
                    _ => ErrorCode::Invalid,
                },
                err.to_string(),
            );
        }
        state.save_contest(&contest);
        state.emit(EventKind::ContestSubmitted {
            contest: contest.id.clone(),
            agent,
            validation: validation.to_owned(),
            score: format!("{score}"),
        });
        let standing = contest.standing();
        if let Some(channel) = contest.channel.clone()
            && let Some(room) = state.channels.get(&channel).cloned()
        {
            state.tell_channel(&room, standing_line(&contest, &standing));
        }
        if let Some(error) = state.storage_failure() {
            return error;
        }
        Response::Contest { contest, standing }
    }

    pub(super) fn contests(
        self: &Arc<Self>,
        contest: Option<ContestId>,
        project: Option<String>,
        agent: Option<String>,
        all: bool,
    ) -> Response {
        let mut state = lock(&self.state);
        // One id is a lookup, not a scan: `contest show` should not read
        // every contest a project has ever had in order to find one.
        if let Some(id) = contest {
            return match state.contest(&id) {
                Some(contest) => Response::Contests {
                    contests: vec![contest],
                },
                None => Response::error(ErrorCode::NotFound, format!("no contest {id}")),
            };
        }
        let of = match agent.as_deref() {
            Some(reference) => match state.resolve(reference) {
                Ok(id) => Some(id),
                Err(e) => return *e,
            },
            None => None,
        };
        let project = match project.as_deref() {
            Some(reference) => match state.registry.resolve_project(reference) {
                Ok(id) => Some(id),
                Err(err) => return registry_error(err),
            },
            None => None,
        };
        let mut contests: Vec<Contest> = state
            .store_op("contest", |store| {
                store.documents::<Contest>("contest", None)
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|c| all || c.is_open())
            .filter(|c| project.as_ref().is_none_or(|p| c.project == *p))
            .filter(|c| of.as_ref().is_none_or(|id| c.has(id)))
            .collect();
        contests.sort_by_key(|c| std::cmp::Reverse(c.opened_at));
        Response::Contests { contests }
    }

    /// `contest_close`: declare the answer.
    pub(super) fn contest_close(
        self: &Arc<Self>,
        reference: &str,
        id: &ContestId,
        winner: Option<String>,
        resolution: Option<String>,
    ) -> Response {
        let mut state = lock(&self.state);
        let closer = match state.resolve(reference) {
            Ok(id) => id,
            Err(e) => return *e,
        };
        let Some(mut contest) = state.contest(id) else {
            return Response::error(ErrorCode::NotFound, format!("no contest {id}"));
        };
        if !contest.is_open() {
            return Response::error(ErrorCode::Invalid, "the contest is already closed");
        }
        // Closing settles somebody's work, so it belongs to the people
        // in it — the opener, or an entrant. `channel_close` draws the
        // same boundary, and a contest without it lets any agent that
        // can resolve an id finalise a competition it is not part of.
        if !contest.has(&closer) && contest.opened_by != closer.to_string() {
            return Response::error(
                ErrorCode::Forbidden,
                "only the agent that opened this contest, or one of its entrants, can close it",
            );
        }
        let winner = match winner.as_deref() {
            Some(reference) => match state.resolve(reference) {
                Ok(id) => Some(id),
                Err(e) => return *e,
            },
            // No winner named: the ranking decides, which it can only do
            // when the metric actually separated them.
            None => match contest.standing() {
                Standing::Leader { agent, .. } => Some(agent),
                Standing::Tied { agents, .. } => {
                    return Response::error(
                        ErrorCode::Conflict,
                        format!(
                            "{} entries are inside the noise floor, so the metric cannot decide; \
                             review them in the channel and close with an explicit winner",
                            agents.len()
                        ),
                    );
                }
                _ => {
                    return Response::error(
                        ErrorCode::Invalid,
                        "no passing entries, so there is nothing to decide",
                    );
                }
            },
        };
        if let Some(winner) = &winner
            && contest.entry_of(winner).is_none()
        {
            return Response::error(
                ErrorCode::Invalid,
                "the winner has no passing entry in this contest",
            );
        }
        contest.winner = winner.clone();
        contest.resolution = resolution.clone();
        contest.closed_at = Some(Utc::now());
        state.save_contest(&contest);
        state.emit(EventKind::ContestClosed {
            contest: contest.id.clone(),
            winner: winner.clone(),
            resolution: resolution.clone(),
        });
        if let Some(channel) = contest.channel.clone()
            && let Some(room) = state.channels.get(&channel).cloned()
        {
            let name = |id: &AgentId| {
                state
                    .registry
                    .get(id)
                    .map(|a| a.spec.name.clone())
                    .unwrap_or_else(|| id.short().to_owned())
            };
            let text = match &winner {
                Some(winner) => format!(
                    "Contest settled: {} wins{}.",
                    name(winner),
                    resolution
                        .as_ref()
                        .map(|r| format!(" — {r}"))
                        .unwrap_or_default()
                ),
                None => "Contest closed with no winner.".to_owned(),
            };
            state.tell_channel(&room, text);
        }
        // The journal is where a project's decisions are read back, and a
        // contest result is one.
        if let Some(record) = state.registry.get(&closer).cloned() {
            let summary = match &winner {
                Some(winner) => format!(
                    "settled the contest on {}: {} wins{}",
                    contest.task,
                    winner.short(),
                    resolution
                        .as_ref()
                        .map(|r| format!(" ({r})"))
                        .unwrap_or_default()
                ),
                None => format!("closed the contest on {} with no winner", contest.task),
            };
            if let Some(entry) = state.plain_entry(
                &record,
                JournalKind::Review,
                summary,
                SummarySource::Synthesised,
            ) {
                state.append_journal(entry);
            }
        }
        if let Some(error) = state.storage_failure() {
            return error;
        }
        let standing = contest.standing();
        Response::Contest { contest, standing }
    }
}

/// Why a validation is not evidence of anything.
fn describe_failure(validation: &Validation) -> String {
    if let Some(error) = &validation.error {
        return error.clone();
    }
    if validation.timed_out {
        return "it timed out".to_owned();
    }
    if validation.descendants_survived {
        return "child processes were still running when it finished".to_owned();
    }
    if validation.after.as_deref() != Some(validation.before.as_str()) {
        return "the code changed while it ran".to_owned();
    }
    match validation.exit_code {
        Some(code) => format!("it exited {code}"),
        None => "it did not exit cleanly".to_owned(),
    }
}

/// Where a contest stands, in a sentence for the channel.
fn standing_line(contest: &Contest, standing: &Standing) -> String {
    let unit = contest.metric.measure.name();
    match standing {
        Standing::Open { entrants, entries } => {
            format!("{entries} of {entrants} entrants have submitted.")
        }
        Standing::Leader {
            agent,
            score,
            margin: Some(margin),
        } => format!(
            "{} leads on {unit} with {score}, {margin} clear of the next.",
            agent.short()
        ),
        Standing::Leader { agent, score, .. } => {
            format!(
                "{} is the only entry so far: {score} {unit}.",
                agent.short()
            )
        }
        Standing::Tied { agents, score } => format!(
            "{} are tied at {score} {unit}, inside the {} noise floor — review settles it.",
            agents
                .iter()
                .map(|a| a.short().to_owned())
                .collect::<Vec<_>>()
                .join(" and "),
            contest.metric.noise
        ),
        Standing::Settled { winner, .. } => format!("Settled: {} wins.", winner.short()),
    }
}
