use super::*;
use agentdocker_core::wait::{self, Priority};
use agentdocker_core::{Blocked, WaitOutcome};

impl Daemon {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn claim(
        self: &Arc<Self>,
        reference: &str,
        resource: String,
        mode: LeaseMode,
        amount: Option<u64>,
        ttl_secs: u64,
        note: Option<String>,
        wait_secs: u64,
        priority: Priority,
        automatic: bool,
    ) -> Response {
        if !wait::valid_priority(priority) {
            return Response::error(
                ErrorCode::Invalid,
                format!(
                    "priority must be from {} to {}, not {priority}",
                    wait::MIN_PRIORITY,
                    wait::MAX_PRIORITY
                ),
            );
        }
        let mut holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };

        let resource = match self
            .localise(ResourceKey::new(resource), Some(&holder), true)
            .await
        {
            Ok(resource) => resource,
            Err(response) => return *response,
        };
        // Asked once, before the first attempt: a policy answer does not
        // change while a claim waits, and re-asking would put a refusal
        // in the log for every retry.
        {
            let mut state = lock(&self.state);
            let action = format!("claim:{resource}");
            let ruling = state.permits(&holder, &action);
            if !ruling.is_allowed() {
                return state.refuse(&holder, &action, ruling);
            }
            // A paused project's agents take nothing new until the person
            // lifts it; what they hold, they keep.
            if let Some(pause) = state.pause_holding(&holder) {
                return pause_refusal(pause);
            }
        }
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(wait_secs.min(MAX_WAIT_SECS));
        // Subscribe before the first attempt so a release that lands between
        // a failed attempt and the wait is not missed.
        let mut events = self.subscribe_events();
        let mut reported_conflict = false;
        // The place in the queue, taken on the first conflict and given
        // up however this ends — including by the client disappearing,
        // which drops this future and with it the guard. A cancelled
        // claim left at the head of a queue would starve everyone
        // behind it.
        let mut waiting = waiting::Waiting::new(self, holder.clone(), resource.clone());
        loop {
            let attempt = {
                let mut state = lock(&self.state);
                // Another claim may have ended this wait to break a ring
                // since it last looked.
                if let Some(cycle) = waiting.deadlocked(&mut state) {
                    return ended_by_deadlock(&state, &resource, &cycle);
                }
                // From here to the end of this hold only this claim can end
                // its wait. Whatever the attempt answers, its place goes in
                // this same hold (`end_locked`, below), so a claim that has
                // its answer is never afterwards picked to break a ring.
                let attempt: Result<(String, Vec<Lease>), (Response, WaitOutcome)> = 'attempt: {
                    let gone = WaitOutcome::Cancelled;
                    if !state
                        .registry
                        .get(&holder)
                        .is_some_and(|a| a.status == AgentStatus::Running)
                    {
                        break 'attempt Err((
                            Response::error(ErrorCode::Invalid, "agent is not running"),
                            gone,
                        ));
                    }
                    // A session's return may have folded the resolved record
                    // into the record its session ended with since (during
                    // `localise`, or between attempts before any place was
                    // taken): a lease granted to the retired id would be one its
                    // session's releases never find. Claim as the record that
                    // stays.
                    if let Some(current) = state.registry.get(&holder).map(|a| a.id.clone())
                        && current != holder
                    {
                        holder = current;
                        waiting.follow(holder.clone());
                    }
                    if let Some(error) = state.write_failure() {
                        break 'attempt Err((error, gone));
                    }
                    // A waiter queued before the pause is held too: a release
                    // during the pause grants nothing to the project's agents.
                    if let Some(pause) = state.pause_holding(&holder) {
                        break 'attempt Err((pause_refusal(pause), gone));
                    }
                    let now = Utc::now();
                    state.expire_leases_at(now);
                    if let Some(error) = state.write_failure() {
                        break 'attempt Err((error, gone));
                    }
                    // Fairness: a waiter with somebody ahead of it on an
                    // overlapping resource does not try, so a newcomer
                    // cannot take what somebody has been waiting minutes
                    // for. A first attempt has no ticket and always tries.
                    // A quota is a number, not a place: several agents hold
                    // one at once, and what decides is whether the sum fits.
                    // Checked here, under the same lock as the table, so two
                    // claims cannot both see room for the last of it.
                    if resource.kind() == "quota"
                        && let Some(capacity) = state.quota_capacity(&holder, resource.value())
                    {
                        let want = amount.unwrap_or(1);
                        let committed = state.leases.committed(&resource);
                        if committed.saturating_add(want) > capacity {
                            state.touch(&holder);
                            if let Some(error) = state.write_failure() {
                                break 'attempt Err((error, gone));
                            }
                            break 'attempt Err((
                                Response::Error {
                                    code: ErrorCode::Conflict,
                                    message: format!(
                                        "{resource} has {} of {capacity} left and {want} was asked for",
                                        capacity.saturating_sub(committed)
                                    ),
                                    details: Some(json!({
                                        "quota": resource.value(),
                                        "capacity": capacity,
                                        "committed": committed,
                                        "requested": want,
                                    })),
                                },
                                gone,
                            ));
                        }
                    }
                    let result = if state.may_attempt(waiting.ticket()) {
                        state.leases.clone().claim_as(
                            resource.clone(),
                            holder.clone(),
                            // A quota is shared by construction: it is spent,
                            // not occupied, so exclusivity would make every
                            // budget a lock on itself.
                            if resource.kind() == "quota" {
                                LeaseMode::Shared
                            } else {
                                mode
                            },
                            ttl(ttl_secs),
                            note.clone(),
                            now,
                            automatic,
                        )
                    } else {
                        Err(LeaseError::Conflict {
                            resource: resource.clone(),
                            held_by: state
                                .leases
                                .holders_of(&resource)
                                .into_iter()
                                .cloned()
                                .collect(),
                        })
                    };
                    let (message, held_by) = match result {
                        Ok(Claimed::New(mut lease)) => {
                            lease.change_seq = state.store_read("lease ledger boundary", |store| {
                                store.change_watermark()
                            });
                            if lease.resource.kind() == "quota" {
                                lease.amount = amount.unwrap_or(1);
                            }
                            if !state.commit_lease_activity(
                                &holder,
                                Some(&lease),
                                Some(EventKind::LeaseClaimed {
                                    lease: lease.clone(),
                                }),
                                now,
                            ) {
                                let error = state
                                    .write_failure()
                                    .expect("failed lease commit freezes storage");
                                break 'attempt Err((error, gone));
                            }
                            break 'attempt Err((Response::Lease { lease }, WaitOutcome::Claimed));
                        }
                        Ok(Claimed::Renewed(lease)) => {
                            if !state.commit_lease_activity(
                                &holder,
                                Some(&lease),
                                Some(EventKind::LeaseRenewed {
                                    lease: lease.clone(),
                                }),
                                now,
                            ) {
                                let error = state
                                    .write_failure()
                                    .expect("failed lease commit freezes storage");
                                break 'attempt Err((error, gone));
                            }
                            break 'attempt Err((Response::Lease { lease }, WaitOutcome::Claimed));
                        }
                        Err(err) => match err {
                            // Said by whom, by name: the holder is somebody the
                            // person can go and talk to, and an id is not a name.
                            LeaseError::Conflict { held_by, .. } => {
                                let holders = held_by
                                    .iter()
                                    .map(|l| {
                                        let name = state
                                            .registry
                                            .get(&l.holder)
                                            .map(|a| a.spec.name.as_str())
                                            .filter(|n| !n.is_empty())
                                            .unwrap_or("an unknown agent");
                                        format!(
                                            "{name} ({}; {} on {})",
                                            l.holder.short(),
                                            l.mode,
                                            l.resource
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                (format!("{resource} is held by {holders}"), held_by)
                            }
                            other => break 'attempt Err((lease_error(other), gone)),
                        },
                    };
                    // One conflict event per request, committed with liveness.
                    let conflict = (!reported_conflict).then(|| EventKind::LeaseConflict {
                        resource: resource.clone(),
                        requester: holder.clone(),
                        held_by: held_by.iter().map(|l| l.holder.clone()).collect(),
                    });
                    if !state.commit_lease_activity(&holder, None, conflict, now) {
                        let error = state
                            .write_failure()
                            .expect("failed conflict commit freezes storage");
                        break 'attempt Err((error, gone));
                    }
                    if !reported_conflict {
                        reported_conflict = true;
                        warn!(agent = %holder.short(), %resource, waiting = wait_secs > 0, "lease conflict");
                    }
                    // Before agreeing to wait: would waiting close a cycle?
                    // If it would, nobody in it could ever proceed, so it is
                    // broken at its victim: the lowest priority, the youngest
                    // among equals, this claim counting as the youngest of
                    // all. An older wait chosen is ended and this claim waits;
                    // chosen itself, it is refused.
                    if wait_secs > 0 && waiting.ticket().is_none() {
                        if let Some(cycle) = state.break_rings(&holder, &resource, priority) {
                            warn!(agent = %holder.short(), %resource, "claim would deadlock");
                            // A wait that never began still ended, and its
                            // outcome is one subscribers are told to expect.
                            waiting.never_started(&mut state, WaitOutcome::Deadlock);
                            break 'attempt Err((
                                Response::Error {
                                    code: ErrorCode::Deadlock,
                                    message: format!(
                                        "waiting for {resource} would deadlock: {}",
                                        describe(&cycle)
                                    ),
                                    details: Some(json!({ "cycle": cycle, "held_by": held_by })),
                                },
                                WaitOutcome::Deadlock,
                            ));
                        }
                        // Joined under the same lock the check ran under.
                        // Letting go in between would let two claims each
                        // see no cycle and then both create one.
                        waiting.join_locked(&mut state, mode, priority);
                    }
                    Ok((message, held_by))
                };
                attempt.map_err(|(response, outcome)| {
                    waiting.end_locked(&mut state, outcome);
                    response
                })
            };
            let (message, held_by) = match attempt {
                Ok(conflict) => conflict,
                Err(answer) => {
                    #[cfg(test)]
                    self.claim_pause(waiting::ClaimPoint::Answered, &holder)
                        .await;
                    return answer;
                }
            };
            if wait_secs == 0 {
                return Response::Error {
                    code: ErrorCode::Conflict,
                    message,
                    details: Some(json!({ "held_by": held_by })),
                };
            }
            // The conflict is recorded; from here this request only waits,
            // and an offer must not wait with it.
            self.done_writing();
            let woke = wait_for_release(&mut events, &resource, deadline).await;
            #[cfg(test)]
            self.claim_pause(waiting::ClaimPoint::Woken, &holder).await;
            // What the wake means is decided in one hold, and a wait it ends
            // gives its place up in that hold: a ring broken at this wait
            // first, whatever else woke it or the deadline says; then the
            // deadline; then a handover that refuses it another attempt.
            let next = {
                let mut state = lock(&self.state);
                if let Some(cycle) = waiting.deadlocked(&mut state) {
                    Err(ended_by_deadlock(&state, &resource, &cycle))
                } else if !woke {
                    waiting.end_locked(&mut state, WaitOutcome::Timeout);
                    Err(Response::Error {
                        code: ErrorCode::Conflict,
                        message,
                        details: Some(json!({ "held_by": held_by })),
                    })
                } else {
                    // Something changed; claiming again is a write, so take
                    // a place back for it, or learn the daemon is handing
                    // over.
                    self.readmit_locked(&mut state).map_err(|refusal| {
                        waiting.end_locked(&mut state, WaitOutcome::Cancelled);
                        *refusal
                    })
                }
            };
            #[cfg(test)]
            self.claim_pause(waiting::ClaimPoint::Decided, &holder)
                .await;
            match next {
                Ok(place) => Self::resume_writing(place),
                Err(answer) => return answer,
            }
        }
    }

    pub(super) async fn leases(&self, agent: Option<&str>, resource: Option<String>) -> Response {
        let holder = match agent.map(|reference| self.resolve(reference)).transpose() {
            Ok(holder) => holder,
            Err(response) => return *response,
        };
        // Normalize query aliases using the same physical identity as claims.
        let mut keys: Vec<ResourceKey> = Vec::new();
        if let Some(resource) = resource {
            let raw = ResourceKey::new(resource);
            let local = match self.localise(raw.clone(), holder.as_ref(), false).await {
                Ok(key) => key,
                Err(response) => return *response,
            };
            if local != raw {
                keys.push(local);
            }
            keys.push(raw);
        }
        let mut state = lock(&self.state);
        state.expire_leases();
        let leases: Vec<Lease> = state
            .leases
            .all()
            .into_iter()
            .filter(|l| holder.as_ref().is_none_or(|h| l.holder == *h))
            .filter(|l| keys.is_empty() || keys.iter().any(|k| l.resource.overlaps(k)))
            .cloned()
            .collect();
        Response::Leases { leases }
    }

    /// Canonical physical identity for write protection. A logical file key
    /// is accepted only with an explicit holder checkout; it is never a second lock domain.
    pub(super) async fn localise(
        &self,
        key: ResourceKey,
        holder: Option<&AgentId>,
        _record: bool,
    ) -> Result<ResourceKey, Box<Response>> {
        let path = if key.kind() == "path" {
            let path = PathBuf::from(key.value());
            if !path.is_absolute() {
                return Err(Box::new(Response::error(
                    ErrorCode::Invalid,
                    "path resources must be absolute",
                )));
            }
            path
        } else if key.kind() == "file" {
            let state = lock(&self.state);
            physical_file(
                &key,
                holder
                    .and_then(|id| state.registry.get(id))
                    .and_then(|a| a.project.as_ref()),
            )
            .ok_or_else(|| {
                Box::new(Response::error(
                    ErrorCode::Invalid,
                    "file resources require a matching agent project and a safe relative path",
                ))
            })?
        } else {
            return Ok(key);
        };
        let path = tokio::task::spawn_blocking(move || project::try_canonical(&path))
            .await
            .map_err(|err| Box::new(Response::error(ErrorCode::Internal, err.to_string())))?
            .map_err(|err| Box::new(Response::error(ErrorCode::Invalid, err.to_string())))?;
        if !path.is_absolute() {
            return Err(Box::new(Response::error(
                ErrorCode::Invalid,
                "physical paths must be absolute",
            )));
        }
        Ok(ResourceKey::new(format!("path:{}", path.display())))
    }
}

/// The answer to a claim whose wait another claim ended to break a ring:
/// `deadlock`, with the ring, as a claim refused before it waited is told
/// it, and who holds what it wanted.
fn ended_by_deadlock(state: &State, resource: &ResourceKey, cycle: &[Blocked]) -> Response {
    let held_by: Vec<Lease> = state
        .leases
        .holders_of(resource)
        .into_iter()
        .cloned()
        .collect();
    Response::Error {
        code: ErrorCode::Deadlock,
        message: format!(
            "the wait for {resource} was ended to break a deadlock: {}",
            describe(cycle)
        ),
        details: Some(json!({ "cycle": cycle, "held_by": held_by })),
    }
}

impl State {
    pub(super) fn renew(&mut self, reference: &str, lease: &LeaseId, ttl_secs: u64) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        if !self
            .registry
            .get(&holder)
            .is_some_and(|a| a.status == AgentStatus::Running)
        {
            return Response::error(ErrorCode::Invalid, "agent is not running");
        }
        let now = Utc::now();
        self.expire_leases_at(now);
        if let Some(error) = self.write_failure() {
            return error;
        }
        // A card's hold renews for its whole four hours unless asked for
        // longer: a plain renew must not shorten it to a file lease's five
        // minutes.
        let card = self
            .leases
            .get(lease)
            .is_some_and(|l| l.resource.as_str().starts_with("task:"));
        let ttl_secs = if card {
            ttl_secs.max(tasks::TASK_LEASE_SECS)
        } else {
            ttl_secs
        };
        let result = self
            .leases
            .clone()
            .renew(lease, &holder, ttl(ttl_secs), now);
        match result {
            Ok(lease) => {
                if !self.commit_lease_activity(
                    &holder,
                    Some(&lease),
                    Some(EventKind::LeaseRenewed {
                        lease: lease.clone(),
                    }),
                    now,
                ) {
                    return self
                        .write_failure()
                        .expect("failed renewal freezes storage");
                }
                Response::Lease { lease }
            }
            Err(err) => lease_error(err),
        }
    }

    pub(super) fn release(
        &mut self,
        reference: &str,
        lease: &LeaseId,
        summary: Option<String>,
        source: SummarySource,
    ) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        match self.leases.clone().release(lease, &holder) {
            Ok(lease) => {
                let mut released = self.finish_release(&holder, vec![lease], summary, source);
                Response::Lease {
                    lease: released.remove(0),
                }
            }
            Err(err) => lease_error(err),
        }
    }

    pub(super) fn release_all(
        &mut self,
        reference: &str,
        summary: Option<String>,
        source: SummarySource,
        only_automatic: bool,
    ) -> Response {
        let holder = match self.resolve(reference) {
            Ok(id) => id,
            Err(response) => return *response,
        };
        let released = self
            .leases
            .by_holder(&holder)
            .into_iter()
            .filter(|l| !only_automatic || l.automatic)
            .cloned()
            .collect();
        let released = self.finish_release(&holder, released, summary, source);
        Response::Leases { leases: released }
    }

    /// Stage deletion and the journal/replay evidence in one transaction,
    /// then remove memory protection and announce. Returns leases for the reply.
    pub(super) fn finish_release(
        &mut self,
        holder: &AgentId,
        released: Vec<Lease>,
        summary: Option<String>,
        source: SummarySource,
    ) -> Vec<Lease> {
        // A summary the agent typed is explicit whatever a client claims;
        // only a quoted transcript is not.
        let source = match source {
            SummarySource::Transcript => SummarySource::Transcript,
            _ => SummarySource::Explicit,
        };
        if released.is_empty() {
            // Nothing was freed, but an explicit summary is still something
            // said: record it rather than drop the agent's text silently. A
            // transcript tail only ever describes leases actually released.
            let explicit = summary
                .filter(|_| source == SummarySource::Explicit)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
            if let (Some(text), Some(record)) = (explicit, self.registry.get(holder).cloned()) {
                if let Some(entry) =
                    self.plain_entry(&record, JournalKind::Release, text, SummarySource::Explicit)
                {
                    self.append_journal(entry);
                }
            }
            return released;
        }
        let previous_seq = self.journal_seq.clone();
        let entry = self
            .release_entry(holder, &released, summary, source)
            .map(|mut entry| {
                entry.seq = self.next_journal_seq(&entry.project);
                entry
            });
        let ids: Vec<LeaseId> = released.iter().map(|l| l.id.clone()).collect();
        let now = Utc::now();
        let mut events: Vec<Event> = released
            .iter()
            .map(|lease| {
                Event::new(
                    EventKind::LeaseReleased {
                        lease: lease.clone(),
                    },
                    now,
                )
            })
            .collect();
        if let Some(entry) = &entry {
            events.push(Event::new(
                EventKind::JournalAppended {
                    entry: entry.clone(),
                },
                now,
            ));
        }
        let committed = self.commit(
            "release",
            events,
            |store, events| store.release_leases(&ids, entry.as_ref(), events),
            |state, _, ()| {
                for lease in &released {
                    state
                        .leases
                        .release(&lease.id, &lease.holder)
                        .expect("release protection retained until commit");
                }
                if let Some(entry) = &entry {
                    state.cache_journal(entry.clone());
                }
            },
        );
        if committed != Persisted::Committed {
            self.journal_seq = previous_seq;
        }
        released
    }

    /// The journal entry for a release: what the ledger saw change under
    /// the released paths while they were held, plus the summary the agent
    /// gave, or one synthesised from those paths. `None` when there is
    /// nothing to say.
    pub(super) fn release_entry(
        &mut self,
        holder: &AgentId,
        released: &[Lease],
        summary: Option<String>,
        source: SummarySource,
    ) -> Option<JournalEntry> {
        let record = self.registry.get(holder)?.clone();
        let project = record.project.clone()?;
        let checkout = project.dir().to_path_buf();
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut range: Option<(u64, u64)> = None;
        let mut head_before: Option<String> = None;
        for lease in released.iter().filter(|l| l.resource.kind() == "path") {
            let physical = Path::new(lease.resource.value());
            let Ok(relative) = physical.strip_prefix(&checkout) else {
                continue;
            };
            let query = ChangesQuery {
                project: project.id(),
                since_seq: lease.change_seq,
                path: Some(relative.to_string_lossy().into_owned()),
                agent: None,
                limit: RELEASE_SCAN,
                after: lease.change_seq.is_none().then_some(lease.acquired_at),
                before_seq: None,
            };
            let Some(changes) = self.store_read("journal", |store| store.changes(&query)) else {
                continue;
            };
            for change in changes
                .into_iter()
                .filter(|c| c.checkout.as_deref().is_none_or(|dir| dir == checkout))
            {
                range = Some(match range {
                    None => (change.seq, change.seq),
                    Some((lo, hi)) => (lo.min(change.seq), hi.max(change.seq)),
                });
                if head_before.is_none() {
                    head_before = change.head.clone();
                }
                paths.push(change.path);
            }
        }
        let (paths, paths_total) = cap_paths(paths);
        let (summary, summary_source) = match summary
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
        {
            Some(text) => (text, source),
            None if paths.is_empty() => return None,
            None => (
                synthesise_summary(&paths, paths_total),
                SummarySource::Synthesised,
            ),
        };
        Some(JournalEntry {
            project: project.id(),
            seq: 0,
            at: Utc::now(),
            agent: Some(holder.clone()),
            agent_name: record.spec.name.clone(),
            branch: record.vcs.as_ref().and_then(|v| v.branch.clone()),
            checkout: Some(checkout),
            worktree: project.worktree.clone(),
            kind: JournalKind::Release,
            summary,
            summary_source,
            resources: released.iter().map(|l| l.resource.clone()).collect(),
            paths,
            paths_total,
            head_before,
            head_after: record.vcs.as_ref().and_then(|v| v.head.clone()),
            changes: range,
        })
    }

    /// A journal entry about an agent that needs no ledger: join, leave,
    /// note, commit.
    pub(super) fn plain_entry(
        &self,
        record: &AgentRecord,
        kind: JournalKind,
        summary: String,
        source: SummarySource,
    ) -> Option<JournalEntry> {
        let project = record.project.as_ref()?;
        Some(JournalEntry {
            project: project.id(),
            seq: 0,
            at: Utc::now(),
            agent: Some(record.id.clone()),
            agent_name: record.spec.name.clone(),
            branch: record.vcs.as_ref().and_then(|v| v.branch.clone()),
            checkout: Some(project.dir().to_path_buf()),
            worktree: project.worktree.clone(),
            kind,
            summary,
            summary_source: source,
            resources: Vec::new(),
            paths: Vec::new(),
            paths_total: 0,
            head_before: None,
            head_after: record.vcs.as_ref().and_then(|v| v.head.clone()),
            changes: None,
        })
    }
}

impl State {
    pub fn expire_leases(&mut self) {
        self.expire_leases_at(Utc::now());
    }

    pub(super) fn persist_lease_removal(&mut self, lease: &Lease, expired: bool) {
        let kind = if expired {
            EventKind::LeaseExpired {
                lease: lease.clone(),
            }
        } else {
            EventKind::LeaseReleased {
                lease: lease.clone(),
            }
        };
        let _ = self.transition(
            "lease removal",
            Event::new(kind, Utc::now()),
            |store, event| store.delete_lease_with_event(&lease.id, event),
            |_| {},
        );
    }

    pub(super) fn expire_leases_at(&mut self, now: DateTime<Utc>) {
        if self.fenced() {
            // Expiry is memory first, then the write; while fenced the
            // write cannot land, so the tick is skipped whole rather than
            // leaving memory without a lease the database still holds.
            return;
        }
        let expired = self.leases.expire(now);
        for lease in expired {
            info!(lease = %lease.id, holder = %lease.holder.short(), resource = %lease.resource, "lease expired");
            self.persist_lease_removal(&lease, true);
        }
    }
}
